use chrono::{Timelike, Utc};
use rookery_core::config::AgentConfig;
use rookery_core::state::{AgentEntry, AgentPersistence, AgentState};
use serde::Serialize;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::integrity::{self, SQLITE3};
use crate::logs::LogBuffer;
use crate::process::is_pid_alive;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub pid: u32,
    pub started_at: chrono::DateTime<Utc>,
    pub status: AgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_restarts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_restart_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifetime_errors: Option<u32>,
}

/// Extended health detail for an agent, including watchdog state.
#[derive(Debug, Clone, Serialize)]
pub struct AgentHealthDetail {
    #[serde(flatten)]
    pub info: AgentInfo,
    pub watchdog: WatchdogState,
    pub dependency_ports: Vec<DependencyPort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_restart_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchdogState {
    /// "idle", "backing_off", "healthy"
    pub state: String,
    /// Current consecutive crash count (0 = no recent crashes)
    pub consecutive_crashes: u32,
    /// Current backoff delay in seconds (0 if not backing off)
    pub backoff_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyPort {
    pub port: u16,
    pub up: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Running,
    Stopped,
    Failed { error: String },
}

struct ManagedAgent {
    child: Option<Child>,
    info: AgentInfo,
    /// Whether this agent was intentionally stopped (not a crash).
    intentional_stop: bool,
    /// Seconds to wait after SIGTERM before SIGKILL, captured from config at
    /// start/adopt time so stop() doesn't need a config lookup.
    stop_timeout_secs: u64,
    // Observability metrics
    total_restarts: u32,
    last_restart_reason: Option<String>,
    last_restart_at: Option<chrono::DateTime<Utc>>,
    /// Shared with stderr capture task — incremented on error lines.
    error_count: Arc<AtomicU32>,
    /// Accumulated errors from previous restarts.
    lifetime_errors: u32,
}

/// Is the tracked process actually alive right now?
///
/// `try_wait()` for children we spawned, `/proc` for adopted PIDs. Every read
/// path must go through this rather than trusting `info.status`: that field is
/// set to `Running` at start/adopt and never mutated, so reading it reports a
/// long-dead agent as Running forever.
///
/// `try_wait()` reaps an exited child, but tokio caches the exit status and
/// returns it from every later `try_wait()`/`wait()`, so repeated calls from
/// several read paths are safe.
fn agent_is_alive(agent: &mut ManagedAgent) -> bool {
    match &mut agent.child {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => is_pid_alive(agent.info.pid),
    }
}

/// Does `/proc/<pid>/cmdline` reference `command`?
///
/// cmdline is NUL-separated; joined with spaces before matching. A substring test
/// is deliberate: an agent is commonly launched through an interpreter or wrapper,
/// so the configured command appears as an argument rather than argv[0] (e.g.
/// `python3 /path/to/agent gateway run`).
fn pid_cmdline_matches(pid: u32, command: &str) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let joined = String::from_utf8_lossy(&raw).replace('\0', " ");
    if joined.trim().is_empty() {
        return false;
    }
    if joined.contains(command) {
        return true;
    }
    // Fall back to the basename, so a config naming a bare command ("hermes")
    // still matches a process launched by absolute path, and vice versa.
    match std::path::Path::new(command)
        .file_name()
        .and_then(|s| s.to_str())
    {
        Some(base) if !base.is_empty() => joined.contains(base),
        _ => false,
    }
}

pub struct AgentManager {
    agents: Mutex<HashMap<String, ManagedAgent>>,
    log_buffer: Arc<LogBuffer>,
    persistence: AgentPersistence,
    /// Tracks consecutive crash count per agent for exponential backoff.
    crash_counts: Mutex<HashMap<String, u32>>,
    /// Fires when a fatal error pattern is detected in agent stderr.
    /// Value is the agent name.
    fatal_error_tx: tokio::sync::watch::Sender<Option<String>>,
    fatal_error_rx: tokio::sync::watch::Receiver<Option<String>>,
    /// Set to true during graceful shutdown — watchdog stops restarting agents.
    shutting_down: std::sync::atomic::AtomicBool,
    /// Notifies the watchdog to wake up immediately (used during shutdown).
    shutdown_notify: tokio::sync::Notify,
    /// Suppresses dependency port bounce logic while the backend is intentionally sleeping.
    dependency_bounce_suppressed: AtomicBool,
    /// Dependency port liveness, shared between watchdog and health queries.
    port_status: Mutex<HashMap<u16, bool>>,
    /// Nightly SQLite integrity results per agent, read by the metrics endpoint.
    db_integrity: Mutex<HashMap<String, DbIntegrity>>,
}

/// What the metrics endpoint needs to know about an agent's database health.
///
/// `last_check_ts` matters as much as `failures`: a check that quietly stopped
/// running looks identical to a clean bill of health, which is the exact failure
/// mode that let the 2026-08-15 corruption sit undetected for weeks.
#[derive(Debug, Clone, Copy, Default)]
pub struct DbIntegrity {
    /// Cumulative databases found corrupt across all sweeps.
    pub failures: u64,
    /// Cumulative databases that could not be checked at all.
    pub unchecked: u64,
    /// Unix timestamp of the last completed sweep, 0 if never.
    pub last_check_ts: i64,
}

impl AgentManager {
    pub fn new(log_buffer: Arc<LogBuffer>) -> Self {
        Self::with_persistence(log_buffer, AgentPersistence::new())
    }

    pub fn with_persistence(log_buffer: Arc<LogBuffer>, persistence: AgentPersistence) -> Self {
        let (fatal_error_tx, fatal_error_rx) = tokio::sync::watch::channel(None);
        Self {
            agents: Mutex::new(HashMap::new()),
            log_buffer,
            persistence,
            fatal_error_tx,
            fatal_error_rx,
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            shutdown_notify: tokio::sync::Notify::new(),
            dependency_bounce_suppressed: AtomicBool::new(false),
            crash_counts: Mutex::new(HashMap::new()),
            port_status: Mutex::new(HashMap::new()),
            db_integrity: Mutex::new(HashMap::new()),
        }
    }

    /// Run `PRAGMA quick_check` over every database belonging to `name`.
    ///
    /// Read-only, in a subprocess, and purely advisory — see
    /// [`crate::integrity`]. Returns the number of corrupt databases found.
    pub async fn check_agent_databases(&self, name: &str, config: &AgentConfig) -> usize {
        let Some(root) = config.data_dir.as_ref().or(config.workdir.as_ref()) else {
            tracing::debug!(
                agent = name,
                "no data_dir or workdir configured — skipping sqlite integrity check"
            );
            return 0;
        };

        let result = integrity::sweep(&self.log_buffer, name, root, SQLITE3).await;

        let mut all = self.db_integrity.lock().await;
        let entry = all.entry(name.to_string()).or_default();
        entry.failures += result.corrupt as u64;
        entry.unchecked += result.unchecked as u64;
        entry.last_check_ts = Utc::now().timestamp();
        result.corrupt
    }

    pub async fn db_integrity(&self, name: &str) -> Option<DbIntegrity> {
        self.db_integrity.lock().await.get(name).copied()
    }

    pub fn set_dependency_bounce_suppressed(&self, suppressed: bool) {
        self.dependency_bounce_suppressed
            .store(suppressed, Ordering::SeqCst);
    }

    pub fn is_dependency_bounce_suppressed(&self) -> bool {
        self.dependency_bounce_suppressed.load(Ordering::SeqCst)
    }

    /// Adopt a previously-running agent by PID (used after daemon restart).
    ///
    /// Returns false if the entry was rejected. Adoption is refused unless the PID
    /// is alive AND `/proc/<pid>/cmdline` still matches the configured command,
    /// because everything downstream kills this PID directly — `stop()` sends
    /// SIGTERM then SIGKILL to the bare number. A stale `agents.json` naming a PID
    /// that has since been recycled would otherwise make the daemon kill an
    /// unrelated process on the next stop, swap, or shutdown.
    pub async fn adopt(
        &self,
        name: &str,
        entry: &AgentEntry,
        config: Option<&AgentConfig>,
    ) -> bool {
        let Some(cfg) = config else {
            tracing::warn!(
                agent = name,
                pid = entry.pid,
                "refusing to adopt agent with no config entry — cannot verify identity, \
                 and it could not be restarted anyway"
            );
            return false;
        };

        if !is_pid_alive(entry.pid) {
            tracing::info!(
                agent = name,
                pid = entry.pid,
                "not adopting: process is gone or a zombie"
            );
            return false;
        }

        if !pid_cmdline_matches(entry.pid, &cfg.command) {
            tracing::warn!(
                agent = name,
                pid = entry.pid,
                expected = %cfg.command,
                "refusing to adopt: /proc cmdline does not match configured command \
                 (PID was almost certainly recycled)"
            );
            return false;
        }

        tracing::info!(agent = name, pid = entry.pid, "adopting existing agent");
        let version = config
            .and_then(|c| c.version_file.as_ref())
            .and_then(|path| read_version_file(path));
        let info = AgentInfo {
            name: name.to_string(),
            pid: entry.pid,
            started_at: entry.started_at,
            status: AgentStatus::Running,
            version,
            uptime_secs: None,
            total_restarts: None,
            last_restart_reason: None,
            error_count: None,
            lifetime_errors: None,
        };
        let mut agents = self.agents.lock().await;
        agents.insert(
            name.to_string(),
            ManagedAgent {
                child: None,
                info,
                intentional_stop: false,
                stop_timeout_secs: config
                    .map(|c| c.stop_timeout_secs)
                    .unwrap_or_else(rookery_core::config::default_stop_timeout_secs),
                total_restarts: 0,
                last_restart_reason: None,
                last_restart_at: None,
                error_count: Arc::new(AtomicU32::new(0)),
                lifetime_errors: 0,
            },
        );
        true
    }

    fn persist_state(&self, agents: &HashMap<String, ManagedAgent>) {
        let state = AgentState {
            agents: agents
                .iter()
                .map(|(name, a)| {
                    (
                        name.clone(),
                        AgentEntry {
                            pid: a.info.pid,
                            started_at: a.info.started_at,
                        },
                    )
                })
                .collect(),
        };
        if let Err(e) = self.persistence.save(&state) {
            tracing::warn!(error = %e, "failed to persist agent state");
        }
    }

    pub async fn start(&self, name: &str, config: &AgentConfig) -> Result<AgentInfo, AgentError> {
        let mut agents = self.agents.lock().await;

        // Check if already running
        if let Some(agent) = agents.get_mut(name) {
            if agent_is_alive(agent) {
                return Err(AgentError::AlreadyRunning(name.to_string()));
            }
            // Exited, clean up
            agents.remove(name);
        }

        tracing::info!(agent = name, command = %config.command, "starting agent");

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(false);

        // Set working directory
        if let Some(workdir) = &config.workdir {
            cmd.current_dir(workdir);
        }

        // Set environment variables
        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| AgentError::SpawnFailed {
            name: name.to_string(),
            error: e.to_string(),
        })?;

        let pid = child.id().ok_or_else(|| AgentError::SpawnFailed {
            name: name.to_string(),
            error: "failed to get PID".into(),
        })?;

        // Shared error counter for stderr capture
        let error_count = Arc::new(AtomicU32::new(0));

        // Capture output into log buffer with agent prefix
        let prefix = format!("[agent:{name}]");
        if let Some(stderr) = child.stderr.take() {
            let buf = self.log_buffer.clone();
            let p = prefix.clone();
            let err_count = error_count.clone();
            let fatal_tx = self.fatal_error_tx.clone();
            let agent_name = name.to_string();
            let fatal_patterns: Vec<String> = config
                .restart_on_error_patterns
                .iter()
                .map(|p| p.to_ascii_lowercase())
                .collect();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let lower = line.to_ascii_lowercase();
                    if lower.contains("error") {
                        err_count.fetch_add(1, Ordering::Relaxed);
                    }
                    if !fatal_patterns.is_empty()
                        && fatal_patterns.iter().any(|pat| lower.contains(pat))
                    {
                        tracing::warn!(
                            agent = %agent_name,
                            line = %line,
                            "fatal error pattern detected, triggering restart"
                        );
                        let _ = fatal_tx.send(Some(agent_name.clone()));
                    }
                    buf.push(format!("{p} {line}"));
                }
            });
        }
        if let Some(stdout) = child.stdout.take() {
            let buf = self.log_buffer.clone();
            let p = prefix;
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    buf.push(format!("{p} {line}"));
                }
            });
        }

        let version = config
            .version_file
            .as_ref()
            .and_then(|path| read_version_file(path));
        let info = AgentInfo {
            name: name.to_string(),
            pid,
            started_at: Utc::now(),
            status: AgentStatus::Running,
            version,
            uptime_secs: None,
            total_restarts: None,
            last_restart_reason: None,
            error_count: None,
            lifetime_errors: None,
        };

        agents.insert(
            name.to_string(),
            ManagedAgent {
                child: Some(child),
                info: info.clone(),
                intentional_stop: false,
                stop_timeout_secs: config.stop_timeout_secs,
                total_restarts: 0,
                last_restart_reason: None,
                last_restart_at: None,
                error_count,
                lifetime_errors: 0,
            },
        );

        self.persist_state(&agents);

        tracing::info!(agent = name, pid, "agent started");
        Ok(info)
    }

    /// Stop an agent at a user's request. Resets the crash-backoff counter,
    /// because a human intervening is a fresh start.
    pub async fn stop(&self, name: &str) -> Result<(), AgentError> {
        self.stop_inner(name, true).await
    }

    /// Stop an agent as part of an automated flow (fatal-error restart, port
    /// bounce, profile swap).
    ///
    /// Identical to `stop` except it PRESERVES the crash-backoff counter. Using
    /// `stop` here silently erased the exponential backoff every time an
    /// automated path ran, so a agent failing on every startup attempt would
    /// restart forever at a fixed interval instead of backing off.
    pub async fn stop_automated(&self, name: &str) -> Result<(), AgentError> {
        self.stop_inner(name, false).await
    }

    async fn stop_inner(&self, name: &str, reset_crash_count: bool) -> Result<(), AgentError> {
        let mut agents = self.agents.lock().await;

        let agent = agents
            .get_mut(name)
            .ok_or_else(|| AgentError::NotFound(name.to_string()))?;

        // Mark as intentional so watchdog doesn't restart it
        agent.intentional_stop = true;

        let pid = agent.info.pid;
        let stop_timeout = std::time::Duration::from_secs(agent.stop_timeout_secs);
        tracing::info!(
            agent = name,
            pid,
            timeout_secs = agent.stop_timeout_secs,
            "stopping agent"
        );

        if let Some(ref mut child) = agent.child {
            // Owned child — SIGTERM then wait
            if let Some(cpid) = child.id() {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(cpid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }

            let wait_result = tokio::time::timeout(stop_timeout, child.wait()).await;

            match wait_result {
                Ok(Ok(status)) => {
                    tracing::info!(agent = name, ?status, "agent exited");
                }
                // Split so a genuine wait() failure isn't reported as a hang.
                Ok(Err(e)) => {
                    tracing::error!(agent = name, error = %e, "wait() on agent failed, killing");
                    let _ = child.kill().await;
                }
                Err(_) => {
                    // Treat as an incident, not routine. An agent hard-killed
                    // mid-checkpoint is how a large SQLite WAL gets torn pages.
                    tracing::error!(
                        agent = name,
                        pid,
                        timeout_secs = agent.stop_timeout_secs,
                        "AGENT DID NOT EXIT WITHIN GRACE PERIOD — sending SIGKILL. \
                         If this agent writes a database, data loss is possible; \
                         raise stop_timeout_secs for it."
                    );
                    let _ = child.kill().await;
                }
            }
        } else {
            // Adopted agent — kill by PID
            tracing::info!(agent = name, pid, "stopping adopted agent by PID");
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );

            // Poll at 500ms up to the configured grace period.
            //
            // Uses is_pid_alive, not /proc/<pid> existence: a process that has exited
            // but not yet been reaped is a zombie, and /proc/<pid> still exists for it.
            // Testing existence therefore waited out the whole grace period and then
            // SIGKILLed a process that was already dead.
            for _ in 0..(agent.stop_timeout_secs * 2) {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if !is_pid_alive(pid) {
                    break;
                }
            }

            if is_pid_alive(pid) {
                tracing::error!(
                    agent = name,
                    pid,
                    timeout_secs = agent.stop_timeout_secs,
                    "ADOPTED AGENT DID NOT EXIT WITHIN GRACE PERIOD — sending SIGKILL. \
                     If this agent writes a database, data loss is possible; \
                     raise stop_timeout_secs for it."
                );
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }

        agents.remove(name);
        self.persist_state(&agents);

        // Only a user-initiated stop clears the backoff. Automated paths must not,
        // or an agent that fails on every start restarts forever at a fixed rate.
        if reset_crash_count {
            self.crash_counts.lock().await.remove(name);
        }

        Ok(())
    }

    /// Remove an agent from tracking without sending any signals.
    /// Used when the agent will be restarted with --replace, which
    /// handles killing the old process via its own PID file.
    pub async fn remove_tracking(&self, name: &str) {
        let mut agents = self.agents.lock().await;
        agents.remove(name);
        self.persist_state(&agents);
    }

    /// Returns a reference to the shutdown flag for passing to canary/other tasks.
    pub fn shutdown_flag(&self) -> &std::sync::atomic::AtomicBool {
        &self.shutting_down
    }

    /// Wait for shutdown notification. Returns immediately if already shutting down.
    pub async fn shutdown_notified(&self) {
        if self.is_shutting_down() {
            return;
        }
        self.shutdown_notify.notified().await;
    }

    /// Returns true if shutdown is in progress.
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Signal that the daemon is shutting down — watchdog will stop restarting agents.
    pub fn begin_shutdown(&self) {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
    }

    pub async fn stop_all(&self) {
        self.begin_shutdown();
        let names: Vec<String> = {
            let agents = self.agents.lock().await;
            agents.keys().cloned().collect()
        };

        for name in names {
            if let Err(e) = self.stop(&name).await {
                tracing::warn!(agent = %name, error = %e, "failed to stop agent");
            }
        }
    }

    pub async fn list(&self) -> Vec<AgentInfo> {
        let mut agents = self.agents.lock().await;
        let mut result = Vec::new();

        // Check each agent's actual status but do NOT remove dead agents —
        // the watchdog is responsible for detecting dead agents and restarting them.
        // Removing them here races with the watchdog and prevents crash recovery.
        for agent in agents.values_mut() {
            if agent_is_alive(agent) {
                result.push(agent.info.clone());
            } else {
                let mut info = agent.info.clone();
                info.status = AgentStatus::Stopped;
                result.push(info);
            }
        }

        result
    }

    /// Get health/metrics for a specific agent.
    pub async fn get_health(&self, name: &str) -> Option<AgentInfo> {
        let mut agents = self.agents.lock().await;
        let agent = agents.get_mut(name)?;

        // Ask the OS, don't trust `info.status` — it is stuck on Running for the
        // lifetime of the entry, and the watchdog only evicts dead agents that have
        // restart_on_crash set. Reading the field made rookery_agent_up a constant 1.
        let alive = agent_is_alive(agent);
        let status = if alive {
            AgentStatus::Running
        } else {
            AgentStatus::Stopped
        };

        let uptime_secs = alive.then(|| {
            Utc::now()
                .signed_duration_since(agent.info.started_at)
                .num_seconds()
        });

        Some(AgentInfo {
            name: agent.info.name.clone(),
            pid: agent.info.pid,
            started_at: agent.info.started_at,
            status,
            version: agent.info.version.clone(),
            uptime_secs,
            total_restarts: Some(agent.total_restarts),
            last_restart_reason: agent.last_restart_reason.clone(),
            error_count: Some(agent.error_count.load(Ordering::Relaxed)),
            lifetime_errors: Some(
                agent.lifetime_errors + agent.error_count.load(Ordering::Relaxed),
            ),
        })
    }

    /// Get extended health detail including watchdog state for a specific agent.
    pub async fn get_health_detail(
        &self,
        name: &str,
        config: Option<&AgentConfig>,
    ) -> Option<AgentHealthDetail> {
        let info = self.get_health(name).await?;

        let crash_count = {
            let counts = self.crash_counts.lock().await;
            counts.get(name).copied().unwrap_or(0)
        };

        let backoff_secs = if crash_count > 0 {
            (1u64 << (crash_count - 1).min(6)).min(60)
        } else {
            0
        };

        let watchdog_state = if crash_count > 0 {
            "backing_off".to_string()
        } else if info.uptime_secs.unwrap_or(0) > 300 {
            "healthy".to_string()
        } else {
            "idle".to_string()
        };

        let dependency_ports = if let Some(cfg) = config {
            if let Some(port) = cfg.depends_on_port {
                let port_up = {
                    let ports = self.port_status.lock().await;
                    ports.get(&port).copied().unwrap_or(true)
                };
                vec![DependencyPort { port, up: port_up }]
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        let last_restart_at = {
            let agents = self.agents.lock().await;
            agents.get(name).and_then(|a| a.last_restart_at)
        };

        Some(AgentHealthDetail {
            info,
            watchdog: WatchdogState {
                state: watchdog_state,
                consecutive_crashes: crash_count,
                backoff_secs,
            },
            dependency_ports,
            last_restart_at,
        })
    }

    /// Record restart metrics on a newly-started agent.
    pub async fn record_restart(
        &self,
        name: &str,
        reason: &str,
        prev_restarts: u32,
        prev_errors: u32,
    ) {
        let mut agents = self.agents.lock().await;
        if let Some(agent) = agents.get_mut(name) {
            agent.total_restarts = prev_restarts + 1;
            agent.lifetime_errors = prev_errors;
            agent.last_restart_reason = Some(reason.to_string());
            agent.last_restart_at = Some(Utc::now());
        }
    }

    pub async fn is_running(&self, name: &str) -> bool {
        let mut agents = self.agents.lock().await;
        match agents.get_mut(name) {
            Some(agent) => agent_is_alive(agent),
            None => false,
        }
    }

    /// Spawn a background watchdog task that checks agent liveness and
    /// auto-restarts agents with `restart_on_crash = true`.
    ///
    /// The watchdog polls every 30 seconds. On crash detection it uses
    /// exponential backoff: 1s, 2s, 4s, 8s, … up to 60s cap. The backoff
    /// resets after 5 minutes of successful uptime.
    pub fn spawn_watchdog(
        self: &Arc<Self>,
        configs: HashMap<String, AgentConfig>,
    ) -> tokio::task::JoinHandle<()> {
        let manager = Arc::clone(self);
        let configs = Arc::new(configs);

        tokio::spawn(async move {
            const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
            const MAX_BACKOFF_SECS: u64 = 60;
            const HEALTHY_RESET_SECS: u64 = 300; // reset backoff after 5min uptime
            const BOUNCE_MIN_UPTIME_SECS: i64 = 60; // skip freshly-started agents
            const INTEGRITY_HOUR: u32 = 4; // local-time hour for the nightly sqlite sweep

            let mut fatal_rx = manager.fatal_error_rx.clone();

            // Date of the last sqlite integrity sweep, so it runs once per local
            // day. Starts as None, so a daemon that comes up after INTEGRITY_HOUR
            // sweeps on its first poll rather than skipping a day — the sweep is
            // read-only and measured at ~2s for a 392 MB file, so an extra one
            // after a restart is cheaper than a missed one.
            let mut last_integrity_sweep: Option<chrono::NaiveDate> = None;

            // Track dependency port liveness for down→up transition detection.
            // Initialized to true so a cold start doesn't trigger a false bounce.
            let tracked_ports: std::collections::HashSet<u16> =
                configs.values().filter_map(|c| c.depends_on_port).collect();
            {
                let mut port_status = manager.port_status.lock().await;
                for &p in &tracked_ports {
                    port_status.entry(p).or_insert(true);
                }
            }

            loop {
                // Check shutdown flag before doing anything
                if manager
                    .shutting_down
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    tracing::info!("watchdog: shutdown flag set, exiting");
                    return;
                }

                // Wait for poll interval, fatal error, or shutdown signal
                tokio::select! {
                    _ = tokio::time::sleep(POLL_INTERVAL) => {}
                    _ = manager.shutdown_notify.notified() => {
                        tracing::info!("watchdog: shutdown notification received, exiting");
                        return;
                    }
                    _ = fatal_rx.changed() => {
                        if manager.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
                            tracing::info!("watchdog: shutdown flag set, exiting");
                            return;
                        }
                        // Fatal error pattern detected — restart the agent immediately
                        let triggered = fatal_rx.borrow_and_update().clone();
                        if let Some(agent_name) = triggered {
                            if manager.is_shutting_down() { return; }
                            tracing::warn!(agent = %agent_name, "fatal error pattern triggered immediate restart");
                            if let Some(cfg) = configs.get(&agent_name) {
                                let prev = {
                                    let agents = manager.agents.lock().await;
                                    agents.get(&agent_name).map(|a| (a.total_restarts, a.lifetime_errors + a.error_count.load(Ordering::Relaxed))).unwrap_or((0, 0))
                                };
                                let _ = manager.stop_automated(&agent_name).await;
                                if manager.is_shutting_down() { return; }
                                // Share the crash path's exponential backoff. Previously this
                                // slept a flat 2s with no counter, so an agent emitting a
                                // matching pattern on every startup (a wedged network, say)
                                // restarted every ~2.3s indefinitely — a full agent boot and
                                // a large SQLite open each time. systemd's StartLimit does not
                                // cover this, because rookeryd itself never exits.
                                let crash_count = {
                                    let mut counts = manager.crash_counts.lock().await;
                                    let c = counts.entry(agent_name.clone()).or_insert(0);
                                    *c += 1;
                                    *c
                                };
                                let backoff = (1u64 << (crash_count - 1).min(6)).min(MAX_BACKOFF_SECS);
                                tracing::info!(agent = %agent_name, crash_count, backoff_secs = backoff, "backing off before pattern restart");
                                tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                                if manager.is_shutting_down() { return; }
                                match manager.start(&agent_name, cfg).await {
                                    Ok(info) => {
                                        manager.record_restart(&agent_name, "error_pattern", prev.0, prev.1).await;
                                        tracing::info!(agent = %agent_name, pid = info.pid, "agent restarted after fatal error pattern");
                                    }
                                    Err(e) => tracing::error!(agent = %agent_name, error = %e, "failed to restart after fatal error pattern"),
                                }
                            }
                        }
                        // Drain anything that arrived during the ~2.2s stop/sleep/start
                        // above. One traceback matches the patterns on several lines, and
                        // lines drained off the dying process's pipe land here too — without
                        // this, changed() is already Ready on re-entry and we immediately
                        // SIGTERM the replacement we just spawned, microseconds old and
                        // mid-SQLite-open. Observed 32 times in production since April.
                        fatal_rx.borrow_and_update();
                        continue;
                    }
                }

                // Nightly SQLite integrity sweep, once per local day at INTEGRITY_HOUR.
                //
                // Spawned rather than awaited inline: a sweep is bounded but not
                // instant, and the watchdog is the only thing noticing crashes —
                // it must not go deaf for the duration. Results still reach
                // tracing and the agent log buffer from inside the task, so
                // nothing dies silently in the background.
                {
                    let now = chrono::Local::now();
                    let today = now.date_naive();
                    if now.hour() >= INTEGRITY_HOUR && last_integrity_sweep != Some(today) {
                        last_integrity_sweep = Some(today);
                        let manager = Arc::clone(&manager);
                        let configs = Arc::clone(&configs);
                        tokio::spawn(async move {
                            for (name, cfg) in configs.iter() {
                                if manager.is_shutting_down() {
                                    return;
                                }
                                manager.check_agent_databases(name, cfg).await;
                            }
                        });
                    }
                }

                // Check dependency ports for down→up transitions (server restarted).
                // Agents holding stale connections need to be bounced.
                if !tracked_ports.is_empty() && !manager.is_dependency_bounce_suppressed() {
                    let mut ports_recovered: Vec<u16> = Vec::new();

                    for &port in &tracked_ports {
                        let is_up =
                            crate::health::check_health(port, std::time::Duration::from_secs(3))
                                .await;
                        let was_up = {
                            let ps = manager.port_status.lock().await;
                            ps.get(&port).copied().unwrap_or(true)
                        };

                        if is_up && !was_up {
                            tracing::info!(
                                port,
                                "dependency port recovered, will bounce dependent agents"
                            );
                            ports_recovered.push(port);
                        }

                        if is_up != was_up {
                            if !is_up {
                                tracing::warn!(port, "dependency port is down");
                            }
                            manager.port_status.lock().await.insert(port, is_up);
                        }
                    }

                    // Bounce running agents whose dependency port just recovered
                    if !ports_recovered.is_empty() {
                        let bounce_info: Vec<(String, u32, u32)> = {
                            let agents = manager.agents.lock().await;
                            agents
                                .iter()
                                .filter(|(name, agent)| {
                                    if let Some(cfg) = configs.get(*name)
                                        && let Some(dep_port) = cfg.depends_on_port
                                        && ports_recovered.contains(&dep_port)
                                    {
                                        let uptime = Utc::now()
                                            .signed_duration_since(agent.info.started_at)
                                            .num_seconds();
                                        return uptime > BOUNCE_MIN_UPTIME_SECS
                                            && !agent.intentional_stop;
                                    }
                                    false
                                })
                                .map(|(name, agent)| {
                                    (
                                        name.clone(),
                                        agent.total_restarts,
                                        agent.lifetime_errors
                                            + agent.error_count.load(Ordering::Relaxed),
                                    )
                                })
                                .collect()
                        };

                        for (name, prev_restarts, prev_errors) in bounce_info {
                            if manager.is_shutting_down() {
                                return;
                            }
                            if let Some(cfg) = configs.get(&name) {
                                tracing::info!(
                                    agent = %name,
                                    "bouncing agent after dependency port recovered"
                                );
                                let _ = manager.stop_automated(&name).await;
                                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                                if manager.is_shutting_down() {
                                    return;
                                }
                                match manager.start(&name, cfg).await {
                                    Ok(info) => {
                                        manager
                                            .record_restart(
                                                &name,
                                                "port_recovery",
                                                prev_restarts,
                                                prev_errors,
                                            )
                                            .await;
                                        tracing::info!(
                                            agent = %name,
                                            pid = info.pid,
                                            "agent bounced after port recovery"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            agent = %name,
                                            error = %e,
                                            "failed to bounce agent after port recovery"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // Collect dead agents that need restarting
                let to_restart: Vec<(String, u32, u32)> = {
                    let mut agents = manager.agents.lock().await;
                    let mut dead_entries = Vec::new();

                    for (name, agent) in agents.iter_mut() {
                        if !agent_is_alive(agent)
                            && !agent.intentional_stop
                            && !manager
                                .shutting_down
                                .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            // Check if this agent has restart_on_crash
                            if let Some(cfg) = configs.get(name)
                                && cfg.restart_on_crash
                            {
                                tracing::warn!(
                                    agent = %name,
                                    pid = agent.info.pid,
                                    "agent exited unexpectedly, scheduling restart"
                                );
                                let prev_errors = agent.lifetime_errors
                                    + agent.error_count.load(Ordering::Relaxed);
                                dead_entries.push((
                                    name.clone(),
                                    agent.total_restarts,
                                    prev_errors,
                                ));
                            }
                        }
                    }

                    // Remove dead agents from tracking
                    let dead_names: Vec<String> =
                        dead_entries.iter().map(|(n, _, _)| n.clone()).collect();
                    for name in &dead_names {
                        agents.remove(name);
                    }
                    if !dead_names.is_empty() {
                        manager.persist_state(&agents);
                    }

                    dead_entries
                };

                // Also check for healthy agents and reset their backoff
                {
                    let agents = manager.agents.lock().await;
                    let mut crash_counts = manager.crash_counts.lock().await;
                    for (name, agent) in agents.iter() {
                        let alive = is_pid_alive(agent.info.pid);
                        if alive {
                            let uptime = Utc::now()
                                .signed_duration_since(agent.info.started_at)
                                .num_seconds();
                            if uptime > HEALTHY_RESET_SECS as i64 && crash_counts.contains_key(name)
                            {
                                tracing::info!(
                                    agent = %name,
                                    uptime_secs = uptime,
                                    "agent healthy, resetting crash backoff"
                                );
                                crash_counts.remove(name);
                            }
                        }
                    }
                }

                // Restart each dead agent with backoff
                for (name, prev_restarts, prev_errors) in to_restart {
                    if manager
                        .shutting_down
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        tracing::info!("watchdog: shutdown flag set, skipping restarts");
                        return;
                    }
                    let crash_count = {
                        let mut counts = manager.crash_counts.lock().await;
                        let count = counts.entry(name.clone()).or_insert(0);
                        *count += 1;
                        *count
                    };

                    // Exponential backoff: 1s, 2s, 4s, 8s, ... capped at 60s
                    let backoff_secs = (1u64 << (crash_count - 1).min(6)).min(MAX_BACKOFF_SECS);

                    tracing::info!(
                        agent = %name,
                        crash_count,
                        backoff_secs,
                        "waiting before restart"
                    );

                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    if manager.is_shutting_down() {
                        return;
                    }

                    if let Some(cfg) = configs.get(&name) {
                        match manager.start(&name, cfg).await {
                            Ok(info) => {
                                manager
                                    .record_restart(&name, "crash", prev_restarts, prev_errors)
                                    .await;
                                tracing::info!(
                                    agent = %name,
                                    pid = info.pid,
                                    crash_count,
                                    "agent restarted by watchdog"
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    agent = %name,
                                    error = %e,
                                    crash_count,
                                    "watchdog failed to restart agent"
                                );
                            }
                        }
                    }
                }
            }
        })
    }
}

/// Read a version string from a pyproject.toml or Cargo.toml file.
pub fn read_version_file(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    // Parse as TOML and look for version in common locations
    let table: toml::Table = content.parse().ok()?;
    // pyproject.toml: [project].version or [tool.poetry].version
    if let Some(ver) = table
        .get("project")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
    {
        return Some(ver.to_string());
    }
    if let Some(ver) = table
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
    {
        return Some(ver.to_string());
    }
    // Cargo.toml: [package].version
    if let Some(ver) = table
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
    {
        return Some(ver.to_string());
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent '{0}' is already running")]
    AlreadyRunning(String),

    #[error("agent '{0}' not found")]
    NotFound(String),

    #[error("failed to spawn agent '{name}': {error}")]
    SpawnFailed { name: String, error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create an AgentManager with temp-dir-backed persistence.
    /// Returns (TempDir, AgentManager) — keep TempDir alive for the test duration.
    fn test_manager(log_buffer: Arc<LogBuffer>) -> (tempfile::TempDir, AgentManager) {
        let dir = tempfile::tempdir().unwrap();
        let persistence = rookery_core::state::AgentPersistence {
            path: dir.path().join("agents.json"),
        };
        (dir, AgentManager::with_persistence(log_buffer, persistence))
    }

    /// Same as test_manager but returns Arc<AgentManager>.
    fn test_manager_arc(log_buffer: Arc<LogBuffer>) -> (tempfile::TempDir, Arc<AgentManager>) {
        let (dir, mgr) = test_manager(log_buffer);
        (dir, Arc::new(mgr))
    }

    #[tokio::test]
    async fn test_adopt_refuses_mismatched_cmdline() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_d, manager) = test_manager(log_buffer);

        // A live PID that is NOT the configured command — the PID-reuse case.
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let entry = AgentEntry {
            pid,
            started_at: Utc::now(),
        };

        // Wait for exec, so this asserts a genuine mismatch rather than an
        // as-yet-unpopulated cmdline.
        wait_for_cmdline(pid, "sleep").await;

        let cfg = config_for("/usr/bin/definitely-not-sleep");
        assert!(
            !manager.adopt("agent", &entry, Some(&cfg)).await,
            "must refuse to adopt a PID whose cmdline does not match"
        );
        assert!(!manager.is_running("agent").await);
    }

    #[tokio::test]
    async fn test_adopt_refuses_dead_pid() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_d, manager) = test_manager(log_buffer);

        // Reap a process so the PID is genuinely gone, not a zombie.
        let mut child = tokio::process::Command::new("true").spawn().unwrap();
        let pid = child.id().unwrap();
        let _ = child.wait().await;

        let entry = AgentEntry {
            pid,
            started_at: Utc::now(),
        };
        let cfg = config_for("true");
        assert!(
            !manager.adopt("agent", &entry, Some(&cfg)).await,
            "must refuse to adopt a dead PID"
        );
    }

    #[tokio::test]
    async fn test_adopt_refuses_without_config() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_d, manager) = test_manager(log_buffer);

        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let entry = AgentEntry {
            pid: child.id().unwrap(),
            started_at: Utc::now(),
        };

        assert!(
            !manager.adopt("agent", &entry, None).await,
            "no config means identity is unverifiable and restart is impossible"
        );
    }

    /// Wait until `/proc/<pid>/cmdline` reports `expect`.
    ///
    /// spawn() returns before the child has exec'd, and a forked-but-not-yet-exec'd
    /// child still shows the PARENT's cmdline — here, the test binary's path. So
    /// "wait until non-empty" is not enough: it is satisfied immediately by the
    /// pre-exec parent cmdline, and adoption then correctly refuses because that
    /// cmdline does not name the expected command. Poll for the actual match.
    ///
    /// Real adoption happens after a daemon restart, long after the agent exec'd,
    /// so waiting here reflects production rather than papering over a race. It also
    /// stops the "refuses mismatched cmdline" test passing for the wrong reason.
    async fn wait_for_cmdline(pid: u32, expect: &str) {
        let ready = poll_until(
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(10),
            || pid_cmdline_matches(pid, expect),
        )
        .await;
        assert!(ready, "child never exec'd into {expect:?} (pid {pid})");
    }

    /// Minimal config naming `command`. Adoption now verifies identity against
    /// /proc/<pid>/cmdline, so adopt tests must supply the command they spawned.
    fn config_for(command: &str) -> AgentConfig {
        AgentConfig {
            command: command.to_string(),
            args: vec![],
            workdir: None,
            env: HashMap::new(),
            auto_start: false,
            restart_on_swap: false,
            restart_on_crash: false,
            depends_on_port: None,
            stop_timeout_secs: 30,
            version_file: None,
            update_command: None,
            update_workdir: None,
            restart_on_error_patterns: vec![],
            data_dir: None,
        }
    }

    #[test]
    fn test_read_version_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[project]
name = "test-agent"
version = "1.2.3"
"#,
        )
        .unwrap();
        assert_eq!(read_version_file(&path), Some("1.2.3".to_string()));
    }

    #[test]
    fn test_read_version_poetry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[tool.poetry]
name = "test-agent"
version = "0.4.0"
"#,
        )
        .unwrap();
        assert_eq!(read_version_file(&path), Some("0.4.0".to_string()));
    }

    #[test]
    fn test_read_version_cargo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        std::fs::write(
            &path,
            r#"
[package]
name = "rookery"
version = "0.1.0"
"#,
        )
        .unwrap();
        assert_eq!(read_version_file(&path), Some("0.1.0".to_string()));
    }

    #[test]
    fn test_read_version_missing_file() {
        let path = std::path::PathBuf::from("/tmp/nonexistent_version_file.toml");
        assert_eq!(read_version_file(&path), None);
    }

    #[test]
    fn test_read_version_no_version_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pyproject.toml");
        std::fs::write(
            &path,
            r#"
[project]
name = "test-agent"
"#,
        )
        .unwrap();
        assert_eq!(read_version_file(&path), None);
    }

    #[tokio::test]
    async fn test_agent_manager_start_stop() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        let config = AgentConfig {
            command: "sleep".to_string(),
            args: vec!["60".to_string()],
            workdir: None,
            env: HashMap::new(),
            auto_start: false,
            restart_on_swap: false,
            restart_on_crash: false,
            depends_on_port: None,
            stop_timeout_secs: 30,
            version_file: None,
            update_command: None,
            update_workdir: None,
            restart_on_error_patterns: vec![],
            data_dir: None,
        };

        // Start
        let info = manager.start("test", &config).await.unwrap();
        assert_eq!(info.name, "test");
        assert!(info.pid > 0);
        assert!(manager.is_running("test").await);

        // Stop
        manager.stop("test").await.unwrap();
        assert!(!manager.is_running("test").await);
    }

    #[tokio::test]
    async fn test_agent_manager_already_running() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        let config = AgentConfig {
            command: "sleep".to_string(),
            args: vec!["60".to_string()],
            workdir: None,
            env: HashMap::new(),
            auto_start: false,
            restart_on_swap: false,
            restart_on_crash: false,
            depends_on_port: None,
            stop_timeout_secs: 30,
            version_file: None,
            update_command: None,
            update_workdir: None,
            restart_on_error_patterns: vec![],
            data_dir: None,
        };

        manager.start("test", &config).await.unwrap();
        let err = manager.start("test", &config).await.unwrap_err();
        assert!(matches!(err, AgentError::AlreadyRunning(_)));

        manager.stop("test").await.unwrap();
    }

    #[tokio::test]
    async fn test_agent_manager_get_health() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        let config = AgentConfig {
            command: "sleep".to_string(),
            args: vec!["60".to_string()],
            workdir: None,
            env: HashMap::new(),
            auto_start: false,
            restart_on_swap: false,
            restart_on_crash: false,
            depends_on_port: None,
            stop_timeout_secs: 30,
            version_file: None,
            update_command: None,
            update_workdir: None,
            restart_on_error_patterns: vec![],
            data_dir: None,
        };

        manager.start("test", &config).await.unwrap();

        let health = manager.get_health("test").await.unwrap();
        assert_eq!(health.name, "test");
        assert_eq!(health.status, AgentStatus::Running);
        assert!(health.uptime_secs.unwrap() >= 0);
        assert_eq!(health.total_restarts, Some(0));
        assert_eq!(health.error_count, Some(0));
        assert_eq!(health.lifetime_errors, Some(0));

        // Nonexistent agent
        assert!(manager.get_health("nope").await.is_none());

        manager.stop("test").await.unwrap();
    }

    /// get_health must consult the OS, not the never-mutated `info.status`.
    /// Before the fix this reported Running with a growing uptime forever, which
    /// pinned the rookery_agent_up gauge at 1 for a dead process.
    #[tokio::test]
    async fn test_agent_get_health_reports_dead_agent_as_stopped() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        // restart_on_crash is false, so the watchdog would never evict this entry.
        let config = AgentConfig {
            command: "bash".to_string(),
            args: vec!["-c".to_string(), "exit 0".to_string()],
            ..test_agent_config()
        };
        manager.start("crasher", &config).await.unwrap();

        // Poll the real condition — the process being gone, as reported by
        // try_wait — not a proxy like a log line or a file appearing.
        let exited = poll_until_async(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(20),
            || async { !manager.is_running("crasher").await },
        )
        .await;
        assert!(exited, "crasher agent should have exited");

        // is_running above already reaped the child; get_health must still see it
        // as dead rather than being confused by the cached exit status.
        let health = manager.get_health("crasher").await.unwrap();
        assert_eq!(
            health.status,
            AgentStatus::Stopped,
            "get_health must report a dead process as Stopped"
        );
        assert_eq!(health.uptime_secs, None, "a dead agent has no uptime");

        // get_health_detail inherits the fix, so it cannot report phantom health.
        let detail = manager
            .get_health_detail("crasher", Some(&config))
            .await
            .unwrap();
        assert_eq!(detail.info.status, AgentStatus::Stopped);
    }

    #[tokio::test]
    async fn test_agent_manager_remove_tracking() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        let config = AgentConfig {
            command: "sleep".to_string(),
            args: vec!["60".to_string()],
            workdir: None,
            env: HashMap::new(),
            auto_start: false,
            restart_on_swap: false,
            restart_on_crash: false,
            depends_on_port: None,
            stop_timeout_secs: 30,
            version_file: None,
            update_command: None,
            update_workdir: None,
            restart_on_error_patterns: vec![],
            data_dir: None,
        };

        let info = manager.start("test", &config).await.unwrap();
        let pid = info.pid;

        // Remove tracking — process still runs but manager forgets it
        manager.remove_tracking("test").await;
        assert!(!manager.is_running("test").await);

        // Process is still alive
        assert!(crate::process::is_pid_alive(pid));

        // Clean up
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }

    #[tokio::test]
    async fn test_agent_manager_record_restart() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        let config = AgentConfig {
            command: "sleep".to_string(),
            args: vec!["60".to_string()],
            workdir: None,
            env: HashMap::new(),
            auto_start: false,
            restart_on_swap: false,
            restart_on_crash: false,
            depends_on_port: None,
            stop_timeout_secs: 30,
            version_file: None,
            update_command: None,
            update_workdir: None,
            restart_on_error_patterns: vec![],
            data_dir: None,
        };

        manager.start("test", &config).await.unwrap();
        manager.record_restart("test", "crash", 2, 5).await;

        let health = manager.get_health("test").await.unwrap();
        assert_eq!(health.total_restarts, Some(3));
        assert_eq!(health.last_restart_reason, Some("crash".to_string()));
        assert_eq!(health.lifetime_errors, Some(5)); // prev 5 + current 0

        manager.stop("test").await.unwrap();
    }

    #[tokio::test]
    async fn test_agent_fatal_error_pattern_detection() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        let config = AgentConfig {
            command: "bash".to_string(),
            args: vec![
                "-c".to_string(),
                "echo 'telegram.error.TimedOut: connection lost' >&2; sleep 60".to_string(),
            ],
            workdir: None,
            env: HashMap::new(),
            auto_start: false,
            restart_on_swap: false,
            restart_on_crash: false,
            depends_on_port: None,
            stop_timeout_secs: 30,
            version_file: None,
            update_command: None,
            update_workdir: None,
            restart_on_error_patterns: vec!["telegram.error.TimedOut".to_string()],
            data_dir: None,
        };

        manager.start("test", &config).await.unwrap();

        // Wait for stderr to be read and fatal pattern to fire
        let mut rx = manager.fatal_error_rx.clone();
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.changed())
            .await
            .expect("fatal error should trigger within 3s")
            .expect("watch channel should not be closed");

        let triggered = rx.borrow().clone();
        assert_eq!(triggered, Some("test".to_string()));

        manager.stop("test").await.unwrap();
    }

    #[tokio::test]
    async fn test_agent_no_false_fatal_trigger() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        let config = AgentConfig {
            command: "bash".to_string(),
            args: vec![
                "-c".to_string(),
                "echo 'normal warning message' >&2; sleep 60".to_string(),
            ],
            workdir: None,
            env: HashMap::new(),
            auto_start: false,
            restart_on_swap: false,
            restart_on_crash: false,
            depends_on_port: None,
            stop_timeout_secs: 30,
            version_file: None,
            update_command: None,
            update_workdir: None,
            restart_on_error_patterns: vec!["telegram.error.TimedOut".to_string()],
            data_dir: None,
        };

        manager.start("test", &config).await.unwrap();

        // Should NOT trigger within 2s since the pattern doesn't match
        let mut rx = manager.fatal_error_rx.clone();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), rx.changed()).await;
        assert!(result.is_err(), "should timeout — no fatal pattern matched");

        manager.stop("test").await.unwrap();
    }

    /// Poll a condition with timeout, returning true if the condition was met.
    async fn poll_until(
        timeout: std::time::Duration,
        interval: std::time::Duration,
        mut f: impl FnMut() -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if f() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Async version of poll_until for async conditions.
    async fn poll_until_async<F, Fut>(
        timeout: std::time::Duration,
        interval: std::time::Duration,
        mut f: F,
    ) -> bool
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if f().await {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Helper to build a default AgentConfig for tests.
    fn test_agent_config() -> AgentConfig {
        AgentConfig {
            command: "sleep".to_string(),
            args: vec!["60".to_string()],
            workdir: None,
            env: HashMap::new(),
            auto_start: false,
            restart_on_swap: false,
            restart_on_crash: false,
            depends_on_port: None,
            stop_timeout_secs: 30,
            version_file: None,
            update_command: None,
            update_workdir: None,
            restart_on_error_patterns: vec![],
            data_dir: None,
        }
    }

    // adopt() registers PID and is_running returns true
    #[tokio::test]
    async fn test_agent_adopt_registers_pid_and_is_tracked() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        // Spawn a real process to get a valid PID
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();

        let entry = AgentEntry {
            pid,
            started_at: Utc::now(),
        };

        // Adopt the PID (no child handle)
        wait_for_cmdline(pid, "sleep").await;
        let cfg = config_for("sleep");
        assert!(
            manager.adopt("adopted-agent", &entry, Some(&cfg)).await,
            "adoption should succeed when the PID is alive and cmdline matches"
        );

        // Verify it's tracked and running
        assert!(manager.is_running("adopted-agent").await);

        // Verify it appears in list
        let agents = manager.list().await;
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "adopted-agent");
        assert_eq!(agents[0].pid, pid);
        assert_eq!(agents[0].status, AgentStatus::Running);

        // Clean up
        manager.stop("adopted-agent").await.unwrap();
        drop(child);
    }

    // stop() on adopted agent uses kill-by-PID path
    #[tokio::test]
    async fn test_agent_stop_adopted_kills_by_pid() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        // Spawn a real process
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(false)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();

        let entry = AgentEntry {
            pid,
            started_at: Utc::now(),
        };

        wait_for_cmdline(pid, "sleep").await;
        let cfg = config_for("sleep");
        assert!(manager.adopt("adopted", &entry, Some(&cfg)).await);
        assert!(manager.is_running("adopted").await);

        // Stop the adopted agent (should use kill-by-PID since no child handle)
        manager.stop("adopted").await.unwrap();

        // Wait via the Child handle, NOT by polling is_pid_alive(pid).
        //
        // tokio reaps the children it owns, so once `sleep` dies its PID is freed
        // and can be handed to any other test spawning concurrently — at which
        // point is_pid_alive(pid) becomes true again for an unrelated process and
        // the assertion fails intermittently. try_wait() is bound to this specific
        // child, so it is immune to PID reuse.
        let mut exited = false;
        for _ in 0..100 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                exited = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(exited, "process should have exited after stop");
        assert!(!manager.is_running("adopted").await);
    }

    // stop_all() stops multiple running agents
    #[tokio::test]
    async fn test_agent_stop_all_stops_multiple_agents() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);
        let config = test_agent_config();

        let info1 = manager.start("agent-1", &config).await.unwrap();
        let info2 = manager.start("agent-2", &config).await.unwrap();
        let info3 = manager.start("agent-3", &config).await.unwrap();

        assert!(manager.is_running("agent-1").await);
        assert!(manager.is_running("agent-2").await);
        assert!(manager.is_running("agent-3").await);

        manager.stop_all().await;

        assert!(!manager.is_running("agent-1").await);
        assert!(!manager.is_running("agent-2").await);
        assert!(!manager.is_running("agent-3").await);

        // Poll until all processes are dead.
        //
        // Checks raw PIDs because stop_all consumed the Child handles. That leaves a
        // narrow PID-reuse window — tokio reaps the children it owns, so a freed PID
        // could in principle be reissued to another test and read as alive. The
        // window is small enough not to have been observed; if this ever goes flaky,
        // the fix is to assert on manager state rather than on PID liveness.
        let all_dead = poll_until(
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(50),
            || !is_pid_alive(info1.pid) && !is_pid_alive(info2.pid) && !is_pid_alive(info3.pid),
        )
        .await;
        assert!(all_dead, "all processes should be dead after stop_all");
    }

    #[tokio::test]
    async fn test_watchdog_shutdown_notify_wakes_immediately() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager_arc(log_buffer);

        let handle = manager.spawn_watchdog(HashMap::new());

        // Give the task a moment to enter the select! wait. Without
        // shutdown_notify this would sleep for the full 30s poll interval.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        manager.begin_shutdown();

        let join = tokio::time::timeout(std::time::Duration::from_millis(250), handle)
            .await
            .expect("watchdog should wake immediately on shutdown");
        join.expect("watchdog task should exit cleanly");
    }

    // list() returns correct status and cleans up dead agents
    #[tokio::test]
    async fn test_agent_list_returns_status_and_cleans_dead() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        // Start an agent that exits immediately
        let short_config = AgentConfig {
            command: "true".to_string(),
            args: vec![],
            ..test_agent_config()
        };
        manager.start("short-lived", &short_config).await.unwrap();

        // Start a long-running agent
        let config = test_agent_config();
        manager.start("long-lived", &config).await.unwrap();

        // Poll until the short-lived agent has exited (detected via is_running)
        let exited = poll_until_async(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(50),
            || async { !manager.is_running("short-lived").await },
        )
        .await;
        assert!(exited, "short-lived agent should have exited");

        // list() should detect dead agent and clean it up
        let agents = manager.list().await;
        assert_eq!(agents.len(), 2);

        let short = agents.iter().find(|a| a.name == "short-lived").unwrap();
        assert_eq!(short.status, AgentStatus::Stopped);

        let long = agents.iter().find(|a| a.name == "long-lived").unwrap();
        assert_eq!(long.status, AgentStatus::Running);

        // After list(), dead agents stay in tracking (watchdog handles cleanup).
        // A second list() should still show both agents.
        let agents2 = manager.list().await;
        assert_eq!(agents2.len(), 2);

        manager.stop("long-lived").await.unwrap();
    }

    // Agent persistence — save and load round-trip
    #[test]
    fn test_agent_persistence_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.json");

        let persistence = AgentPersistence { path: path.clone() };

        let now = Utc::now();
        let mut agents = HashMap::new();
        agents.insert(
            "agent-a".to_string(),
            AgentEntry {
                pid: 12345,
                started_at: now,
            },
        );
        agents.insert(
            "agent-b".to_string(),
            AgentEntry {
                pid: 67890,
                started_at: now,
            },
        );

        let state = AgentState { agents };
        persistence.save(&state).unwrap();

        // Verify file was written
        assert!(path.exists());

        // Load and verify
        let loaded = persistence.load().unwrap();
        assert_eq!(loaded.agents.len(), 2);
        assert_eq!(loaded.agents["agent-a"].pid, 12345);
        assert_eq!(loaded.agents["agent-b"].pid, 67890);
    }

    // Agent persistence — load from nonexistent file returns empty
    #[test]
    fn test_agent_persistence_load_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent-agents.json");
        let persistence = AgentPersistence { path };

        let state = persistence.load().unwrap();
        assert!(state.agents.is_empty());
    }

    // Agent persistence — reconcile removes dead agents
    #[test]
    fn test_agent_persistence_reconcile_removes_dead() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.json");
        let persistence = AgentPersistence { path };

        let mut agents = HashMap::new();
        // Use a PID that definitely doesn't exist
        agents.insert(
            "dead-agent".to_string(),
            AgentEntry {
                pid: 999_999_999,
                started_at: Utc::now(),
            },
        );
        // Use PID 1 (init) which is always alive
        agents.insert(
            "alive-agent".to_string(),
            AgentEntry {
                pid: 1,
                started_at: Utc::now(),
            },
        );

        let state = AgentState { agents };
        let reconciled = persistence.reconcile(state);

        // Dead agent should be removed, alive agent kept
        assert!(!reconciled.agents.contains_key("dead-agent"));
        assert!(reconciled.agents.contains_key("alive-agent"));
    }

    // === Agent env var passing — spawn with custom env, verify they're set
    #[tokio::test]
    async fn test_agent_env_var_passing() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer.clone());

        let dir = tempfile::tempdir().unwrap();
        let marker_path = dir.path().join("env_output.txt");
        let marker_str = marker_path.to_str().unwrap().to_string();

        let config = AgentConfig {
            command: "bash".to_string(),
            args: vec![
                "-c".to_string(),
                format!("echo \"MY_VAR=$MY_VAR ANOTHER=$ANOTHER\" > {marker_str}"),
            ],
            env: HashMap::from([
                ("MY_VAR".to_string(), "hello_world".to_string()),
                ("ANOTHER".to_string(), "test_value".to_string()),
            ]),
            ..test_agent_config()
        };

        manager.start("env-test", &config).await.unwrap();

        // Poll for the CONTENT, not for the file's existence.
        //
        // `>` creates the file before echo writes into it, so an exists() poll
        // succeeds against an empty file and the assertion below then reads "".
        // Failed on CI for exactly this reason; a longer timeout cannot fix it,
        // because the file appears immediately either way.
        let file_written = poll_until(
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(50),
            || {
                std::fs::read_to_string(&marker_path)
                    .map(|c| c.contains("MY_VAR="))
                    .unwrap_or(false)
            },
        )
        .await;
        assert!(file_written, "env output file should have been written");

        let content = std::fs::read_to_string(&marker_path).unwrap();
        assert!(
            content.contains("MY_VAR=hello_world"),
            "Expected MY_VAR=hello_world in: {content}"
        );
        assert!(
            content.contains("ANOTHER=test_value"),
            "Expected ANOTHER=test_value in: {content}"
        );
    }

    // === Agent workdir setting — spawn with custom workdir
    #[tokio::test]
    async fn test_agent_workdir_setting() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer.clone());

        let workdir = tempfile::tempdir().unwrap();
        let output_path = workdir.path().join("workdir_output.txt");

        let config = AgentConfig {
            command: "bash".to_string(),
            args: vec!["-c".to_string(), "pwd > workdir_output.txt".to_string()],
            workdir: Some(workdir.path().to_path_buf()),
            ..test_agent_config()
        };

        manager.start("workdir-test", &config).await.unwrap();

        // Poll for the CONTENT, not for the file's existence — `>` creates the
        // file before pwd writes into it, so exists() succeeds against an empty
        // file and the assertion below reads "". Same trap as the env-var test.
        let file_written = poll_until(
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(50),
            || {
                std::fs::read_to_string(&output_path)
                    .map(|c| !c.trim().is_empty())
                    .unwrap_or(false)
            },
        )
        .await;
        assert!(file_written, "workdir output file should have been written");

        let content = std::fs::read_to_string(&output_path).unwrap();
        let expected = workdir.path().to_str().unwrap();
        assert!(
            content.trim().ends_with(expected) || content.trim() == expected,
            "Expected workdir {expected} in output: {content}"
        );
    }

    // === is_running() for adopted (PID check) vs owned (try_wait)
    #[tokio::test]
    async fn test_agent_is_running_adopted_vs_owned() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        // Start an owned agent
        let config = test_agent_config();
        manager.start("owned", &config).await.unwrap();

        // Adopt a process
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let adopted_pid = child.id().unwrap();
        let entry = AgentEntry {
            pid: adopted_pid,
            started_at: Utc::now(),
        };
        wait_for_cmdline(adopted_pid, "sleep").await;
        let cfg = config_for("sleep");
        assert!(manager.adopt("adopted", &entry, Some(&cfg)).await);

        // Both should be running
        assert!(manager.is_running("owned").await);
        assert!(manager.is_running("adopted").await);

        // Nonexistent agent returns false
        assert!(!manager.is_running("nonexistent").await);

        // Clean up
        manager.stop("owned").await.unwrap();
        manager.stop("adopted").await.unwrap();
        drop(child);
    }

    // === Crash detection — agent exits unexpectedly, detected on next list()
    #[tokio::test]
    async fn test_agent_crash_detected_on_list() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        // Start an agent that exits after a brief delay
        let config = AgentConfig {
            command: "bash".to_string(),
            args: vec!["-c".to_string(), "sleep 0.1".to_string()],
            ..test_agent_config()
        };
        manager.start("crasher", &config).await.unwrap();

        // Initially running
        assert!(manager.is_running("crasher").await);

        // Poll until the process has exited
        let exited = poll_until_async(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(50),
            || async { !manager.is_running("crasher").await },
        )
        .await;
        assert!(exited, "crasher agent should have exited");

        // list() should detect the crash and report Stopped
        let agents = manager.list().await;
        let crashed = agents.iter().find(|a| a.name == "crasher");
        assert!(
            crashed.is_some(),
            "crashed agent should still appear in list with Stopped status"
        );
        assert_eq!(crashed.unwrap().status, AgentStatus::Stopped);

        // Dead agents stay in tracking for the watchdog to handle.
        let agents2 = manager.list().await;
        assert_eq!(
            agents2.len(),
            1,
            "dead agent stays in tracking for watchdog"
        );
        assert_eq!(agents2[0].status, AgentStatus::Stopped);
    }

    // === Error count tracking — stderr error lines increment counter
    #[tokio::test]
    async fn test_agent_error_count_tracking() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        // Start an agent that writes error lines to stderr then sleeps
        let config = AgentConfig {
            command: "bash".to_string(),
            args: vec![
                "-c".to_string(),
                "echo 'error: first problem' >&2; echo 'error: second problem' >&2; echo 'error: third problem' >&2; sleep 60"
                    .to_string(),
            ],
            ..test_agent_config()
        };

        manager.start("error-agent", &config).await.unwrap();

        // Poll until at least 3 errors have been captured
        let errors_captured = poll_until_async(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(50),
            || async {
                manager
                    .get_health("error-agent")
                    .await
                    .is_some_and(|h| h.error_count.unwrap_or(0) >= 3)
            },
        )
        .await;
        assert!(errors_captured, "should have captured at least 3 errors");

        let health = manager.get_health("error-agent").await.unwrap();
        assert!(
            health.error_count.unwrap() >= 3,
            "Expected at least 3 errors, got {}",
            health.error_count.unwrap()
        );
        assert!(
            health.lifetime_errors.unwrap() >= 3,
            "Expected at least 3 lifetime errors, got {}",
            health.lifetime_errors.unwrap()
        );

        manager.stop("error-agent").await.unwrap();
    }

    // === stop() on nonexistent agent returns NotFound error
    #[tokio::test]
    async fn test_agent_stop_not_found() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let (_adir, manager) = test_manager(log_buffer);

        let err = manager.stop("nonexistent").await.unwrap_err();
        assert!(matches!(err, AgentError::NotFound(_)));
    }
}

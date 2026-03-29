use chrono::Utc;
use rookery_core::config::AgentConfig;
use rookery_core::state::{AgentEntry, AgentPersistence, AgentState};
use serde::Serialize;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

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
    // Observability metrics
    total_restarts: u32,
    last_restart_reason: Option<String>,
    last_restart_at: Option<chrono::DateTime<Utc>>,
    /// Shared with stderr capture task — incremented on error lines.
    error_count: Arc<AtomicU32>,
    /// Accumulated errors from previous restarts.
    lifetime_errors: u32,
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
            crash_counts: Mutex::new(HashMap::new()),
        }
    }

    /// Adopt a previously-running agent by PID (used after daemon restart).
    pub async fn adopt(&self, name: &str, entry: &AgentEntry, config: Option<&AgentConfig>) {
        tracing::info!(agent = name, pid = entry.pid, "adopting existing agent");
        let version = config
            .and_then(|c| c.version_file.as_ref())
            .and_then(read_version_file);
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
                total_restarts: 0,
                last_restart_reason: None,
                last_restart_at: None,
                error_count: Arc::new(AtomicU32::new(0)),
                lifetime_errors: 0,
            },
        );
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
            let alive = match &mut agent.child {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => is_pid_alive(agent.info.pid),
            };
            if alive {
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

        let version = config.version_file.as_ref().and_then(read_version_file);
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

    pub async fn stop(&self, name: &str) -> Result<(), AgentError> {
        let mut agents = self.agents.lock().await;

        let agent = agents
            .get_mut(name)
            .ok_or_else(|| AgentError::NotFound(name.to_string()))?;

        // Mark as intentional so watchdog doesn't restart it
        agent.intentional_stop = true;

        let pid = agent.info.pid;
        tracing::info!(agent = name, pid, "stopping agent");

        if let Some(ref mut child) = agent.child {
            // Owned child — SIGTERM then wait
            if let Some(cpid) = child.id() {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(cpid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }

            let wait_result =
                tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;

            match wait_result {
                Ok(Ok(status)) => {
                    tracing::info!(agent = name, ?status, "agent exited");
                }
                _ => {
                    tracing::warn!(agent = name, "agent did not exit in time, killing");
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

            for _ in 0..10 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                    break;
                }
            }

            if std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tracing::warn!(
                    agent = name,
                    pid,
                    "adopted agent didn't exit, sending SIGKILL"
                );
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }

        agents.remove(name);
        self.persist_state(&agents);

        // Reset crash count on intentional stop
        self.crash_counts.lock().await.remove(name);

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

        // Check each agent's actual status
        let mut dead = Vec::new();
        for (name, agent) in agents.iter_mut() {
            let alive = match &mut agent.child {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => is_pid_alive(agent.info.pid),
            };

            if alive {
                result.push(agent.info.clone());
            } else {
                let mut info = agent.info.clone();
                info.status = AgentStatus::Stopped;
                result.push(info);
                dead.push(name.clone());
            }
        }

        // Clean up dead agents
        if !dead.is_empty() {
            for name in dead {
                agents.remove(&name);
            }
            self.persist_state(&agents);
        }

        result
    }

    /// Get health/metrics for a specific agent.
    pub async fn get_health(&self, name: &str) -> Option<AgentInfo> {
        let agents = self.agents.lock().await;
        let agent = agents.get(name)?;

        let uptime_secs = if agent.info.status == AgentStatus::Running {
            Some(
                Utc::now()
                    .signed_duration_since(agent.info.started_at)
                    .num_seconds(),
            )
        } else {
            None
        };

        Some(AgentInfo {
            name: agent.info.name.clone(),
            pid: agent.info.pid,
            started_at: agent.info.started_at,
            status: agent.info.status.clone(),
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
        if let Some(agent) = agents.get_mut(name) {
            match &mut agent.child {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => is_pid_alive(agent.info.pid),
            }
        } else {
            false
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

            let mut fatal_rx = manager.fatal_error_rx.clone();

            // Track dependency port liveness for down→up transition detection.
            // Initialized to true so a cold start doesn't trigger a false bounce.
            let tracked_ports: std::collections::HashSet<u16> =
                configs.values().filter_map(|c| c.depends_on_port).collect();
            let mut port_was_up: HashMap<u16, bool> =
                tracked_ports.iter().map(|&p| (p, true)).collect();

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
                                let _ = manager.stop(&agent_name).await;
                                if manager.is_shutting_down() { return; }
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
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
                        continue;
                    }
                }

                // Check dependency ports for down→up transitions (server restarted).
                // Agents holding stale connections need to be bounced.
                if !tracked_ports.is_empty() {
                    let mut ports_recovered: Vec<u16> = Vec::new();

                    for &port in &tracked_ports {
                        let is_up =
                            crate::health::check_health(port, std::time::Duration::from_secs(3))
                                .await;
                        let was_up = port_was_up.get(&port).copied().unwrap_or(true);

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
                            port_was_up.insert(port, is_up);
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
                                let _ = manager.stop(&name).await;
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
                        let alive = match &mut agent.child {
                            Some(child) => matches!(child.try_wait(), Ok(None)),
                            None => is_pid_alive(agent.info.pid),
                        };

                        if !alive
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
fn read_version_file(path: &std::path::PathBuf) -> Option<String> {
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
            version_file: None,
            restart_on_error_patterns: vec![],
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
            version_file: None,
            restart_on_error_patterns: vec![],
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
            version_file: None,
            restart_on_error_patterns: vec![],
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
            version_file: None,
            restart_on_error_patterns: vec![],
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
            version_file: None,
            restart_on_error_patterns: vec![],
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
            version_file: None,
            restart_on_error_patterns: vec!["telegram.error.TimedOut".to_string()],
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
            version_file: None,
            restart_on_error_patterns: vec!["telegram.error.TimedOut".to_string()],
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
            version_file: None,
            restart_on_error_patterns: vec![],
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
        manager.adopt("adopted-agent", &entry, None).await;

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
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(false)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();

        let entry = AgentEntry {
            pid,
            started_at: Utc::now(),
        };

        manager.adopt("adopted", &entry, None).await;
        assert!(manager.is_running("adopted").await);

        // Stop the adopted agent (should use kill-by-PID since no child handle)
        manager.stop("adopted").await.unwrap();

        // Poll until the process is dead
        let died = poll_until(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(50),
            || !is_pid_alive(pid),
        )
        .await;
        assert!(died, "process should have died after stop");
        assert!(!manager.is_running("adopted").await);

        drop(child);
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

        // Poll until all processes are dead
        let all_dead = poll_until(
            std::time::Duration::from_secs(5),
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

        // After list(), the dead agent should be removed from tracking
        // A second list() should only show the still-running agent
        let agents2 = manager.list().await;
        assert_eq!(agents2.len(), 1);
        assert_eq!(agents2[0].name, "long-lived");

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

        // Poll until the output file is written
        let file_written = poll_until(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(50),
            || marker_path.exists(),
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

        // Poll until the output file is written
        let file_written = poll_until(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(50),
            || output_path.exists(),
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
        manager.adopt("adopted", &entry, None).await;

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

        // After list cleans up, it's no longer tracked
        let agents2 = manager.list().await;
        assert!(
            agents2.is_empty(),
            "dead agent should be cleaned up after list()"
        );
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

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
}

pub struct AgentManager {
    agents: Mutex<HashMap<String, ManagedAgent>>,
    log_buffer: Arc<LogBuffer>,
    persistence: AgentPersistence,
    /// Tracks consecutive crash count per agent for exponential backoff.
    crash_counts: Mutex<HashMap<String, u32>>,
}

impl AgentManager {
    pub fn new(log_buffer: Arc<LogBuffer>) -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
            log_buffer,
            persistence: AgentPersistence::new(),
            crash_counts: Mutex::new(HashMap::new()),
        }
    }

    /// Adopt a previously-running agent by PID (used after daemon restart).
    pub async fn adopt(&self, name: &str, entry: &AgentEntry, config: Option<&AgentConfig>) {
        tracing::info!(agent = name, pid = entry.pid, "adopting existing agent");
        let version = config.and_then(|c| c.version_file.as_ref()).and_then(read_version_file);
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

    pub async fn start(
        &self,
        name: &str,
        config: &AgentConfig,
    ) -> Result<AgentInfo, AgentError> {
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
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.to_ascii_lowercase().contains("error") {
                        err_count.fetch_add(1, Ordering::Relaxed);
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
                tracing::warn!(agent = name, pid, "adopted agent didn't exit, sending SIGKILL");
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

    pub async fn stop_all(&self) {
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
            Some(Utc::now().signed_duration_since(agent.info.started_at).num_seconds())
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
        })
    }

    /// Record restart metrics on a newly-started agent.
    pub async fn record_restart(&self, name: &str, reason: &str, prev_restarts: u32) {
        let mut agents = self.agents.lock().await;
        if let Some(agent) = agents.get_mut(name) {
            agent.total_restarts = prev_restarts + 1;
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

            // Track dependency port liveness for down→up transition detection.
            // Initialized to true so a cold start doesn't trigger a false bounce.
            let tracked_ports: std::collections::HashSet<u16> = configs
                .values()
                .filter_map(|c| c.depends_on_port)
                .collect();
            let mut port_was_up: HashMap<u16, bool> = tracked_ports
                .iter()
                .map(|&p| (p, true))
                .collect();

            loop {
                tokio::time::sleep(POLL_INTERVAL).await;

                // Check dependency ports for down→up transitions (server restarted).
                // Agents holding stale connections need to be bounced.
                if !tracked_ports.is_empty() {
                    let mut ports_recovered: Vec<u16> = Vec::new();

                    for &port in &tracked_ports {
                        let is_up = crate::health::check_health(
                            port,
                            std::time::Duration::from_secs(3),
                        )
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
                        let bounce_info: Vec<(String, u32)> = {
                            let agents = manager.agents.lock().await;
                            agents
                                .iter()
                                .filter(|(name, agent)| {
                                    if let Some(cfg) = configs.get(*name) {
                                        if let Some(dep_port) = cfg.depends_on_port {
                                            if ports_recovered.contains(&dep_port) {
                                                let uptime = Utc::now()
                                                    .signed_duration_since(agent.info.started_at)
                                                    .num_seconds();
                                                return uptime > BOUNCE_MIN_UPTIME_SECS
                                                    && !agent.intentional_stop;
                                            }
                                        }
                                    }
                                    false
                                })
                                .map(|(name, agent)| (name.clone(), agent.total_restarts))
                                .collect()
                        };

                        for (name, prev_restarts) in bounce_info {
                            if let Some(cfg) = configs.get(&name) {
                                tracing::info!(
                                    agent = %name,
                                    "bouncing agent after dependency port recovered"
                                );
                                let _ = manager.stop(&name).await;
                                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                                match manager.start(&name, cfg).await {
                                    Ok(info) => {
                                        manager.record_restart(&name, "port_recovery", prev_restarts).await;
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
                let to_restart: Vec<(String, u32)> = {
                    let mut agents = manager.agents.lock().await;
                    let mut dead_entries = Vec::new();

                    for (name, agent) in agents.iter_mut() {
                        let alive = match &mut agent.child {
                            Some(child) => matches!(child.try_wait(), Ok(None)),
                            None => is_pid_alive(agent.info.pid),
                        };

                        if !alive && !agent.intentional_stop {
                            // Check if this agent has restart_on_crash
                            if let Some(cfg) = configs.get(name) {
                                if cfg.restart_on_crash {
                                    tracing::warn!(
                                        agent = %name,
                                        pid = agent.info.pid,
                                        "agent exited unexpectedly, scheduling restart"
                                    );
                                    dead_entries.push((name.clone(), agent.total_restarts));
                                }
                            }
                        }
                    }

                    // Remove dead agents from tracking
                    let dead_names: Vec<String> = dead_entries.iter().map(|(n, _)| n.clone()).collect();
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
                for (name, prev_restarts) in to_restart {
                    let crash_count = {
                        let mut counts = manager.crash_counts.lock().await;
                        let count = counts.entry(name.clone()).or_insert(0);
                        *count += 1;
                        *count
                    };

                    // Exponential backoff: 1s, 2s, 4s, 8s, ... capped at 60s
                    let backoff_secs =
                        (1u64 << (crash_count - 1).min(6)).min(MAX_BACKOFF_SECS);

                    tracing::info!(
                        agent = %name,
                        crash_count,
                        backoff_secs,
                        "waiting before restart"
                    );

                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;

                    if let Some(cfg) = configs.get(&name) {
                        match manager.start(&name, cfg).await {
                            Ok(info) => {
                                manager.record_restart(&name, "crash", prev_restarts).await;
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

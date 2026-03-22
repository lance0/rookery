use chrono::Utc;
use rookery_core::config::AgentConfig;
use rookery_core::state::{AgentEntry, AgentPersistence, AgentState};
use serde::Serialize;
use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::logs::LogBuffer;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub pid: u32,
    pub started_at: chrono::DateTime<Utc>,
    pub status: AgentStatus,
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
}

pub struct AgentManager {
    agents: Mutex<HashMap<String, ManagedAgent>>,
    log_buffer: Arc<LogBuffer>,
    persistence: AgentPersistence,
}

impl AgentManager {
    pub fn new(log_buffer: Arc<LogBuffer>) -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
            log_buffer,
            persistence: AgentPersistence::new(),
        }
    }

    /// Adopt a previously-running agent by PID (used after daemon restart).
    pub async fn adopt(&self, name: &str, entry: &AgentEntry) {
        tracing::info!(agent = name, pid = entry.pid, "adopting existing agent");
        let info = AgentInfo {
            name: name.to_string(),
            pid: entry.pid,
            started_at: entry.started_at,
            status: AgentStatus::Running,
        };
        let mut agents = self.agents.lock().await;
        agents.insert(
            name.to_string(),
            ManagedAgent { child: None, info },
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
                None => std::path::Path::new(&format!("/proc/{}", agent.info.pid)).exists(),
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

        // Capture output into log buffer with agent prefix
        let prefix = format!("[agent:{name}]");
        if let Some(stderr) = child.stderr.take() {
            let buf = self.log_buffer.clone();
            let p = prefix.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
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

        let info = AgentInfo {
            name: name.to_string(),
            pid,
            started_at: Utc::now(),
            status: AgentStatus::Running,
        };

        agents.insert(
            name.to_string(),
            ManagedAgent {
                child: Some(child),
                info: info.clone(),
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
                None => std::path::Path::new(&format!("/proc/{}", agent.info.pid)).exists(),
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

    pub async fn is_running(&self, name: &str) -> bool {
        let mut agents = self.agents.lock().await;
        if let Some(agent) = agents.get_mut(name) {
            match &mut agent.child {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => std::path::Path::new(&format!("/proc/{}", agent.info.pid)).exists(),
            }
        } else {
            false
        }
    }
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

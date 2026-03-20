use chrono::Utc;
use rookery_core::config::AgentConfig;
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
    child: Child,
    info: AgentInfo,
}

pub struct AgentManager {
    agents: Mutex<HashMap<String, ManagedAgent>>,
    log_buffer: Arc<LogBuffer>,
}

impl AgentManager {
    pub fn new(log_buffer: Arc<LogBuffer>) -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
            log_buffer,
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
            match agent.child.try_wait() {
                Ok(None) => return Err(AgentError::AlreadyRunning(name.to_string())),
                _ => {
                    // Exited, clean up
                    agents.remove(name);
                }
            }
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
                child,
                info: info.clone(),
            },
        );

        tracing::info!(agent = name, pid, "agent started");
        Ok(info)
    }

    pub async fn stop(&self, name: &str) -> Result<(), AgentError> {
        let mut agents = self.agents.lock().await;

        let agent = agents
            .get_mut(name)
            .ok_or_else(|| AgentError::NotFound(name.to_string()))?;

        tracing::info!(agent = name, pid = agent.info.pid, "stopping agent");

        // SIGTERM first
        if let Some(pid) = agent.child.id() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }

        // Wait up to 5 seconds
        let wait_result =
            tokio::time::timeout(std::time::Duration::from_secs(5), agent.child.wait()).await;

        match wait_result {
            Ok(Ok(status)) => {
                tracing::info!(agent = name, ?status, "agent exited");
            }
            _ => {
                tracing::warn!(agent = name, "agent did not exit in time, killing");
                let _ = agent.child.kill().await;
            }
        }

        agents.remove(name);
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
            match agent.child.try_wait() {
                Ok(None) => {
                    // Still running
                    result.push(agent.info.clone());
                }
                Ok(Some(status)) => {
                    let mut info = agent.info.clone();
                    info.status = if status.success() {
                        AgentStatus::Stopped
                    } else {
                        AgentStatus::Failed {
                            error: format!("exited with {status}"),
                        }
                    };
                    result.push(info);
                    dead.push(name.clone());
                }
                Err(e) => {
                    let mut info = agent.info.clone();
                    info.status = AgentStatus::Failed {
                        error: e.to_string(),
                    };
                    result.push(info);
                    dead.push(name.clone());
                }
            }
        }

        // Clean up dead agents
        for name in dead {
            agents.remove(&name);
        }

        result
    }

    pub async fn is_running(&self, name: &str) -> bool {
        let mut agents = self.agents.lock().await;
        if let Some(agent) = agents.get_mut(name) {
            matches!(agent.child.try_wait(), Ok(None))
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

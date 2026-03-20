use chrono::Utc;
use rookery_core::config::Config;
use rookery_core::error::{Error, Result};
use rookery_core::state::ServerState;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::health;
use crate::logs::LogBuffer;

pub struct ProcessManager {
    child: Arc<Mutex<Option<Child>>>,
    info: Arc<Mutex<Option<ProcessInfo>>>,
    log_buffer: Arc<LogBuffer>,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub port: u16,
    pub profile: String,
    pub started_at: chrono::DateTime<Utc>,
    pub command_line: Vec<String>,
    pub exe_path: PathBuf,
}

impl ProcessManager {
    pub fn new(log_buffer: Arc<LogBuffer>) -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            info: Arc::new(Mutex::new(None)),
            log_buffer,
        }
    }

    pub async fn start(&self, config: &Config, profile_name: &str) -> Result<ProcessInfo> {
        // Check if already running
        if self.is_running().await {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "server is already running",
            )));
        }

        let args = config.resolve_command_line(profile_name)?;
        let profile = config
            .profiles
            .get(profile_name)
            .ok_or_else(|| Error::ProfileNotFound(profile_name.into()))?;

        tracing::info!(profile = profile_name, "starting llama-server");
        tracing::debug!(args = ?args, "command line");

        // args[0] is the binary, rest are arguments
        let (binary, cmd_args) = args.split_first().ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty command line",
            ))
        })?;

        let mut child = Command::new(binary)
            .args(cmd_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false) // we manage lifecycle explicitly
            .spawn()
            .map_err(Error::Io)?;

        let pid = child.id().ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "failed to get child PID",
            ))
        })?;

        let info = ProcessInfo {
            pid,
            port: profile.port,
            profile: profile_name.to_string(),
            started_at: Utc::now(),
            command_line: args.clone(),
            exe_path: config.llama_server.clone(),
        };

        // Capture stdout/stderr into log buffer
        let log_buf = self.log_buffer.clone();
        if let Some(stderr) = child.stderr.take() {
            let buf = log_buf.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    buf.push(line);
                }
            });
        }
        if let Some(stdout) = child.stdout.take() {
            let buf = log_buf;
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    buf.push(line);
                }
            });
        }

        *self.child.lock().await = Some(child);
        *self.info.lock().await = Some(info.clone());

        Ok(info)
    }

    pub async fn stop(&self) -> Result<()> {
        let mut child_lock = self.child.lock().await;

        if let Some(ref mut child) = *child_lock {
            let pid = child.id();
            tracing::info!(?pid, "stopping llama-server");

            // Try SIGTERM first
            if let Some(pid) = pid {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }

            // Wait up to 10 seconds for graceful exit
            let wait_result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                child.wait(),
            )
            .await;

            match wait_result {
                Ok(Ok(status)) => {
                    tracing::info!(?status, "llama-server exited gracefully");
                }
                _ => {
                    tracing::warn!("llama-server did not exit in time, sending SIGKILL");
                    let _ = child.kill().await;
                }
            }
        }

        *child_lock = None;
        *self.info.lock().await = None;

        Ok(())
    }

    pub async fn is_running(&self) -> bool {
        let mut child_lock = self.child.lock().await;
        if let Some(ref mut child) = *child_lock {
            // try_wait returns None if still running
            match child.try_wait() {
                Ok(None) => true,
                _ => {
                    // Process exited, clean up
                    drop(child_lock);
                    *self.child.lock().await = None;
                    *self.info.lock().await = None;
                    false
                }
            }
        } else {
            false
        }
    }

    pub async fn process_info(&self) -> Option<ProcessInfo> {
        self.info.lock().await.clone()
    }

    pub async fn to_server_state(&self) -> ServerState {
        if let Some(info) = self.process_info().await {
            if self.is_running().await {
                ServerState::Running {
                    profile: info.profile,
                    pid: info.pid,
                    port: info.port,
                    since: info.started_at,
                    command_line: info.command_line,
                    exe_path: Some(info.exe_path),
                }
            } else {
                ServerState::Stopped
            }
        } else {
            ServerState::Stopped
        }
    }

    /// Start and wait for health check, returning the final state.
    pub async fn start_and_wait(
        &self,
        config: &Config,
        profile_name: &str,
    ) -> Result<ServerState> {
        let info = self.start(config, profile_name).await?;

        // Wait for health with 120s timeout (model download can take a while)
        match health::wait_for_health(info.port, std::time::Duration::from_secs(120)).await {
            Ok(()) => Ok(self.to_server_state().await),
            Err(e) => {
                tracing::error!(error = %e, "health check failed, stopping server");
                let _ = self.stop().await;
                Ok(ServerState::Failed {
                    last_error: e.to_string(),
                    profile: profile_name.to_string(),
                    since: Utc::now(),
                })
            }
        }
    }
}

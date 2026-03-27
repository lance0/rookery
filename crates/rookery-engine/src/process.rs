use chrono::Utc;
use rookery_core::config::Config;
use rookery_core::error::{Error, Result};
use rookery_core::state::ServerState;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, watch};

use crate::health;
use crate::logs::LogBuffer;

/// Check if a PID is alive and not a zombie.
/// Reads /proc/{pid}/stat and checks the state field (3rd field).
/// Returns false for zombies (state 'Z') and non-existent processes.
pub fn is_pid_alive(pid: u32) -> bool {
    let stat_path = format!("/proc/{pid}/stat");
    match std::fs::read_to_string(&stat_path) {
        Ok(content) => {
            // Format: "pid (comm) state ..."
            // The comm field can contain spaces and parens, so find the last ')' first
            if let Some(pos) = content.rfind(')') {
                let after_comm = &content[pos + 1..];
                let state = after_comm.trim().chars().next().unwrap_or('?');
                state != 'Z'
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

pub struct ProcessManager {
    child: Arc<Mutex<Option<Child>>>,
    info: Arc<Mutex<Option<ProcessInfo>>>,
    log_buffer: Arc<LogBuffer>,
    draining: AtomicBool,
    cuda_error_tx: watch::Sender<bool>,
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
        let (cuda_error_tx, _) = watch::channel(false);
        Self {
            child: Arc::new(Mutex::new(None)),
            info: Arc::new(Mutex::new(None)),
            log_buffer,
            draining: AtomicBool::new(false),
            cuda_error_tx,
        }
    }

    /// Subscribe to CUDA error notifications from llama-server stderr.
    pub fn subscribe_cuda_errors(&self) -> watch::Receiver<bool> {
        self.cuda_error_tx.subscribe()
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
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

        // Protect llama-server from OOM killer (requires CAP_SYS_RESOURCE or root)
        let oom_path = format!("/proc/{pid}/oom_score_adj");
        if let Err(e) = std::fs::write(&oom_path, "-900") {
            tracing::warn!(pid, error = %e, "failed to set oom_score_adj");
        } else {
            tracing::info!(pid, "set oom_score_adj to -900");
        }

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
            let cuda_tx = self.cuda_error_tx.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // Detect CUDA errors to trigger immediate canary
                    let lower = line.to_ascii_lowercase();
                    if lower.contains("cuda error") || lower.contains("ggml_cuda_error") {
                        tracing::error!("CUDA error detected in stderr: {line}");
                        let _ = cuda_tx.send(true);
                    }
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
            tracing::info!(?pid, "stopping llama-server (owned child)");

            // Try SIGTERM first
            if let Some(pid) = pid {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }

            // Wait up to 10 seconds for graceful exit
            let wait_result =
                tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await;

            match wait_result {
                Ok(Ok(status)) => {
                    tracing::info!(?status, "llama-server exited gracefully");
                }
                _ => {
                    tracing::warn!("llama-server did not exit in time, sending SIGKILL");
                    let _ = child.kill().await;
                }
            }
        } else if let Some(info) = self.info.lock().await.as_ref() {
            // No child handle (daemon restarted), but we know the PID — kill by PID
            let pid = info.pid;
            tracing::info!(pid, "stopping llama-server (orphan PID, no child handle)");

            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );

            // Wait for process to exit
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let proc_path = std::path::PathBuf::from(format!("/proc/{pid}"));
                if !proc_path.exists() {
                    tracing::info!(pid, "orphan llama-server exited");
                    break;
                }
            }

            // SIGKILL if still alive
            let proc_path = std::path::PathBuf::from(format!("/proc/{pid}"));
            if proc_path.exists() {
                tracing::warn!(pid, "orphan didn't exit, sending SIGKILL");
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }

        *child_lock = None;
        *self.info.lock().await = None;

        Ok(())
    }

    /// Adopt an existing process by PID (used when daemon restarts and finds a running server).
    pub async fn adopt(&self, info: ProcessInfo) {
        tracing::info!(pid = info.pid, profile = %info.profile, "adopting existing llama-server");
        *self.info.lock().await = Some(info);
        // No child handle — stop() will fall back to kill-by-PID
    }

    pub async fn is_running(&self) -> bool {
        let mut child_lock = self.child.lock().await;
        if let Some(ref mut child) = *child_lock {
            matches!(child.try_wait(), Ok(None))
        } else {
            // No child handle — check by PID (adopted process), excluding zombies
            if let Some(info) = self.info.lock().await.as_ref() {
                is_pid_alive(info.pid)
            } else {
                false
            }
        }
    }

    pub async fn process_info(&self) -> Option<ProcessInfo> {
        self.info.lock().await.clone()
    }

    pub async fn to_server_state(&self) -> ServerState {
        let alive = self.is_running().await;
        let info = self.info.lock().await.clone();

        match (alive, info) {
            (true, Some(info)) => ServerState::Running {
                profile: info.profile,
                pid: info.pid,
                port: info.port,
                since: info.started_at,
                command_line: info.command_line,
                exe_path: Some(info.exe_path),
            },
            _ => ServerState::Stopped,
        }
    }

    /// Hot-swap: drain in-flight requests, stop current server, start new profile, health check.
    pub async fn swap(&self, config: &Config, new_profile: &str) -> Result<ServerState> {
        let old_profile = self.process_info().await.map(|i| i.profile.clone());

        tracing::info!(
            from = ?old_profile,
            to = new_profile,
            "hot-swapping model"
        );

        // Stop current if running, with drain period
        if self.is_running().await {
            self.draining.store(true, Ordering::SeqCst);
            tracing::info!("draining in-flight requests (5s)");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            self.stop().await?;
            self.draining.store(false, Ordering::SeqCst);
        }

        // Start new profile
        self.start_and_wait(config, new_profile).await
    }

    /// Start and wait for health check, returning the final state.
    pub async fn start_and_wait(&self, config: &Config, profile_name: &str) -> Result<ServerState> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_pid_alive_current_process() {
        let pid = std::process::id();
        assert!(is_pid_alive(pid), "current process should be alive");
    }

    #[test]
    fn test_is_pid_alive_nonexistent() {
        assert!(!is_pid_alive(999_999_999));
    }

    #[test]
    fn test_is_pid_alive_pid_1() {
        assert!(is_pid_alive(1));
    }

    #[test]
    fn test_is_pid_alive_parses_stat() {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id()));
        assert!(stat.is_ok());
        let content = stat.unwrap();
        let pos = content.rfind(')').unwrap();
        let state = content[pos + 1..].trim().chars().next().unwrap();
        assert_eq!(state, 'R', "test process should be Running");
    }
}

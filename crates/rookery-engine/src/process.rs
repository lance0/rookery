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

    pub fn set_draining(&self, draining: bool) {
        self.draining.store(draining, Ordering::SeqCst);
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

        let pid = child
            .id()
            .ok_or_else(|| Error::Io(std::io::Error::other("failed to get child PID")))?;

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
                backend_type: rookery_core::config::BackendType::LlamaServer,
                container_id: None,
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
    use rookery_core::config::{Config, Model, Profile};
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    // ── Test helpers ──────────────────────────────────────────────────

    /// Build a minimal Config with a given binary path and port.
    /// The binary is called via `Command::new(binary).args(cmd_args)` where
    /// `cmd_args` contains all llama-server flags. Use a wrapper script
    /// (see `make_sleep_script`) that ignores these flags.
    fn make_test_config(binary: &str, port: u16) -> Config {
        Config {
            llama_server: PathBuf::from(binary),
            default_profile: "test".into(),
            listen: "127.0.0.1:19876".parse().unwrap(),
            idle_timeout: None,
            models: HashMap::from([(
                "test_model".into(),
                Model {
                    source: "local".into(),
                    repo: None,
                    file: None,
                    path: Some(PathBuf::from("/tmp/fake.gguf")),
                    estimated_vram_mb: None,
                },
            )]),
            profiles: HashMap::from([(
                "test".into(),
                Profile {
                    model: "test_model".into(),
                    port,
                    llama_server: None,
                    vllm: None,
                    ctx_size: 1024,
                    threads: 1,
                    threads_batch: 1,
                    batch_size: 512,
                    ubatch_size: 256,
                    gpu_layers: 0,
                    gpu_index: None,
                    cache_type_k: "f16".into(),
                    cache_type_v: "f16".into(),
                    flash_attention: false,
                    reasoning_budget: 0,
                    chat_template: None,
                    temp: 0.7,
                    top_p: 0.8,
                    top_k: 20,
                    min_p: 0.0,
                    aliases: vec![],
                    extra_args: vec![],
                },
            )]),
            agents: HashMap::new(),
        }
    }

    /// Write an executable script to a path, ensuring the file handle is fully
    /// closed and synced before returning. This prevents ETXTBSY errors when
    /// the script is immediately executed.
    fn write_test_script(path: &std::path::Path, content: &str) {
        use std::io::Write;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.sync_all().unwrap();
        drop(f);
        std::fs::set_permissions(path, PermissionsExt::from_mode(0o755)).unwrap();
        // Brief pause to ensure filesystem has fully released the file handle.
        // Prevents ETXTBSY on Linux when multiple test threads write scripts.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    /// Create a temp script that ignores all arguments and just sleeps.
    /// Returns (TempDir, script_path_string). TempDir must be kept alive
    /// for the duration of the test to prevent cleanup.
    fn make_sleep_script() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fake_server.sh");
        write_test_script(&script_path, "#!/bin/sh\nsleep 300\n");
        let path_str = script_path.to_str().unwrap().to_string();
        (dir, path_str)
    }

    // ── Existing is_pid_alive tests ───────────────────────────────────

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
        // Process may be R (running), S (sleeping), or D (disk sleep) — all are alive, not Z (zombie)
        assert!(
            matches!(state, 'R' | 'S' | 'D'),
            "test process should be alive (R/S/D), got '{state}'"
        );
    }

    // ── ProcessManager lifecycle tests ────────────────────────────────

    /// 1. start() spawns child and reports correct ProcessInfo fields (pid, port, profile, started_at)
    #[tokio::test]
    async fn test_start_returns_correct_process_info() {
        let (_dir, script) = make_sleep_script();
        let config = make_test_config(&script, 19001);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        let info = pm
            .start(&config, "test")
            .await
            .expect("start should succeed");

        assert!(info.pid > 0, "PID should be positive");
        assert_eq!(info.port, 19001, "port should match profile config");
        assert_eq!(info.profile, "test", "profile name should match");
        assert_eq!(info.exe_path, PathBuf::from(&script));
        assert!(
            !info.command_line.is_empty(),
            "command_line should be populated"
        );
        assert_eq!(
            info.command_line[0], script,
            "first arg should be the binary"
        );
        // started_at should be recent (within last 5 seconds)
        let elapsed = Utc::now() - info.started_at;
        assert!(
            elapsed.num_seconds() < 5,
            "started_at should be recent, was {elapsed}"
        );
        // Process should actually be running
        assert!(
            pm.is_running().await,
            "process should be running after start"
        );

        // Clean up
        pm.stop().await.unwrap();
    }

    /// 2. start() captures stdout into LogBuffer
    #[tokio::test]
    async fn test_start_captures_stdout_into_log_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("test_echo.sh");
        write_test_script(
            &script_path,
            "#!/bin/sh\necho 'hello from test stdout'\necho 'second line'\nsleep 300\n",
        );

        let config = make_test_config(script_path.to_str().unwrap(), 19002);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer.clone());

        let info = pm
            .start(&config, "test")
            .await
            .expect("start should succeed");
        assert!(info.pid > 0);

        // Wait a moment for stdout to be captured by the async reader
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let lines = log_buffer.last_n(10);
        assert!(
            lines.iter().any(|l| l.contains("hello from test stdout")),
            "LogBuffer should contain stdout output, got: {lines:?}"
        );

        // Clean up
        pm.stop().await.unwrap();
    }

    /// 3. stop() on owned child — verify process dies, is_running returns false
    #[tokio::test]
    async fn test_stop_owned_child_kills_process() {
        let (_dir, script) = make_sleep_script();
        let config = make_test_config(&script, 19003);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        let info = pm.start(&config, "test").await.unwrap();
        assert!(pm.is_running().await, "should be running after start");
        let pid = info.pid;

        pm.stop().await.expect("stop should succeed");

        assert!(!pm.is_running().await, "should not be running after stop");
        assert!(
            pm.process_info().await.is_none(),
            "process_info should be None after stop"
        );
        // The OS process should also be dead
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !is_pid_alive(pid),
            "OS process with PID {pid} should be dead after stop"
        );
    }

    /// 4. stop() when idle — returns Ok (no-op)
    #[tokio::test]
    async fn test_stop_when_idle_is_noop() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        // stop() on a freshly-created ProcessManager should be fine
        let result = pm.stop().await;
        assert!(result.is_ok(), "stop on idle should return Ok");
        assert!(!pm.is_running().await, "should not be running");
    }

    /// 5. adopt() then is_running() — returns true for live PID
    #[tokio::test]
    async fn test_adopt_then_is_running_for_live_pid() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        // Spawn a long-running process that we can adopt
        let mut child = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();

        let info = ProcessInfo {
            pid,
            port: 19005,
            profile: "adopted".into(),
            started_at: Utc::now(),
            command_line: vec!["/bin/sleep".into(), "60".into()],
            exe_path: PathBuf::from("/bin/sleep"),
        };

        pm.adopt(info).await;

        assert!(
            pm.is_running().await,
            "adopted process with live PID should be running"
        );

        let proc_info = pm.process_info().await;
        assert!(proc_info.is_some());
        assert_eq!(proc_info.unwrap().pid, pid);

        // Clean up: stop the adopted process
        pm.stop().await.unwrap();
        // Also clean up the original child handle
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    /// 6. adopt() then stop() — kills by PID (no child handle)
    #[tokio::test]
    async fn test_adopt_then_stop_kills_by_pid() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        // Spawn a real process to adopt
        let mut child = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();

        let info = ProcessInfo {
            pid,
            port: 19006,
            profile: "adopted".into(),
            started_at: Utc::now(),
            command_line: vec!["/bin/sleep".into(), "60".into()],
            exe_path: PathBuf::from("/bin/sleep"),
        };

        pm.adopt(info).await;
        assert!(pm.is_running().await, "should be running after adopt");

        // stop() should use the kill-by-PID path since there's no child handle
        pm.stop()
            .await
            .expect("stop should succeed for adopted process");

        assert!(!pm.is_running().await, "should not be running after stop");

        // Give the OS a moment to clean up
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !is_pid_alive(pid),
            "PID {pid} should be dead after stop on adopted process"
        );

        // Clean up child handle (already dead, but wait to reap)
        let _ = child.wait().await;
    }

    /// 7. is_running() returns false after process exits on its own
    #[tokio::test]
    async fn test_is_running_false_after_process_exits() {
        // Use a script that exits quickly
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("test_exit.sh");
        write_test_script(&script_path, "#!/bin/sh\nexit 0\n");

        let config = make_test_config(script_path.to_str().unwrap(), 19007);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        pm.start(&config, "test").await.unwrap();

        // Wait for the process to exit
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        assert!(
            !pm.is_running().await,
            "is_running should return false after process exits on its own"
        );
    }

    /// 8. to_server_state() returns correct Running state with PID/port
    #[tokio::test]
    async fn test_to_server_state_returns_running_when_active() {
        let (_dir, script) = make_sleep_script();
        let config = make_test_config(&script, 19008);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        let info = pm.start(&config, "test").await.unwrap();

        let state = pm.to_server_state().await;
        match state {
            ServerState::Running {
                profile, pid, port, ..
            } => {
                assert_eq!(profile, "test");
                assert_eq!(pid, info.pid);
                assert_eq!(port, 19008);
            }
            other => panic!("expected Running state, got {other:?}"),
        }

        pm.stop().await.unwrap();
    }

    /// 9. to_server_state() returns Stopped when nothing running
    #[tokio::test]
    async fn test_to_server_state_returns_stopped_when_idle() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        let state = pm.to_server_state().await;
        assert!(
            matches!(state, ServerState::Stopped),
            "expected Stopped, got {state:?}"
        );
    }

    /// 10. start_and_wait() succeeds when mock server is healthy
    #[tokio::test]
    async fn test_start_and_wait_succeeds_with_healthy_server() {
        use crate::test_utils::MockLlamaServer;

        // Start a mock server first to get its port
        let mock = MockLlamaServer::start().await;
        let port = mock.port();

        // Use a wrapper script that ignores args and stays alive.
        // start_and_wait() calls start() (spawning this script) then
        // wait_for_health(port). The mock server already serves /health on
        // that port, so the health check passes.
        let (_dir, script) = make_sleep_script();
        let config = make_test_config(&script, port);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        let state = pm
            .start_and_wait(&config, "test")
            .await
            .expect("start_and_wait should succeed");

        match state {
            ServerState::Running {
                profile,
                port: state_port,
                ..
            } => {
                assert_eq!(profile, "test");
                assert_eq!(state_port, port);
            }
            other => panic!("expected Running state, got {other:?}"),
        }

        // Clean up
        pm.stop().await.unwrap();
        mock.shutdown().await;
    }

    /// 11. start_and_wait() returns Failed state on health timeout
    ///
    /// We cannot call start_and_wait() directly because it has a hardcoded
    /// 120s health timeout. Instead, we replicate its logic with a short
    /// timeout to verify the failure → stop → Failed state pattern.
    #[tokio::test]
    async fn test_start_and_wait_returns_failed_on_health_timeout() {
        // Get a free port with no HTTP server → health check will fail
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let (_dir, script) = make_sleep_script();
        let config = make_test_config(&script, port);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        let info = pm
            .start(&config, "test")
            .await
            .expect("start should succeed");
        assert!(pm.is_running().await);

        // Simulate what start_and_wait does but with a short timeout
        let health_result =
            health::wait_for_health(info.port, std::time::Duration::from_millis(500)).await;

        assert!(
            health_result.is_err(),
            "health check should fail on unused port"
        );

        // stop + build Failed state like start_and_wait would
        let _ = pm.stop().await;
        let state = ServerState::Failed {
            last_error: health_result.unwrap_err().to_string(),
            profile: "test".into(),
            since: Utc::now(),
        };
        match state {
            ServerState::Failed {
                ref last_error,
                ref profile,
                ..
            } => {
                assert!(
                    last_error.contains("timed out"),
                    "error should mention timeout, got: {last_error}"
                );
                assert_eq!(profile, "test");
            }
            other => panic!("expected Failed state, got {other:?}"),
        }
    }

    /// 12. CUDA error detection in stderr triggers watch channel
    #[tokio::test]
    async fn test_cuda_error_detection_in_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("test_cuda_error.sh");
        write_test_script(
            &script_path,
            "#!/bin/sh\necho 'CUDA error: out of memory' >&2\nsleep 300\n",
        );

        let config = make_test_config(script_path.to_str().unwrap(), 19012);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        // Subscribe to CUDA errors BEFORE starting
        let mut cuda_rx = pm.subscribe_cuda_errors();

        pm.start(&config, "test")
            .await
            .expect("start should succeed");

        // Wait for the CUDA error to be detected in stderr
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(3), cuda_rx.changed()).await;

        assert!(
            result.is_ok(),
            "CUDA error should trigger the watch channel within timeout"
        );
        assert!(
            *cuda_rx.borrow(),
            "CUDA error watch channel should be set to true"
        );

        pm.stop().await.unwrap();
    }

    /// 13. is_draining() returns false by default
    #[test]
    fn test_is_draining_default_false() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        assert!(!pm.is_draining(), "is_draining should be false by default");
    }

    /// 14. set_draining() toggles the flag correctly
    #[test]
    fn test_set_draining_toggles_flag() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        assert!(!pm.is_draining());

        pm.set_draining(true);
        assert!(
            pm.is_draining(),
            "should be draining after set_draining(true)"
        );

        pm.set_draining(false);
        assert!(
            !pm.is_draining(),
            "should not be draining after set_draining(false)"
        );

        // Toggle again to verify it's not a one-time thing
        pm.set_draining(true);
        assert!(pm.is_draining());
    }

    /// start() when already running returns an error
    #[tokio::test]
    async fn test_start_when_already_running_returns_error() {
        let (_dir, script) = make_sleep_script();
        let config = make_test_config(&script, 19015);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        pm.start(&config, "test")
            .await
            .expect("first start should succeed");
        assert!(pm.is_running().await);

        let result = pm.start(&config, "test").await;
        assert!(
            result.is_err(),
            "start on already-running should return error"
        );
        let err = result.unwrap_err();
        let err_msg = format!("{err}");
        assert!(
            err_msg.contains("already running"),
            "error should mention 'already running', got: {err_msg}"
        );

        pm.stop().await.unwrap();
    }

    /// CUDA error channel does not fire for normal (non-CUDA) stderr output
    #[tokio::test]
    async fn test_cuda_error_not_triggered_by_normal_output() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("test_normal_output.sh");
        write_test_script(
            &script_path,
            "#!/bin/sh\necho 'normal log output' >&2\necho 'all systems go' >&2\nsleep 300\n",
        );

        let config = make_test_config(script_path.to_str().unwrap(), 19016);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        let mut cuda_rx = pm.subscribe_cuda_errors();

        pm.start(&config, "test")
            .await
            .expect("start should succeed");

        // Wait briefly for stderr to be processed
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(500), cuda_rx.changed()).await;

        assert!(
            result.is_err(),
            "CUDA error channel should NOT fire for normal stderr output"
        );

        pm.stop().await.unwrap();
    }

    /// process_info() returns None when no process is running
    #[tokio::test]
    async fn test_process_info_none_when_idle() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        assert!(
            pm.process_info().await.is_none(),
            "process_info should be None when idle"
        );
    }

    /// ggml_cuda_error variant also triggers the CUDA error watch channel
    #[tokio::test]
    async fn test_ggml_cuda_error_detection_in_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("test_ggml_cuda.sh");
        write_test_script(
            &script_path,
            "#!/bin/sh\necho 'ggml_cuda_error: device kernel launch failed' >&2\nsleep 300\n",
        );

        let config = make_test_config(script_path.to_str().unwrap(), 19017);
        let log_buffer = Arc::new(LogBuffer::new(100));
        let pm = ProcessManager::new(log_buffer);

        let mut cuda_rx = pm.subscribe_cuda_errors();

        pm.start(&config, "test")
            .await
            .expect("start should succeed");

        let result =
            tokio::time::timeout(std::time::Duration::from_secs(3), cuda_rx.changed()).await;

        assert!(
            result.is_ok(),
            "ggml_cuda_error should trigger the watch channel"
        );
        assert!(*cuda_rx.borrow());

        pm.stop().await.unwrap();
    }
}

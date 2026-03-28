use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rookery_core::config::{BackendType, Config, Profile};
use rookery_core::error::{Error, Result};
use rookery_core::state::ServerState;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::logs::LogBuffer;
use crate::process::{ProcessInfo, ProcessManager};

/// Information about a running backend instance.
///
/// Captures the minimal set of fields needed for state persistence,
/// reconciliation on daemon restart, and status display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendInfo {
    /// Process ID (set for llama-server, None for container-based backends).
    pub pid: Option<u32>,

    /// Docker container ID (set for vLLM, None for native process backends).
    #[serde(default)]
    pub container_id: Option<String>,

    /// Port the backend is listening on.
    pub port: u16,

    /// Profile name that was used to start this backend.
    pub profile: String,

    /// When the backend was started.
    pub started_at: DateTime<Utc>,

    /// Which backend type is running.
    pub backend_type: BackendType,

    /// Full command line used to start the backend.
    #[serde(default)]
    pub command_line: Vec<String>,

    /// Path to the executable (for llama-server process reconciliation).
    #[serde(default)]
    pub exe_path: Option<PathBuf>,
}

/// The abstraction boundary between daemon orchestration and backend specifics.
///
/// Both `LlamaServerBackend` and `VllmBackend` implement this trait.
/// The daemon holds a single `Box<dyn InferenceBackend>` and calls these
/// methods without knowing which backend is active.
#[async_trait]
pub trait InferenceBackend: Send + Sync {
    /// Start the backend with the given config and profile name.
    ///
    /// Returns `BackendInfo` on success with all relevant metadata.
    async fn start(&self, config: &Config, profile: &str) -> Result<BackendInfo>;

    /// Stop the backend. No-op if not running.
    async fn stop(&self) -> Result<()>;

    /// Returns true if the backend is currently running.
    async fn is_running(&self) -> bool;

    /// Returns info about the running backend, or None if stopped.
    async fn process_info(&self) -> Option<BackendInfo>;

    /// Adopt an existing backend instance (used during daemon restart reconciliation).
    ///
    /// For llama-server: registers the PID for kill-by-PID on stop.
    /// For vLLM: registers the container ID for docker compose operations.
    async fn adopt(&self, info: BackendInfo) -> Result<()>;

    /// Convert the current backend state to a `ServerState` for persistence.
    async fn to_server_state(&self) -> ServerState;

    /// Returns true if the backend is in drain mode (swap in progress).
    fn is_draining(&self) -> bool;

    /// Set the drain mode flag (called by daemon during swap orchestration).
    fn set_draining(&self, draining: bool);

    /// Subscribe to error notifications (e.g., CUDA errors from stderr).
    fn subscribe_errors(&self) -> watch::Receiver<bool>;
}

// ── LlamaServerBackend ────────────────────────────────────────────────

/// Wraps the existing `ProcessManager` to implement `InferenceBackend`.
///
/// All trait methods delegate to `ProcessManager`, preserving existing behavior:
/// OOM protection, SIGTERM→SIGKILL timing, log capture, kill_on_drop(false),
/// and CUDA error detection.
pub struct LlamaServerBackend {
    process_manager: ProcessManager,
}

impl LlamaServerBackend {
    /// Create a new LlamaServerBackend with the given log buffer.
    pub fn new(log_buffer: Arc<LogBuffer>) -> Self {
        Self {
            process_manager: ProcessManager::new(log_buffer),
        }
    }

    /// Access the underlying ProcessManager (for daemon code that still needs it).
    pub fn process_manager(&self) -> &ProcessManager {
        &self.process_manager
    }
}

/// Convert a `ProcessInfo` (engine-internal) to a `BackendInfo` (trait-level).
fn process_info_to_backend_info(info: &ProcessInfo) -> BackendInfo {
    BackendInfo {
        pid: Some(info.pid),
        container_id: None,
        port: info.port,
        profile: info.profile.clone(),
        started_at: info.started_at,
        backend_type: BackendType::LlamaServer,
        command_line: info.command_line.clone(),
        exe_path: Some(info.exe_path.clone()),
    }
}

/// Convert a `BackendInfo` (trait-level) to a `ProcessInfo` (engine-internal).
///
/// Returns `Err` if `pid` is `None`, since LlamaServer backend requires a valid PID.
fn backend_info_to_process_info(info: &BackendInfo) -> Result<ProcessInfo> {
    let pid = info.pid.ok_or_else(|| {
        Error::ConfigValidation("pid required for LlamaServer BackendInfo".into())
    })?;
    Ok(ProcessInfo {
        pid,
        port: info.port,
        profile: info.profile.clone(),
        started_at: info.started_at,
        command_line: info.command_line.clone(),
        exe_path: info.exe_path.clone().unwrap_or_default(),
    })
}

#[async_trait]
impl InferenceBackend for LlamaServerBackend {
    async fn start(&self, config: &Config, profile: &str) -> Result<BackendInfo> {
        let info = self.process_manager.start(config, profile).await?;
        Ok(process_info_to_backend_info(&info))
    }

    async fn stop(&self) -> Result<()> {
        self.process_manager.stop().await
    }

    async fn is_running(&self) -> bool {
        self.process_manager.is_running().await
    }

    async fn process_info(&self) -> Option<BackendInfo> {
        self.process_manager
            .process_info()
            .await
            .as_ref()
            .map(process_info_to_backend_info)
    }

    async fn adopt(&self, info: BackendInfo) -> Result<()> {
        let process_info = backend_info_to_process_info(&info)?;
        self.process_manager.adopt(process_info).await;
        Ok(())
    }

    async fn to_server_state(&self) -> ServerState {
        self.process_manager.to_server_state().await
    }

    fn is_draining(&self) -> bool {
        self.process_manager.is_draining()
    }

    fn set_draining(&self, draining: bool) {
        self.process_manager.set_draining(draining);
    }

    fn subscribe_errors(&self) -> watch::Receiver<bool> {
        self.process_manager.subscribe_cuda_errors()
    }
}

// ── Backend factory ──────────────────────────────────────────────────

/// Create the appropriate backend implementation based on the profile's backend type.
///
/// Returns `LlamaServerBackend` for `LlamaServer` profiles.
/// Returns `Err` for `Vllm` profiles until `VllmBackend` is implemented.
pub fn create_backend(
    profile: &Profile,
    log_buffer: Arc<LogBuffer>,
) -> Result<Box<dyn InferenceBackend>> {
    match profile.backend_type() {
        BackendType::LlamaServer => Ok(Box::new(LlamaServerBackend::new(log_buffer))),
        BackendType::Vllm => Err(Error::ConfigValidation(
            "vLLM backend not yet implemented".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === VAL-TRAIT-001: InferenceBackend is object-safe, Send + Sync ===
    #[test]
    fn test_trait_is_object_safe_send_sync() {
        // This test verifies at compile time that InferenceBackend can be used
        // as a trait object with Send + Sync bounds.
        fn _assert_object_safe(_: Box<dyn InferenceBackend>) {}
        fn _assert_send_sync<T: Send + Sync>() {}
        // If this compiles, the trait is object-safe and the bounds are satisfied.
        // Box<dyn InferenceBackend> is Send + Sync because the trait requires Send + Sync.
    }

    // === BackendInfo serde roundtrip ===
    #[test]
    fn test_backend_info_serde_roundtrip() {
        let info = BackendInfo {
            pid: Some(12345),
            container_id: None,
            port: 8081,
            profile: "fast".into(),
            started_at: Utc::now(),
            backend_type: BackendType::LlamaServer,
            command_line: vec!["llama-server".into(), "-ngl".into(), "99".into()],
            exe_path: Some(PathBuf::from("/usr/bin/llama-server")),
        };

        let json = serde_json::to_string(&info).unwrap();
        let restored: BackendInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.pid, Some(12345));
        assert_eq!(restored.container_id, None);
        assert_eq!(restored.port, 8081);
        assert_eq!(restored.profile, "fast");
        assert_eq!(restored.backend_type, BackendType::LlamaServer);
        assert_eq!(restored.command_line.len(), 3);
        assert_eq!(
            restored.exe_path,
            Some(PathBuf::from("/usr/bin/llama-server"))
        );
    }

    // === BackendInfo with vLLM fields ===
    #[test]
    fn test_backend_info_vllm_serde() {
        let info = BackendInfo {
            pid: None,
            container_id: Some("abc123def456".into()),
            port: 8081,
            profile: "vllm_prod".into(),
            started_at: Utc::now(),
            backend_type: BackendType::Vllm,
            command_line: vec!["--model".into(), "test/model".into()],
            exe_path: None,
        };

        let json = serde_json::to_string(&info).unwrap();
        let restored: BackendInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.pid, None);
        assert_eq!(restored.container_id.as_deref(), Some("abc123def456"));
        assert_eq!(restored.backend_type, BackendType::Vllm);
        assert_eq!(restored.exe_path, None);
    }

    // === BackendInfo backward compat: missing optional fields ===
    #[test]
    fn test_backend_info_deserialize_missing_optional_fields() {
        // Simulate a minimal JSON without optional fields
        let json = r#"{
            "pid": 1234,
            "port": 8081,
            "profile": "fast",
            "started_at": "2025-01-01T00:00:00Z",
            "backend_type": "llama_server"
        }"#;
        let info: BackendInfo = serde_json::from_str(json).unwrap();

        assert_eq!(info.pid, Some(1234));
        assert_eq!(info.container_id, None);
        assert_eq!(info.port, 8081);
        assert_eq!(info.profile, "fast");
        assert_eq!(info.backend_type, BackendType::LlamaServer);
        assert!(info.command_line.is_empty());
        assert_eq!(info.exe_path, None);
    }

    // === VAL-TRAIT-006: BackendType default is LlamaServer ===
    #[test]
    fn test_backend_type_default_is_llama_server() {
        assert_eq!(BackendType::default(), BackendType::LlamaServer);
    }

    // === VAL-TRAIT-002: LlamaServerBackend implements InferenceBackend ===
    #[test]
    fn test_llama_server_backend_implements_trait() {
        // Compile-time check: LlamaServerBackend can be used as Box<dyn InferenceBackend>
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);
        let _boxed: Box<dyn InferenceBackend> = Box::new(backend);
        // If this compiles, the trait is fully implemented.
    }

    // === VAL-TRAIT-004: stop() is no-op when idle (no process running) ===
    #[tokio::test]
    async fn test_llama_server_backend_stop_noop_when_idle() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        // stop() on a fresh backend should be a no-op and return Ok
        let result = backend.stop().await;
        assert!(result.is_ok(), "stop() should be no-op when idle");
    }

    // === VAL-TRAIT-004 (continued): is_running() is false when idle ===
    #[tokio::test]
    async fn test_llama_server_backend_not_running_when_idle() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        assert!(
            !backend.is_running().await,
            "should not be running when idle"
        );
    }

    // === process_info is None when idle ===
    #[tokio::test]
    async fn test_llama_server_backend_process_info_none_when_idle() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        assert!(
            backend.process_info().await.is_none(),
            "process_info should be None when idle"
        );
    }

    // === to_server_state returns Stopped when idle ===
    #[tokio::test]
    async fn test_llama_server_backend_to_server_state_stopped_when_idle() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        let state = backend.to_server_state().await;
        assert!(
            matches!(state, ServerState::Stopped),
            "should be Stopped when idle, got {state:?}"
        );
    }

    // === is_draining defaults to false ===
    #[test]
    fn test_llama_server_backend_not_draining_by_default() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        assert!(!backend.is_draining(), "should not be draining by default");
    }

    // === set_draining toggles drain flag ===
    #[test]
    fn test_llama_server_backend_set_draining() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        assert!(!backend.is_draining(), "should start non-draining");
        backend.set_draining(true);
        assert!(
            backend.is_draining(),
            "should be draining after set_draining(true)"
        );
        backend.set_draining(false);
        assert!(
            !backend.is_draining(),
            "should not be draining after set_draining(false)"
        );
    }

    // === subscribe_errors returns a valid receiver ===
    #[test]
    fn test_llama_server_backend_subscribe_errors() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        let rx = backend.subscribe_errors();
        // Initial value should be false (no errors)
        assert!(!*rx.borrow(), "initial error state should be false");
    }

    // === VAL-TRAIT-005: adopt() registers PID for orphan recovery ===
    #[tokio::test]
    async fn test_llama_server_backend_adopt() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        // Adopt info for the current process (which is alive)
        let info = BackendInfo {
            pid: Some(std::process::id()),
            container_id: None,
            port: 8081,
            profile: "test_profile".into(),
            started_at: Utc::now(),
            backend_type: BackendType::LlamaServer,
            command_line: vec!["llama-server".into()],
            exe_path: Some(PathBuf::from("/usr/bin/llama-server")),
        };

        let result = backend.adopt(info.clone()).await;
        assert!(result.is_ok(), "adopt should succeed");

        // After adopt, process_info should return the adopted info
        let adopted = backend.process_info().await;
        assert!(adopted.is_some(), "should have process_info after adopt");
        let adopted = adopted.unwrap();
        assert_eq!(adopted.pid, Some(std::process::id()));
        assert_eq!(adopted.port, 8081);
        assert_eq!(adopted.profile, "test_profile");
        assert_eq!(adopted.backend_type, BackendType::LlamaServer);
        assert_eq!(adopted.container_id, None);
    }

    // === VAL-TRAIT-005 (continued): adopt makes is_running() true when PID alive ===
    #[tokio::test]
    async fn test_llama_server_backend_adopt_makes_running() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        // Adopt our own PID (definitely alive)
        let info = BackendInfo {
            pid: Some(std::process::id()),
            container_id: None,
            port: 8081,
            profile: "test".into(),
            started_at: Utc::now(),
            backend_type: BackendType::LlamaServer,
            command_line: vec![],
            exe_path: Some(PathBuf::from("/test")),
        };

        backend.adopt(info).await.unwrap();
        assert!(
            backend.is_running().await,
            "should be running after adopting a live PID"
        );
    }

    // === VAL-TRAIT-005 (continued): adopt with dead PID makes is_running() false ===
    #[tokio::test]
    async fn test_llama_server_backend_adopt_dead_pid_not_running() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        // Adopt a non-existent PID
        let info = BackendInfo {
            pid: Some(999_999_999),
            container_id: None,
            port: 8081,
            profile: "test".into(),
            started_at: Utc::now(),
            backend_type: BackendType::LlamaServer,
            command_line: vec![],
            exe_path: Some(PathBuf::from("/test")),
        };

        backend.adopt(info).await.unwrap();
        assert!(
            !backend.is_running().await,
            "should NOT be running with a dead PID"
        );
    }

    // === VAL-TRAIT-003: start() returns BackendInfo with correct fields ===
    // This test starts an actual process (using /bin/sleep as a stand-in for llama-server).
    // It verifies the BackendInfo fields are populated correctly.
    #[tokio::test]
    async fn test_llama_server_backend_start_returns_correct_backend_info() {
        use rookery_core::config::{Config, Model, Profile};
        use std::collections::HashMap;

        // Create a config that uses /bin/sleep as a fake binary
        let config = Config {
            llama_server: PathBuf::from("/bin/sleep"),
            default_profile: "test".into(),
            listen: "127.0.0.1:19999".parse().unwrap(),
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
                    port: 19999,
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
                    extra_args: vec![],
                },
            )]),
            agents: HashMap::new(),
        };

        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        let result = backend.start(&config, "test").await;
        assert!(result.is_ok(), "start should succeed: {:?}", result.err());

        let info = result.unwrap();

        // Verify BackendInfo fields
        assert!(info.pid.is_some(), "pid should be set for llama-server");
        assert!(info.pid.unwrap() > 0, "pid should be positive");
        assert_eq!(info.container_id, None, "container_id should be None");
        assert_eq!(info.port, 19999);
        assert_eq!(info.profile, "test");
        assert_eq!(info.backend_type, BackendType::LlamaServer);
        assert!(
            !info.command_line.is_empty(),
            "command_line should be populated"
        );
        assert_eq!(info.exe_path, Some(PathBuf::from("/bin/sleep")));

        // is_running should be true now
        assert!(backend.is_running().await, "should be running after start");

        // Clean up: stop the process
        backend.stop().await.unwrap();
        assert!(
            !backend.is_running().await,
            "should not be running after stop"
        );
    }

    // === VAL-TRAIT-010: create_backend() returns correct backend type based on profile ===
    #[test]
    fn test_create_backend_llama_server_profile() {
        use rookery_core::config::Profile;

        let log_buffer = Arc::new(LogBuffer::new(100));

        // A profile with no vllm sub-table → LlamaServer
        let profile = Profile {
            model: "test".into(),
            port: 8081,
            llama_server: None,
            vllm: None,
            ctx_size: 4096,
            threads: 4,
            threads_batch: 24,
            batch_size: 4096,
            ubatch_size: 1024,
            gpu_layers: -1,
            gpu_index: None,
            cache_type_k: "q8_0".into(),
            cache_type_v: "q8_0".into(),
            flash_attention: true,
            reasoning_budget: 0,
            chat_template: None,
            temp: 0.7,
            top_p: 0.8,
            top_k: 20,
            min_p: 0.0,
            extra_args: vec![],
        };

        // Should return Ok with LlamaServerBackend
        let backend =
            create_backend(&profile, log_buffer).expect("should succeed for llama-server");
        // We can verify it works by checking initial state
        assert!(!backend.is_draining());
    }

    // === VAL-TRAIT-010: create_backend() returns Err for Vllm profile (not yet implemented) ===
    #[test]
    fn test_create_backend_vllm_profile_returns_err() {
        use rookery_core::config::{Profile, VllmConfig};

        let log_buffer = Arc::new(LogBuffer::new(100));

        let profile = Profile {
            model: "test".into(),
            port: 8081,
            llama_server: None,
            vllm: Some(VllmConfig {
                docker_image: "vllm/vllm-openai:latest".into(),
                gpu_memory_utilization: 0.9,
                max_num_seqs: None,
                max_num_batched_tokens: None,
                max_model_len: None,
                quantization: None,
                tool_call_parser: None,
                kv_cache_dtype: None,
                extra_args: vec![],
            }),
            ctx_size: 4096,
            threads: 4,
            threads_batch: 24,
            batch_size: 4096,
            ubatch_size: 1024,
            gpu_layers: -1,
            gpu_index: None,
            cache_type_k: "q8_0".into(),
            cache_type_v: "q8_0".into(),
            flash_attention: true,
            reasoning_budget: 0,
            chat_template: None,
            temp: 0.7,
            top_p: 0.8,
            top_k: 20,
            min_p: 0.0,
            extra_args: vec![],
        };

        // Should return Err with ConfigValidation
        let result = create_backend(&profile, log_buffer);
        assert!(result.is_err(), "should return Err for vLLM profile");
        let err = result.err().unwrap();
        assert!(
            err.to_string().contains("vLLM backend not yet implemented"),
            "error should mention vLLM not implemented, got: {err}"
        );
    }

    // === Conversion helpers: process_info <-> backend_info roundtrip ===
    #[test]
    fn test_process_info_to_backend_info_conversion() {
        let pinfo = ProcessInfo {
            pid: 42,
            port: 8081,
            profile: "fast".into(),
            started_at: Utc::now(),
            command_line: vec!["llama-server".into(), "--port".into(), "8081".into()],
            exe_path: PathBuf::from("/usr/bin/llama-server"),
        };

        let binfo = process_info_to_backend_info(&pinfo);
        assert_eq!(binfo.pid, Some(42));
        assert_eq!(binfo.container_id, None);
        assert_eq!(binfo.port, 8081);
        assert_eq!(binfo.profile, "fast");
        assert_eq!(binfo.backend_type, BackendType::LlamaServer);
        assert_eq!(binfo.command_line, pinfo.command_line);
        assert_eq!(binfo.exe_path, Some(pinfo.exe_path));
    }

    #[test]
    fn test_backend_info_to_process_info_conversion() {
        let binfo = BackendInfo {
            pid: Some(42),
            container_id: None,
            port: 8081,
            profile: "fast".into(),
            started_at: Utc::now(),
            backend_type: BackendType::LlamaServer,
            command_line: vec!["llama-server".into()],
            exe_path: Some(PathBuf::from("/usr/bin/llama-server")),
        };

        let pinfo = backend_info_to_process_info(&binfo).expect("should succeed with pid present");
        assert_eq!(pinfo.pid, 42);
        assert_eq!(pinfo.port, 8081);
        assert_eq!(pinfo.profile, "fast");
        assert_eq!(pinfo.command_line, binfo.command_line);
        assert_eq!(pinfo.exe_path, PathBuf::from("/usr/bin/llama-server"));
    }

    #[test]
    fn test_backend_info_to_process_info_missing_pid_returns_err() {
        let binfo = BackendInfo {
            pid: None,
            container_id: None,
            port: 8081,
            profile: "test".into(),
            started_at: Utc::now(),
            backend_type: BackendType::LlamaServer,
            command_line: vec![],
            exe_path: None,
        };

        let result = backend_info_to_process_info(&binfo);
        assert!(result.is_err(), "should return Err when pid is None");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("pid required"),
            "error should mention pid required, got: {err}"
        );
    }

    // === VAL-TRAIT-004: stop() after adopt() with valid PID completes without error ===
    //
    // After adopting a process by PID (no child handle), stop() should complete
    // successfully using the kill-by-PID fallback path. This tests the orphan
    // stop scenario where the daemon restarted and adopted a running process.
    #[tokio::test]
    async fn test_stop_after_adopt_completes_ok() {
        // Spawn a real process (sleep 60) so we can adopt and stop it
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .expect("failed to spawn sleep process");
        let pid = child.id().expect("no PID for child");

        // Drop the child handle — simulates daemon restart where we lose the handle
        drop(child);

        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        let info = BackendInfo {
            pid: Some(pid),
            container_id: None,
            port: 8081,
            profile: "test".into(),
            started_at: Utc::now(),
            backend_type: BackendType::LlamaServer,
            command_line: vec!["sleep".into(), "60".into()],
            exe_path: Some(PathBuf::from("/bin/sleep")),
        };

        backend.adopt(info).await.unwrap();
        assert!(
            backend.is_running().await,
            "adopted process should be running"
        );

        // stop() should complete without error using kill-by-PID path
        let result = backend.stop().await;
        assert!(
            result.is_ok(),
            "stop() after adopt should succeed: {:?}",
            result.err()
        );

        // After stop, should no longer be running
        assert!(
            !backend.is_running().await,
            "should not be running after stop"
        );
        assert!(
            backend.process_info().await.is_none(),
            "process_info should be None after stop"
        );
    }

    // === VAL-TRAIT-005: adopt stores info, process_info returns it, stop uses kill-by-PID ===
    //
    // Comprehensive test for the orphan recovery flow:
    // 1. adopt() stores the BackendInfo (no child handle)
    // 2. process_info() returns the adopted info with correct fields
    // 3. stop() falls back to kill-by-PID (SIGTERM→wait→SIGKILL) since there's no child handle
    #[tokio::test]
    async fn test_adopt_stores_info_and_stop_uses_pid_kill() {
        // Spawn a real process to adopt
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .expect("failed to spawn sleep process");
        let pid = child.id().expect("no PID for child");

        // Drop child handle — this is the key: after adopt, stop() must use kill-by-PID
        drop(child);

        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        let started_at = Utc::now();
        let info = BackendInfo {
            pid: Some(pid),
            container_id: None,
            port: 9090,
            profile: "adopted_profile".into(),
            started_at,
            backend_type: BackendType::LlamaServer,
            command_line: vec!["sleep".into(), "60".into()],
            exe_path: Some(PathBuf::from("/bin/sleep")),
        };

        // 1. adopt() stores the info
        backend.adopt(info).await.unwrap();

        // 2. process_info() returns the adopted info
        let adopted = backend
            .process_info()
            .await
            .expect("should have info after adopt");
        assert_eq!(adopted.pid, Some(pid));
        assert_eq!(adopted.port, 9090);
        assert_eq!(adopted.profile, "adopted_profile");
        assert_eq!(adopted.backend_type, BackendType::LlamaServer);
        assert_eq!(adopted.container_id, None);

        // Process should be alive
        let proc_path = std::path::PathBuf::from(format!("/proc/{pid}"));
        assert!(proc_path.exists(), "process should be alive before stop");

        // 3. stop() uses kill-by-PID (no child handle available)
        backend.stop().await.unwrap();

        // Give the OS a moment to clean up
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Process should be dead
        assert!(
            !proc_path.exists(),
            "process should be dead after stop via PID kill"
        );
        assert!(
            !backend.is_running().await,
            "should not be running after stop"
        );
        assert!(
            backend.process_info().await.is_none(),
            "info cleared after stop"
        );
    }

    // === VAL-CROSS-005: Canary-relevant trait methods work on LlamaServerBackend ===
    //
    // The inference canary uses three trait methods: to_server_state(), is_draining(),
    // and subscribe_errors(). This test verifies all three work correctly on
    // LlamaServerBackend, which is the foundation for the canary integration.
    #[tokio::test]
    async fn test_canary_trait_methods_on_llama_server_backend() {
        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend = LlamaServerBackend::new(log_buffer);

        // to_server_state() — canary checks this to determine if backend is Running
        let state = backend.to_server_state().await;
        assert!(
            matches!(state, ServerState::Stopped),
            "idle backend should report Stopped to canary"
        );

        // is_draining() — canary skips health checks during drain
        assert!(
            !backend.is_draining(),
            "fresh backend should not be draining (canary would skip checks)"
        );

        // set_draining works correctly for canary drain awareness
        backend.set_draining(true);
        assert!(
            backend.is_draining(),
            "canary should see draining=true during swap"
        );
        backend.set_draining(false);
        assert!(
            !backend.is_draining(),
            "canary should see draining=false after swap completes"
        );

        // subscribe_errors() — canary uses this to detect CUDA errors
        let rx = backend.subscribe_errors();
        assert!(
            !*rx.borrow(),
            "initial error state should be false (no CUDA errors for canary)"
        );

        // The receiver should be a valid watch channel that can be polled
        // (canary polls this in its loop to trigger immediate restart on CUDA error)
        let rx2 = backend.subscribe_errors();
        assert!(
            !*rx2.borrow(),
            "multiple subscribers should all see false initially"
        );
    }
}

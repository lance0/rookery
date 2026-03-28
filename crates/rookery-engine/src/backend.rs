use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rookery_core::config::{BackendType, Config, Profile};
use rookery_core::error::Result;
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
/// Requires `pid` to be `Some` and `exe_path` to be `Some`; falls back to
/// defaults if missing (pid=0, exe_path="" — these cases only arise from
/// malformed adoption data).
fn backend_info_to_process_info(info: &BackendInfo) -> ProcessInfo {
    ProcessInfo {
        pid: info.pid.unwrap_or(0),
        port: info.port,
        profile: info.profile.clone(),
        started_at: info.started_at,
        command_line: info.command_line.clone(),
        exe_path: info.exe_path.clone().unwrap_or_default(),
    }
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
        let process_info = backend_info_to_process_info(&info);
        self.process_manager.adopt(process_info).await;
        Ok(())
    }

    async fn to_server_state(&self) -> ServerState {
        self.process_manager.to_server_state().await
    }

    fn is_draining(&self) -> bool {
        self.process_manager.is_draining()
    }

    fn subscribe_errors(&self) -> watch::Receiver<bool> {
        self.process_manager.subscribe_cuda_errors()
    }
}

// ── Backend factory ──────────────────────────────────────────────────

/// Create the appropriate backend implementation based on the profile's backend type.
///
/// Returns `LlamaServerBackend` for `LlamaServer` profiles.
/// VllmBackend for `Vllm` profiles will be added in a later milestone.
pub fn create_backend(profile: &Profile, log_buffer: Arc<LogBuffer>) -> Box<dyn InferenceBackend> {
    match profile.backend_type() {
        BackendType::LlamaServer => Box::new(LlamaServerBackend::new(log_buffer)),
        BackendType::Vllm => {
            // VllmBackend will be implemented in the vllm-backend milestone.
            // For now, panic with a clear message since vLLM profiles shouldn't
            // reach this code path until the backend is implemented.
            unimplemented!("VllmBackend not yet implemented — coming in the vllm-backend milestone")
        }
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

        // Should not panic — returns LlamaServerBackend
        let backend = create_backend(&profile, log_buffer);
        // We can verify it works by checking initial state
        assert!(!backend.is_draining());
    }

    // === VAL-TRAIT-010: create_backend() panics for Vllm profile (not yet implemented) ===
    #[test]
    #[should_panic(expected = "VllmBackend not yet implemented")]
    fn test_create_backend_vllm_profile_unimplemented() {
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

        // Should panic with unimplemented message
        let _backend = create_backend(&profile, log_buffer);
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

        let pinfo = backend_info_to_process_info(&binfo);
        assert_eq!(pinfo.pid, 42);
        assert_eq!(pinfo.port, 8081);
        assert_eq!(pinfo.profile, "fast");
        assert_eq!(pinfo.command_line, binfo.command_line);
        assert_eq!(pinfo.exe_path, PathBuf::from("/usr/bin/llama-server"));
    }

    #[test]
    fn test_backend_info_to_process_info_missing_pid_defaults() {
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

        let pinfo = backend_info_to_process_info(&binfo);
        assert_eq!(pinfo.pid, 0, "missing pid should default to 0");
        assert_eq!(
            pinfo.exe_path,
            PathBuf::new(),
            "missing exe_path should default to empty"
        );
    }
}

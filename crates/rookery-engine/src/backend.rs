use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rookery_core::config::{BackendType, Config};
use rookery_core::error::Result;
use rookery_core::state::ServerState;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

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
}

//! Shared test utilities for rookery-daemon route and SSE tests.
//!
//! Provides an `AppStateBuilder` that constructs a valid `AppState` with
//! a mock backend, real LogBuffer, tempdir-based StatePersistence, and
//! all required fields for route handler testing.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rookery_core::config::{BackendType, Config, Model, Profile};
use rookery_core::error::Result;
use rookery_core::state::{ServerState, StatePersistence};
use rookery_engine::agent::AgentManager;
use rookery_engine::backend::{BackendInfo, InferenceBackend};
use rookery_engine::hardware::{CpuProfile, HardwareProfile};
use rookery_engine::logs::LogBuffer;
use rookery_engine::models::HfClient;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, RwLock, broadcast, watch};

use crate::app_state::AppState;

/// A mock implementation of `InferenceBackend` for testing daemon routes.
///
/// All methods are configurable via internal state. By default:
/// - `is_running()` returns false
/// - `to_server_state()` returns `Stopped`
/// - `stop()` returns `Ok(())`
/// - `is_draining()` returns false
pub struct MockBackend {
    running: AtomicBool,
    draining: AtomicBool,
    info: Mutex<Option<BackendInfo>>,
    cuda_error_tx: watch::Sender<bool>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    pub fn new() -> Self {
        let (cuda_error_tx, _) = watch::channel(false);
        Self {
            running: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            info: Mutex::new(None),
            cuda_error_tx,
        }
    }

    /// Create a mock backend that reports as running with the given info.
    pub fn running_with(info: BackendInfo) -> Self {
        let (cuda_error_tx, _) = watch::channel(false);
        Self {
            running: AtomicBool::new(true),
            draining: AtomicBool::new(false),
            info: Mutex::new(Some(info)),
            cuda_error_tx,
        }
    }
}

#[async_trait]
impl InferenceBackend for MockBackend {
    async fn start(&self, _config: &Config, profile: &str) -> Result<BackendInfo> {
        let info = BackendInfo {
            pid: Some(99999),
            container_id: None,
            port: 0,
            profile: profile.to_string(),
            started_at: chrono::Utc::now(),
            backend_type: BackendType::LlamaServer,
            command_line: vec!["mock-server".into()],
            exe_path: Some(PathBuf::from("/mock/llama-server")),
        };
        self.running.store(true, Ordering::SeqCst);
        *self.info.lock().await = Some(info.clone());
        Ok(info)
    }

    async fn stop(&self) -> Result<()> {
        self.running.store(false, Ordering::SeqCst);
        *self.info.lock().await = None;
        Ok(())
    }

    async fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    async fn process_info(&self) -> Option<BackendInfo> {
        self.info.lock().await.clone()
    }

    async fn adopt(&self, info: BackendInfo) -> Result<()> {
        self.running.store(true, Ordering::SeqCst);
        *self.info.lock().await = Some(info);
        Ok(())
    }

    async fn to_server_state(&self) -> ServerState {
        if self.running.load(Ordering::SeqCst)
            && let Some(info) = self.info.lock().await.as_ref()
        {
            return ServerState::Running {
                profile: info.profile.clone(),
                pid: info.pid.unwrap_or(0),
                port: info.port,
                since: info.started_at,
                command_line: info.command_line.clone(),
                exe_path: info.exe_path.clone(),
                backend_type: info.backend_type,
                container_id: info.container_id.clone(),
            };
        }
        ServerState::Stopped
    }

    fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    fn set_draining(&self, draining: bool) {
        self.draining.store(draining, Ordering::SeqCst);
    }

    fn subscribe_errors(&self) -> watch::Receiver<bool> {
        self.cuda_error_tx.subscribe()
    }
}

/// Build a test `AppState` with sensible defaults.
///
/// Uses:
/// - A `MockBackend` (or custom `Box<dyn InferenceBackend>`)
/// - A real `LogBuffer` (capacity 100)
/// - A tempdir-based `StatePersistence`
/// - A minimal test `Config` with one profile
/// - An `AgentManager` with no agents
/// - No GPU monitor
/// - A dummy `HardwareProfile`
///
/// # Example
///
/// ```ignore
/// let (_dir, state) = build_test_app_state(None);
/// // Use `state` with axum route handlers
/// ```
pub fn build_test_app_state(
    backend: Option<Box<dyn InferenceBackend>>,
) -> (tempfile::TempDir, Arc<AppState>) {
    let dir = tempfile::tempdir().expect("failed to create tempdir for test state");
    let state_path = dir.path().join("state.json");

    let log_buffer = Arc::new(LogBuffer::new(100));
    let agent_persistence = rookery_core::state::AgentPersistence {
        path: dir.path().join("agents.json"),
    };
    let agent_manager = Arc::new(AgentManager::with_persistence(
        log_buffer.clone(),
        agent_persistence,
    ));

    let backend: Box<dyn InferenceBackend> =
        backend.unwrap_or_else(|| Box::new(MockBackend::new()));

    let config = Config {
        llama_server: PathBuf::from("/mock/llama-server"),
        default_profile: "test".into(),
        listen: "127.0.0.1:19876".parse().unwrap(),
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
                port: 19876,
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

    let (state_tx, _) = broadcast::channel(16);

    // Create a StatePersistence pointing at the tempdir
    let state_persistence = StatePersistence { path: state_path };

    let hardware_profile = HardwareProfile {
        gpu: None,
        cpu: CpuProfile {
            name: "test-cpu".into(),
            cores: 4,
            threads: 8,
            ram_total_mb: 16384,
        },
    };

    let config_path = dir.path().join("config.toml");
    let app_state = Arc::new(AppState {
        config_path,
        config: Arc::new(RwLock::new(config)),
        backend: Arc::new(tokio::sync::Mutex::new(backend)),
        agent_manager,
        gpu_monitor: None,
        log_buffer,
        state_persistence,
        state_tx,
        op_lock: Mutex::new(()),
        hf_client: HfClient::new(),
        hardware_profile,
    });

    (dir, app_state)
}

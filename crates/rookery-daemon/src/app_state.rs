use rookery_core::config::Config;
use rookery_core::state::StatePersistence;
use rookery_engine::agent::AgentManager;
use rookery_engine::backend::InferenceBackend;
use rookery_engine::gpu::GpuMonitor;
use rookery_engine::hardware::HardwareProfile;
use rookery_engine::logs::LogBuffer;
use rookery_engine::models::HfClient;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, broadcast};

pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    pub backend: Arc<tokio::sync::Mutex<Box<dyn InferenceBackend>>>,
    pub agent_manager: Arc<AgentManager>,
    pub gpu_monitor: Option<GpuMonitor>,
    pub log_buffer: Arc<LogBuffer>,
    pub state_persistence: StatePersistence,
    pub state_tx: broadcast::Sender<serde_json::Value>,
    pub op_lock: Mutex<()>,
    pub hf_client: HfClient,
    pub hardware_profile: HardwareProfile,
}

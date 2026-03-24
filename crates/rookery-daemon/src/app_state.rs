use rookery_core::config::Config;
use rookery_core::state::StatePersistence;
use rookery_engine::agent::AgentManager;
use rookery_engine::gpu::GpuMonitor;
use rookery_engine::hardware::HardwareProfile;
use rookery_engine::logs::LogBuffer;
use rookery_engine::models::HfClient;
use rookery_engine::process::ProcessManager;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};

pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    pub process_manager: ProcessManager,
    pub agent_manager: AgentManager,
    pub gpu_monitor: Option<GpuMonitor>,
    pub log_buffer: Arc<LogBuffer>,
    pub state_persistence: StatePersistence,
    pub state_tx: broadcast::Sender<serde_json::Value>,
    pub op_lock: Mutex<()>,
    pub hf_client: HfClient,
    pub hardware_profile: HardwareProfile,
}

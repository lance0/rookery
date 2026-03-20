use rookery_core::config::Config;
use rookery_core::state::StatePersistence;
use rookery_engine::gpu::GpuMonitor;
use rookery_engine::logs::LogBuffer;
use rookery_engine::agent::AgentManager;
use rookery_engine::process::ProcessManager;
use std::sync::Arc;

pub struct AppState {
    pub config: Config,
    pub process_manager: ProcessManager,
    pub agent_manager: AgentManager,
    pub gpu_monitor: Option<GpuMonitor>,
    pub log_buffer: Arc<LogBuffer>,
    pub state_persistence: StatePersistence,
}

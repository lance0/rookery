use rookery_core::config::Config;
use rookery_core::state::{ServerState, StatePersistence};
use rookery_engine::agent::AgentManager;
use rookery_engine::backend::InferenceBackend;
use rookery_engine::gpu::GpuMonitor;
use rookery_engine::hardware::HardwareProfile;
use rookery_engine::logs::LogBuffer;
use rookery_engine::models::HfClient;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::metrics::RuntimeMetrics;

#[derive(Debug)]
pub enum StartServerError {
    Start(String),
    Health(String),
}

impl std::fmt::Display for StartServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start(msg) | Self::Health(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for StartServerError {}

pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    pub config_path: PathBuf,
    pub backend: Arc<tokio::sync::Mutex<Box<dyn InferenceBackend>>>,
    pub agent_manager: Arc<AgentManager>,
    pub metrics: Arc<RuntimeMetrics>,
    pub gpu_monitor: Option<GpuMonitor>,
    pub log_buffer: Arc<LogBuffer>,
    pub state_persistence: StatePersistence,
    pub server_state: RwLock<ServerState>,
    pub state_tx: broadcast::Sender<serde_json::Value>,
    pub last_inference_at: AtomicI64,
    pub op_lock: Mutex<()>,
    pub hf_client: HfClient,
    pub hardware_profile: HardwareProfile,
}

impl AppState {
    pub async fn current_state(&self) -> ServerState {
        self.server_state.read().await.clone()
    }

    pub async fn set_server_state(&self, server_state: ServerState) {
        *self.server_state.write().await = server_state.clone();
        let _ = self.state_persistence.save(&server_state);
        let json = crate::routes::status_json_from_state(&server_state);
        let _ = self.state_tx.send(json);
    }

    pub fn record_inference_activity(&self) {
        self.last_inference_at
            .store(chrono::Utc::now().timestamp(), Ordering::SeqCst);
    }

    pub fn last_inference_at(&self) -> i64 {
        self.last_inference_at.load(Ordering::SeqCst)
    }

    pub async fn start_profile(
        &self,
        profile_name: &str,
        record_activity: bool,
    ) -> Result<ServerState, StartServerError> {
        let starting_state = ServerState::Starting {
            profile: profile_name.to_string(),
            since: chrono::Utc::now(),
        };
        self.set_server_state(starting_state).await;

        let config = self.config.read().await;
        let backend = self.backend.lock().await;
        if let Err(e) = backend.start(&config, profile_name).await {
            drop(backend);
            drop(config);
            let failed = ServerState::Failed {
                last_error: e.to_string(),
                profile: profile_name.to_string(),
                since: chrono::Utc::now(),
            };
            self.set_server_state(failed).await;
            self.agent_manager.set_dependency_bounce_suppressed(false);
            return Err(StartServerError::Start(e.to_string()));
        }

        let port = config
            .profiles
            .get(profile_name)
            .map(|p| p.port)
            .unwrap_or(8081);
        drop(backend);
        drop(config);

        match rookery_engine::health::wait_for_health(port, std::time::Duration::from_secs(120))
            .await
        {
            Ok(()) => {
                let server_state = self.backend.lock().await.to_server_state().await;
                self.set_server_state(server_state.clone()).await;
                if server_state.is_running() {
                    self.metrics.inc_server_restart();
                    if record_activity {
                        self.record_inference_activity();
                    }
                }
                self.agent_manager.set_dependency_bounce_suppressed(false);
                Ok(server_state)
            }
            Err(e) => {
                let _ = self.backend.lock().await.stop().await;
                let failed = ServerState::Failed {
                    last_error: e.to_string(),
                    profile: profile_name.to_string(),
                    since: chrono::Utc::now(),
                };
                self.set_server_state(failed).await;
                self.agent_manager.set_dependency_bounce_suppressed(false);
                Err(StartServerError::Health(e.to_string()))
            }
        }
    }

    pub async fn sleep_server(&self) -> Result<ServerState, String> {
        let current = self.current_state().await;
        let profile = match current {
            ServerState::Running { profile, .. } => profile,
            ServerState::Sleeping { .. } => return Ok(current),
            _ => return Err("server is not running".into()),
        };

        self.agent_manager.set_dependency_bounce_suppressed(true);
        self.backend
            .lock()
            .await
            .stop()
            .await
            .map_err(|e| e.to_string())?;

        let sleeping = ServerState::Sleeping {
            profile,
            since: chrono::Utc::now(),
        };
        self.set_server_state(sleeping.clone()).await;
        Ok(sleeping)
    }
}

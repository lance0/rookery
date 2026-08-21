use rookery_core::config::Config;
use rookery_core::state::{ServerState, StatePersistence};
use rookery_engine::agent::AgentManager;
use rookery_engine::backend::InferenceBackend;
use rookery_engine::gpu::GpuMonitor;
use rookery_engine::hardware::HardwareProfile;
use rookery_engine::logs::LogBuffer;
use rookery_engine::models::HfClient;
use rookery_engine::releases::{GitHubClient, ReleaseCache};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use tokio::sync::{Mutex, RwLock, broadcast, watch};

use crate::metrics::RuntimeMetrics;

#[derive(Debug)]
pub enum StartServerError {
    Start(String),
    Health(String),
    /// Shutdown had already begun, so no backend was spawned. Distinct from
    /// `Start` because it is not a failure and must not map to a 500.
    Shutdown,
}

impl std::fmt::Display for StartServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start(msg) | Self::Health(msg) => f.write_str(msg),
            // Borrow the canonical wording rather than repeating the literal.
            Self::Shutdown => write!(f, "{}", rookery_core::error::Error::Shutdown),
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
    pub cuda_error_tx: watch::Sender<Option<rookery_engine::backend::BackendErrorEvent>>,
    pub last_inference_at: AtomicI64,
    pub op_lock: Mutex<()>,
    pub hf_client: HfClient,
    pub hardware_profile: HardwareProfile,
    pub github_client: GitHubClient,
    pub release_cache: RwLock<ReleaseCache>,
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
        // LAN-1128: refuse to spawn a backend once shutdown has begun — the same
        // race LAN-1120 closed for `post_swap`, guarded here instead of at the
        // routes because every start path (`post_start`, `post_wake`,
        // `post_chat`'s wake, the canary restart, auto-start) funnels through
        // this one function.
        //
        // `begin_shutdown()` runs before `server_handle.abort()`, so the flag is
        // already visible; shutdown then gives up on `op_lock` after 20s
        // (LAN-1074) while the health wait below runs for up to 120s. Without
        // this check a start that is merely slow goes on to spawn an
        // llama-server *after* the daemon has exited — ~30 GB of VRAM held by an
        // unsupervised orphan.
        //
        // Placement is before the `Starting` broadcast on purpose: nothing has
        // been torn down or announced yet, so there is no transient state to
        // unwind and no window in which a client sees `Starting` for a start
        // that will never happen.
        if self.agent_manager.is_shutting_down() {
            tracing::warn!(
                profile = %profile_name,
                "daemon is shutting down, refusing to start server"
            );
            // `Stopped` is the honest terminal state (no backend was spawned)
            // and it is what the shutdown path writes anyway, so this is
            // idempotent with it. It also matters if we never get there: a wake
            // aborted here would otherwise persist `Sleeping`, and a SIGKILL at
            // `TimeoutStopSec` would leave the next boot restoring a sleeping
            // server that does not exist.
            self.set_server_state(ServerState::Stopped).await;
            return Err(StartServerError::Shutdown);
        }

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
        if let Err(e) = self.backend.lock().await.stop().await {
            // The server is still Running, so un-suppress — otherwise the watchdog
            // silently stops bouncing agents on dependency-port transitions until
            // the next start/stop/swap happens to clear it.
            self.agent_manager.set_dependency_bounce_suppressed(false);
            return Err(e.to_string());
        }

        let sleeping = ServerState::Sleeping {
            profile,
            since: chrono::Utc::now(),
        };
        self.set_server_state(sleeping.clone()).await;
        Ok(sleeping)
    }
}

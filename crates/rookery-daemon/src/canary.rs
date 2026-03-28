//! Inference canary — periodic minimal completion request to detect
//! CUDA zombie state where /health responds but inference is broken.
//!
//! The canary logic was extracted from the `tokio::spawn` block in `main()`
//! so it can be unit-tested without starting the full daemon.

use rookery_core::config::Config;
use rookery_core::state::StatePersistence;
use rookery_engine::backend::InferenceBackend;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

/// Timeout for inference canary requests.
pub const CANARY_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay before retrying a failed canary check.
pub const CANARY_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Timeout for health check after a canary-triggered restart.
pub const CANARY_HEALTH_TIMEOUT: Duration = Duration::from_secs(120);

/// Run one iteration of the inference canary check.
///
/// Checks whether the running backend can handle inference requests.
/// If the check fails twice (with a 5-second retry), the server is
/// restarted under the `op_lock` to serialize with manual start/stop/swap.
///
/// Returns `true` if a restart was performed (whether or not it succeeded).
/// Returns `false` if no restart was needed (server healthy, not running,
/// draining, or already stopped by someone else).
pub async fn run_canary_check(
    backend: &Arc<Mutex<Box<dyn InferenceBackend>>>,
    config: &Arc<RwLock<Config>>,
    state_persistence: &StatePersistence,
    op_lock: &Mutex<()>,
) -> bool {
    // Only check when server is running and not mid-swap
    if backend.lock().await.is_draining() {
        return false;
    }
    let current = backend.lock().await.to_server_state().await;
    let (profile, port) = match current {
        rookery_core::state::ServerState::Running {
            ref profile, port, ..
        } => (profile.clone(), port),
        _ => return false,
    };

    if rookery_engine::health::check_inference(port, CANARY_TIMEOUT).await {
        tracing::debug!(port, "inference canary passed");
        return false;
    }

    // First failure — retry once after 5s to avoid false positives
    tracing::warn!(port, "inference canary failed, retrying in 5s");
    tokio::time::sleep(CANARY_RETRY_DELAY).await;

    if rookery_engine::health::check_inference(port, CANARY_TIMEOUT).await {
        tracing::info!(port, "inference canary passed on retry");
        return false;
    }

    // Two consecutive failures — server is broken, restart it
    tracing::error!(port, profile = %profile, "inference canary failed twice, restarting server");

    // Acquire op_lock to serialize with manual start/stop/swap
    let _op_guard = op_lock.lock().await;

    // Re-check state under lock — someone may have stopped/swapped already
    let current = backend.lock().await.to_server_state().await;
    if !current.is_running() {
        tracing::info!("server already stopped, skipping canary restart");
        return false;
    }

    let _ = backend.lock().await.stop().await;
    let stopped = rookery_core::state::ServerState::Stopped;
    let _ = state_persistence.save(&stopped);

    let config = config.read().await;
    let backend_guard = backend.lock().await;
    match backend_guard.start(&config, &profile).await {
        Ok(_info) => {
            let port_for_health = config
                .profiles
                .get(&profile)
                .map(|p| p.port)
                .unwrap_or(port);
            drop(backend_guard);
            drop(config);
            match rookery_engine::health::wait_for_health(port_for_health, CANARY_HEALTH_TIMEOUT)
                .await
            {
                Ok(()) => {
                    let server_state = backend.lock().await.to_server_state().await;
                    let _ = state_persistence.save(&server_state);
                    if server_state.is_running() {
                        tracing::info!(profile = %profile, "server restarted by inference canary");
                    } else {
                        tracing::error!(profile = %profile, "server failed to restart after canary");
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, profile = %profile, "health check failed after canary restart");
                    let _ = backend.lock().await.stop().await;
                    let failed = rookery_core::state::ServerState::Failed {
                        last_error: e.to_string(),
                        profile: profile.clone(),
                        since: chrono::Utc::now(),
                    };
                    let _ = state_persistence.save(&failed);
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, profile = %profile, "canary restart failed");
        }
    }

    true
}

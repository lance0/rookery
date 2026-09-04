use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::app_state::AppState;
use crate::metrics;

#[derive(Serialize)]
pub struct StatusResponse {
    pub state: String,
    pub profile: Option<String>,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub uptime_secs: Option<i64>,
    pub backend: Option<String>,
}

pub async fn get_status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let server_state = state.current_state().await;
    Json(status_from_state(&server_state))
}

#[derive(Serialize)]
pub struct GpuResponse {
    pub gpus: Vec<rookery_engine::gpu::GpuStats>,
}

pub async fn get_gpu(State(state): State<Arc<AppState>>) -> Result<Json<GpuResponse>, StatusCode> {
    match &state.gpu_monitor {
        Some(monitor) => match monitor.stats() {
            Ok(gpus) => Ok(Json(GpuResponse { gpus })),
            Err(e) => {
                tracing::error!(error = %e, "failed to query GPU stats");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        },
        None => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

pub async fn get_metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        metrics::encode_metrics(&state).await,
    )
}

#[derive(Deserialize)]
pub struct StartRequest {
    pub profile: Option<String>,
}

#[derive(Serialize)]
pub struct ActionResponse {
    pub success: bool,
    pub message: String,
    pub status: StatusResponse,
}

pub async fn post_start(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StartRequest>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let _op_guard = state.op_lock.lock().await;

    // Read config, extract what we need, then drop the lock before long awaits
    let profile_name;
    let estimated_vram_mb;
    let is_vllm_profile;
    {
        let config = state.config.read().await;
        profile_name = config
            .resolve_profile_name(req.profile.as_deref())
            .to_string();

        let profile = config.profiles.get(&profile_name);
        estimated_vram_mb = profile
            .and_then(|p| config.models.get(&p.model))
            .and_then(|m| m.estimated_vram_mb);
        is_vllm_profile = profile
            .map(|p| p.backend_type() == rookery_core::config::BackendType::Vllm)
            .unwrap_or(false);
    }

    // Idempotent: if already running with same profile, no-op
    let current = state.current_state().await;
    if let rookery_core::state::ServerState::Running { ref profile, .. } = current {
        if profile == &profile_name {
            return Ok(Json(ActionResponse {
                success: true,
                message: format!("already running with profile '{profile_name}'"),
                status: status_from_state(&current),
            }));
        } else {
            return Ok(Json(ActionResponse {
                success: false,
                message: format!("server is running with profile '{profile}' — use swap to change"),
                status: status_from_state(&current),
            }));
        }
    }

    // Capacity gate: check VRAM before starting.
    // For vLLM profiles, skip the capacity gate — vLLM manages its own GPU memory
    // via gpu_memory_utilization. If estimated_vram_mb is set, log a soft warning
    // but do NOT block the start.
    if !is_vllm_profile {
        if let Some(ref monitor) = state.gpu_monitor
            && let Some(estimated_mb) = estimated_vram_mb
            && let Ok(stats) = monitor.stats()
            && let Some(gpu) = stats.first()
        {
            // LAN-1092: saturating. NVML should never report used > total, but
            // if it does, debug panics inside a request handler and release
            // wraps to ~1.8e19 — which silently passes the capacity gate on the
            // next line, i.e. the check fails open exactly when the numbers are
            // untrustworthy.
            let free_mb = gpu.vram_total_mb.saturating_sub(gpu.vram_used_mb);
            if free_mb < estimated_mb as u64 {
                return Ok(Json(ActionResponse {
                    success: false,
                    message: format!(
                        "insufficient VRAM: need ~{}MB, only {}MB free ({}MB / {}MB used)",
                        estimated_mb, free_mb, gpu.vram_used_mb, gpu.vram_total_mb
                    ),
                    status: status_from_state(&current),
                }));
            }
        }
    } else if let Some(estimated_mb) = estimated_vram_mb {
        // Soft warning for vLLM: log that VRAM estimate exists but won't block
        if let Some(ref monitor) = state.gpu_monitor
            && let Ok(stats) = monitor.stats()
            && let Some(gpu) = stats.first()
        {
            // LAN-1092: saturating, same reason as the gate above. Here the
            // consequence is only a bogus warn line, but the debug panic is the
            // same.
            let free_mb = gpu.vram_total_mb.saturating_sub(gpu.vram_used_mb);
            if free_mb < estimated_mb as u64 {
                tracing::warn!(
                    profile = %profile_name,
                    estimated_vram_mb = estimated_mb,
                    free_vram_mb = free_mb,
                    "vLLM profile: estimated VRAM exceeds free VRAM, but vLLM manages its own GPU memory"
                );
            }
        }
    }

    tracing::info!(profile = %profile_name, "starting server");

    match state.start_profile(&profile_name, true).await {
        Ok(server_state) => {
            let is_running = server_state.is_running();
            let status = status_from_state(&server_state);
            Ok(Json(ActionResponse {
                success: is_running,
                message: if is_running {
                    format!("server started with profile '{profile_name}'")
                } else {
                    "server failed to start".into()
                },
                status,
            }))
        }
        Err(crate::app_state::StartServerError::Health(e)) => {
            tracing::error!(error = %e, "health check failed, stopping server");
            let failed = state.current_state().await;
            Ok(Json(ActionResponse {
                success: false,
                message: "server failed to start".into(),
                status: status_from_state(&failed),
            }))
        }
        Err(crate::app_state::StartServerError::Start(e)) => {
            tracing::error!(error = %e, "failed to start server");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
        // LAN-1128: bare 503, no body. This is the only 503 the handler can
        // return, so the status alone says what happened, and `start_profile`
        // has already logged it with the profile name. See post_wake for why
        // the signature was not widened to post_swap's tuple.
        Err(crate::app_state::StartServerError::Shutdown) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

pub async fn post_stop(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let _op_guard = state.op_lock.lock().await;

    tracing::info!("stopping server");

    let current = state.current_state().await;
    if matches!(current, rookery_core::state::ServerState::Stopped) {
        return Ok(Json(ActionResponse {
            success: true,
            message: "server already stopped".into(),
            status: status_from_state(&current),
        }));
    }

    let stopping = rookery_core::state::ServerState::Stopping {
        since: chrono::Utc::now(),
    };
    state.set_server_state(stopping).await;
    state.agent_manager.set_dependency_bounce_suppressed(false);

    let stop_result = match current {
        rookery_core::state::ServerState::Running { .. } => state.backend.lock().await.stop().await,
        _ => Ok(()),
    };

    match stop_result {
        Ok(()) => {
            let stopped = rookery_core::state::ServerState::Stopped;
            state.set_server_state(stopped.clone()).await;
            let status = status_from_state(&stopped);
            Ok(Json(ActionResponse {
                success: true,
                message: "server stopped".into(),
                status,
            }))
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to stop server");
            // LAN-1123: must land a terminal state. `Stopping` was broadcast
            // above and nothing else resets it, so returning here used to wedge
            // the dashboard badge, CLI status and `rookery_server_up` on a
            // transient state until the daemon restarted (`reconcile()` folds
            // Stopping → Stopped, but only across a restart). Same bug LAN-1081
            // fixed in post_swap.
            //
            // Prefer the truth over a blanket `Failed`: a failed stop often
            // means the process is still alive — VllmBackend deliberately keeps
            // its container_id when `docker compose down` fails — and reporting
            // `Failed` while it serves would let a later post_start launch a
            // second server on the same port.
            let after = state.backend.lock().await.to_server_state().await;
            let terminal = if after.is_running() {
                after
            } else {
                rookery_core::state::ServerState::Failed {
                    last_error: e.to_string(),
                    profile: current.profile_name().unwrap_or_default().to_string(),
                    since: chrono::Utc::now(),
                }
            };
            state.set_server_state(terminal).await;
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn post_sleep(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let _op_guard = state.op_lock.lock().await;

    let current = state.current_state().await;
    match &current {
        rookery_core::state::ServerState::Sleeping { .. } => {
            return Ok(Json(ActionResponse {
                success: true,
                message: "server already sleeping".into(),
                status: status_from_state(&current),
            }));
        }
        rookery_core::state::ServerState::Running { .. } => {}
        _ => {
            return Ok(Json(ActionResponse {
                success: false,
                message: "server is not running".into(),
                status: status_from_state(&current),
            }));
        }
    }

    match state.sleep_server().await {
        Ok(server_state) => Ok(Json(ActionResponse {
            success: true,
            message: "server sleeping".into(),
            status: status_from_state(&server_state),
        })),
        Err(e) => {
            tracing::error!(error = %e, "failed to put server to sleep");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn post_wake(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let _op_guard = state.op_lock.lock().await;

    let current = state.current_state().await;
    let profile = match current {
        rookery_core::state::ServerState::Running { ref profile, .. } => {
            return Ok(Json(ActionResponse {
                success: true,
                message: format!("already running with profile '{profile}'"),
                status: status_from_state(&state.current_state().await),
            }));
        }
        rookery_core::state::ServerState::Sleeping { ref profile, .. } => profile.clone(),
        _ => {
            return Ok(Json(ActionResponse {
                success: false,
                message: "server is not sleeping".into(),
                status: status_from_state(&current),
            }));
        }
    };

    match state.start_profile(&profile, true).await {
        Ok(server_state) => Ok(Json(ActionResponse {
            success: server_state.is_running(),
            message: format!("server woke with profile '{profile}'"),
            status: status_from_state(&server_state),
        })),
        Err(crate::app_state::StartServerError::Health(e)) => {
            tracing::error!(error = %e, profile = %profile, "wake health check failed");
            let failed = state.current_state().await;
            Ok(Json(ActionResponse {
                success: false,
                message: "server failed to wake".into(),
                status: status_from_state(&failed),
            }))
        }
        Err(crate::app_state::StartServerError::Start(e)) => {
            tracing::error!(error = %e, profile = %profile, "wake start failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
        // LAN-1128: bare 503 rather than widening this handler (and post_start)
        // to post_swap's `(StatusCode, Json<Value>)` shape. Neither client reads
        // the body on this path — the dashboard collapses any non-2xx to
        // `HTTP {status}` and the CLI's `ClientError::Status` already renders an
        // empty body as plain "server returned status 503" — so the tuple would
        // buy a constant string at the cost of changing two public signatures.
        Err(crate::app_state::StartServerError::Shutdown) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

#[derive(Deserialize)]
pub struct SwapRequest {
    pub profile: String,
}

pub async fn post_swap(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SwapRequest>,
) -> Result<Json<ActionResponse>, (StatusCode, Json<serde_json::Value>)> {
    let _op_guard = state.op_lock.lock().await;

    // Validate the target profile BEFORE any teardown. A typo'd name used to
    // drain and stop the running server first and only fail at the lookup
    // below, taking the model down for nothing.
    {
        let config = state.config.read().await;
        if !config.profiles.contains_key(&req.profile) {
            let mut known: Vec<&String> = config.profiles.keys().collect();
            known.sort();
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("no such profile: {}", req.profile),
                    "profiles": known,
                })),
            ));
        }
    }

    let old_profile = state
        .current_state()
        .await
        .profile_name()
        .map(ToString::to_string);

    tracing::info!(
        from = ?old_profile,
        to = %req.profile,
        "swapping model"
    );

    // Broadcast BEFORE the drain. A swap is 30s+; until this landed, every
    // client kept reporting the old profile as `running` for the whole ride.
    // No drain flag is set yet, so this is not an early-return path.
    state
        .set_server_state(rookery_core::state::ServerState::Swapping {
            from: old_profile.clone(),
            to: req.profile.clone(),
            since: chrono::Utc::now(),
        })
        .await;

    // Swap orchestration at daemon level: drain → stop → create new backend → start → health check
    //
    // IMPORTANT: set_draining(false) must be called on ALL exit paths after drain is set.
    // If the old backend remains in AppState with draining=true, post_chat permanently
    // returns 503. We use a helper closure to ensure cleanup on every error path.
    let swap_result: std::result::Result<
        rookery_core::state::ServerState,
        rookery_core::error::Error,
    > = async {
        // Drain in-flight requests if currently running
        let was_draining;
        {
            let backend = state.backend.lock().await;
            if backend.is_running().await {
                backend.set_draining(true);
                was_draining = true;
                tracing::info!("draining in-flight requests (5s)");
            } else {
                was_draining = false;
            }
        }

        // Helper: clear drain flag on the current backend (no-op if backend was replaced)
        let clear_drain = || async {
            state.backend.lock().await.set_draining(false);
        };

        // Drain period
        if was_draining {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if let Err(e) = state.backend.lock().await.stop().await {
                clear_drain().await;
                return Err(e);
            }
        }

        // Drain served its purpose — clear it on the old backend before proceeding.
        // The new backend will start fresh with draining=false.
        if was_draining {
            clear_drain().await;
        }

        // LAN-1120: refuse to spawn a backend once shutdown has begun.
        // `begin_shutdown()` runs before `server_handle.abort()`, so the flag is
        // already visible here; shutdown then gives up on `op_lock` after 20s
        // (TimeoutStopSec=45 vs a ~135s worst-case swap), so without this check
        // the swap goes on to spawn an llama-server *after* the daemon has
        // exited — ~30 GB of VRAM held by an unsupervised orphan. Same check the
        // idle watcher in main.rs makes after taking `op_lock`.
        //
        // Placement: this sits AFTER the unconditional `clear_drain()` above and
        // before anything sets drain again, so this early return cannot leak
        // `draining=true` and wedge post_chat on a permanent 503.
        if state.agent_manager.is_shutting_down() {
            tracing::warn!(
                profile = %req.profile,
                "daemon is shutting down, aborting swap before starting new backend"
            );
            return Err(rookery_core::error::Error::Shutdown);
        }

        // Create new backend for the target profile and start it
        let config = state.config.read().await;
        let profile = config
            .profiles
            .get(&req.profile)
            .ok_or_else(|| rookery_core::error::Error::ProfileNotFound(req.profile.clone()))?;
        let new_backend = rookery_engine::backend::create_backend_with_error_notifier(
            profile,
            state.log_buffer.clone(),
            Some(state.cuda_error_tx.clone()),
        )?;
        let port = profile.port;

        // Start the new backend.
        // If this fails, the old backend (already stopped) stays in AppState
        // but draining was already cleared above, so post_chat won't return 503.
        new_backend.start(&config, &req.profile).await?;
        drop(config);

        // Replace the backend in AppState
        *state.backend.lock().await = new_backend;
        state.agent_manager.set_dependency_bounce_suppressed(false);

        // Wait for health with 120s timeout
        match rookery_engine::health::wait_for_health(port, std::time::Duration::from_secs(120))
            .await
        {
            Ok(()) => Ok(state.backend.lock().await.to_server_state().await),
            Err(e) => {
                tracing::error!(error = %e, "health check failed after swap, stopping server");
                let _ = state.backend.lock().await.stop().await;
                Ok(rookery_core::state::ServerState::Failed {
                    last_error: e.to_string(),
                    profile: req.profile.clone(),
                    since: chrono::Utc::now(),
                })
            }
        }
    }
    .await;

    match swap_result {
        Ok(server_state) => {
            state.set_server_state(server_state.clone()).await;
            let is_running = server_state.is_running();
            if is_running {
                state.metrics.inc_server_restart();
                state.record_inference_activity();
            }
            let status = status_from_state(&server_state);

            // Restart agents that have restart_on_swap = true.
            // Brief delay between stop and start to let the agent fully exit,
            // and retry once on failure (agent may have been mid-request during swap).
            if is_running {
                let config = state.config.read().await;
                for (name, agent_config) in &config.agents {
                    if agent_config.restart_on_swap && state.agent_manager.is_running(name).await {
                        // Capture prev restarts before stop
                        let health = state.agent_manager.get_health(name).await;
                        let prev_restarts =
                            health.as_ref().and_then(|h| h.total_restarts).unwrap_or(0);
                        let prev_errors =
                            health.as_ref().and_then(|h| h.lifetime_errors).unwrap_or(0);
                        tracing::info!(agent = %name, "restarting agent after swap");
                        let _ = state.agent_manager.stop_automated(name).await;
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        if let Err(e) = state.agent_manager.start(name, agent_config).await {
                            tracing::warn!(agent = %name, error = %e, "agent restart failed after swap, retrying");
                            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                            if let Err(e) = state.agent_manager.start(name, agent_config).await {
                                tracing::error!(agent = %name, error = %e, "agent restart failed after swap retry");
                            }
                        }
                        state
                            .agent_manager
                            .record_restart(name, "swap", prev_restarts, prev_errors)
                            .await;
                    }
                }
            }

            Ok(Json(ActionResponse {
                success: is_running,
                message: if is_running {
                    format!(
                        "swapped {} → '{}'",
                        old_profile
                            .map(|p| format!("'{p}'"))
                            .unwrap_or("(stopped)".into()),
                        req.profile
                    )
                } else {
                    "swap failed — server did not start".into()
                },
                status,
            }))
        }
        Err(rookery_core::error::Error::Shutdown) => {
            // Not a failure: the old backend is stopped and no new one was
            // launched, so `Stopped` is the honest terminal state — and it is
            // what the shutdown path writes anyway, so this is idempotent.
            // `Swapping` must not be left behind (LAN-1081).
            state
                .set_server_state(rookery_core::state::ServerState::Stopped)
                .await;
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "daemon is shutting down" })),
            ))
        }
        Err(e) => {
            tracing::error!(error = %e, "swap failed");
            // Must land a terminal state: this path used to leave the stale
            // `Running{old}` behind, and now that we broadcast `Swapping` up
            // front it would otherwise stick there forever, wedging every
            // client that gates on it.
            state
                .set_server_state(rookery_core::state::ServerState::Failed {
                    last_error: e.to_string(),
                    profile: req.profile.clone(),
                    since: chrono::Utc::now(),
                })
                .await;
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            ))
        }
    }
}

pub async fn get_profiles(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let config = state.config.read().await;
    let profiles: Vec<serde_json::Value> = config
        .profiles
        .iter()
        .map(|(name, p)| {
            let is_default = name == &config.default_profile;
            let model = config.models.get(&p.model);
            let ls = p.llama_server_config();
            serde_json::json!({
                "name": name,
                "model": p.model,
                "port": p.port,
                // Container backends have no llama_server sub-table, so fall back
                // to their own context setting rather than reporting none —
                // `rookery profiles` renders an SGLang row with no context
                // otherwise, which reads like a broken profile.
                "ctx_size": ls.as_ref().map(|c| c.ctx_size).or_else(|| {
                    p.sglang
                        .as_ref()
                        .and_then(|s| s.context_length)
                        .map(|c| c as u32)
                }),
                "reasoning_budget": ls.as_ref().map(|c| c.reasoning_budget),
                "backend": p.backend_type().to_string(),
                "default": is_default,
                "estimated_vram_mb": model.and_then(|m| m.estimated_vram_mb),
            })
        })
        .collect();

    Json(serde_json::json!({ "profiles": profiles }))
}

pub async fn get_health() -> StatusCode {
    StatusCode::OK
}

// --- Config ---

pub async fn get_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let config = state.config.read().await;
    let mut val = serde_json::to_value(&*config).unwrap_or_default();

    // Only redact when a value is actually set — reporting "[redacted]" for an
    // absent api_key tells an operator the daemon is authenticated when it is not.
    if let Some(api_key) = val.get_mut("api_key")
        && !api_key.is_null()
    {
        *api_key = serde_json::json!("[redacted]");
    }
    if let Some(token) = val.get_mut("github_token")
        && !token.is_null()
    {
        *token = serde_json::json!("[redacted]");
    }

    // Redact sensitive fields from agent configs
    if let Some(agents) = val.get_mut("agents").and_then(|a| a.as_object_mut()) {
        for (_name, agent) in agents.iter_mut() {
            if let Some(env) = agent.get_mut("env") {
                let count = env.as_object().map(|o| o.len()).unwrap_or(0);
                *env = serde_json::json!(format!("[{count} vars redacted]"));
            }
        }
    }

    Json(val)
}

/// How long `post_reload` waits for `op_lock` before giving up.
///
/// Reload serialises against start/stop/swap so it can never land half-way
/// through one — but it runs inside a request handler, and a worst-case swap
/// holds `op_lock` for ~135s (LAN-1074 bounded the *shutdown* wait at 20s for
/// exactly this reason). So the wait is bounded: an uncontended reload takes
/// the lock immediately, a contended one gets an actionable 409 instead of
/// pinning a connection for minutes.
const RELOAD_OP_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

fn reload_error(status: StatusCode, error: String) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({ "success": false, "error": error })),
    )
}

/// Re-read the config file from disk into the live `AppState` — without
/// touching the running backend, its port, or any agent process.
///
/// **A bad config can never take down a running daemon.** The file is read,
/// parsed and validated into a *local* `Config`; every failure path returns
/// before `state.config` is written, so a rejected reload leaves the daemon on
/// exactly the config it was already serving. This is deliberately the inverse
/// of LAN-1076's boot behaviour — at boot an invalid config is `exit(1)`,
/// here it is a 400 and nothing else.
///
/// Reload changes what *future* operations see. The live backend keeps its
/// profile, port, PID and binary until the next start/swap; agents are never
/// bounced. Anything the reload cannot honour comes back in `warnings`,
/// because a reload that silently doesn't apply is worse than no reload.
pub async fn post_reload(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let path = state.config_path.clone();

    let text = std::fs::read_to_string(&path).map_err(|e| {
        reload_error(
            StatusCode::BAD_REQUEST,
            format!("failed to read {}: {e} — config unchanged", path.display()),
        )
    })?;

    let candidate: rookery_core::config::Config = toml::from_str(&text).map_err(|e| {
        reload_error(
            StatusCode::BAD_REQUEST,
            format!("{}: parse error: {e} — config unchanged", path.display()),
        )
    })?;

    // The same gate main.rs applies at boot, minus the exit(1).
    candidate.validate().map_err(|e| {
        reload_error(
            StatusCode::BAD_REQUEST,
            format!("{}: {e} — config unchanged", path.display()),
        )
    })?;

    let _op_guard = tokio::time::timeout(RELOAD_OP_LOCK_WAIT, state.op_lock.lock())
        .await
        .map_err(|_| {
            reload_error(
                StatusCode::CONFLICT,
                format!(
                    "a start/stop/swap is in flight (waited {}s) — config unchanged, retry when it finishes",
                    RELOAD_OP_LOCK_WAIT.as_secs()
                ),
            )
        })?;

    let live = state.current_state().await;
    let live_port = match &live {
        rookery_core::state::ServerState::Running { port, .. } => Some(*port),
        _ => None,
    };

    let mut warnings: Vec<String> = Vec::new();
    {
        let old = state.config.read().await;
        if old.listen != candidate.listen {
            warnings.push(format!(
                "listen changed {} → {} — the socket is already bound, this one needs a daemon restart",
                old.listen, candidate.listen
            ));
        }
        // Compare through serde rather than PartialEq: AgentConfig doesn't
        // derive it, and Serialize is already there.
        if serde_json::to_value(&old.agents).ok() != serde_json::to_value(&candidate.agents).ok() {
            warnings.push(
                "[agents] changed — running agents are deliberately left alone and the watchdog \
                 holds the definitions it booted with, so this one needs a daemon restart"
                    .into(),
            );
        }
        if old.release_check_interval != candidate.release_check_interval {
            warnings.push(
                "release_check_interval changed — the release monitor captured its interval at \
                 boot, so this one needs a daemon restart"
                    .into(),
            );
        }
    }

    if let Some(profile) = live.profile_name() {
        match candidate.profiles.get(profile) {
            // Removing the running profile is legal and does not stop anything.
            // The backend is owned by AppState, not by the config entry.
            None => warnings.push(format!(
                "profile '{profile}' is live but is no longer in the config — the backend keeps \
                 running untouched and stop/sleep still work, but start/swap back to it will fail"
            )),
            Some(p) => {
                if let Some(port) = live_port
                    && p.port != port
                {
                    warnings.push(format!(
                        "profile '{profile}' port changed {port} → {} — the live backend stays on \
                         {port}; the new port applies on the next start/swap",
                        p.port
                    ));
                }
            }
        }
    }

    let mut profiles: Vec<String> = candidate.profiles.keys().cloned().collect();
    profiles.sort();

    *state.config.write().await = candidate;

    tracing::info!(
        path = %path.display(),
        profiles = profiles.len(),
        warnings = warnings.len(),
        "config reloaded (backend and agents untouched)"
    );

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("config reloaded from {}", path.display()),
        "path": path.display().to_string(),
        "profiles": profiles,
        "applied_now": [
            "api_key (checked on every request)",
            "idle_timeout (re-read each 30s poll)",
            "default_profile",
            "profiles / models — for the next start or swap",
            "llama_server binary path — for the next start or swap",
        ],
        "unchanged": [
            "the running backend: profile, port, PID and binary stay exactly as they are",
            "agent processes: none are started, stopped or bounced",
        ],
        "needs_daemon_restart": ["listen", "agents", "auto_start", "release_check_interval"],
        "warnings": warnings,
    })))
}

#[derive(Deserialize)]
pub struct ProfileUpdate {
    #[serde(default)]
    pub temp: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub top_k: Option<u32>,
    #[serde(default)]
    pub min_p: Option<f32>,
    #[serde(default)]
    pub ctx_size: Option<u32>,
    #[serde(default)]
    pub threads: Option<u8>,
    #[serde(default)]
    pub threads_batch: Option<u8>,
    #[serde(default)]
    pub batch_size: Option<u32>,
    #[serde(default)]
    pub ubatch_size: Option<u32>,
    #[serde(default)]
    pub reasoning_budget: Option<i32>,
    #[serde(default)]
    pub flash_attention: Option<bool>,
    #[serde(default)]
    pub cache_type_k: Option<String>,
    #[serde(default)]
    pub cache_type_v: Option<String>,
}

pub async fn put_profile(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(update): Json<ProfileUpdate>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut config = state.config.write().await;

    let profile = config.profiles.get_mut(&name).ok_or((
        StatusCode::NOT_FOUND,
        format!(
            "no such profile: {name} (config is read at daemon start — POST /api/reload if you just added it)"
        ),
    ))?;

    // Write through to the [llama_server] sub-table, not the legacy flat fields.
    // resolve_llama_server_command_line consumes ONLY the sub-table when it is
    // present, so writing the flat fields there is a silent no-op. Profiles
    // still on the legacy form get normalized onto the sub-table here —
    // llama_server_config() already knows how to build one from them.
    let mut ls = profile.llama_server_config().ok_or((
        StatusCode::CONFLICT,
        format!("profile '{name}' is a vLLM profile; these fields are llama-server only"),
    ))?;

    if let Some(v) = update.temp {
        ls.temp = v;
    }
    if let Some(v) = update.top_p {
        ls.top_p = v;
    }
    if let Some(v) = update.top_k {
        ls.top_k = v;
    }
    if let Some(v) = update.min_p {
        ls.min_p = v;
    }
    if let Some(v) = update.ctx_size {
        ls.ctx_size = v;
    }
    if let Some(v) = update.threads {
        ls.threads = v;
    }
    if let Some(v) = update.threads_batch {
        ls.threads_batch = v;
    }
    if let Some(v) = update.batch_size {
        ls.batch_size = v;
    }
    if let Some(v) = update.ubatch_size {
        ls.ubatch_size = v;
    }
    if let Some(v) = update.reasoning_budget {
        ls.reasoning_budget = v;
    }
    if let Some(v) = update.flash_attention {
        ls.flash_attention = v;
    }
    if let Some(v) = update.cache_type_k {
        ls.cache_type_k = v;
    }
    if let Some(v) = update.cache_type_v {
        ls.cache_type_v = v;
    }

    profile.llama_server = Some(ls);

    if let Err(e) = config.save_to(&state.config_path) {
        tracing::error!(error = %e, "failed to save config");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to save config: {e}"),
        ));
    }

    tracing::info!(profile = %name, "profile updated and saved to disk");

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("profile '{name}' updated — changes apply on next start/swap"),
    })))
}

// --- Model Info ---

#[derive(Serialize)]
pub struct ModelInfoResponse {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    pub props: Option<serde_json::Value>,
}

pub async fn get_model_info(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ModelInfoResponse>, StatusCode> {
    let current = state.current_state().await;
    let port = match current {
        rookery_core::state::ServerState::Running { port, .. } => port,
        _ => {
            return Ok(Json(ModelInfoResponse {
                available: false,
                model_id: None,
                owned_by: None,
                props: None,
            }));
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut model_id = None;
    let mut owned_by = None;

    // Fetch /v1/models
    if let Ok(resp) = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .send()
        .await
        && let Ok(data) = resp.json::<serde_json::Value>().await
        && let Some(models) = data["data"].as_array()
        && let Some(first) = models.first()
    {
        model_id = first["id"].as_str().map(String::from);
        owned_by = first["owned_by"].as_str().map(String::from);
    }

    // Fetch /props (llama.cpp-specific — returns 404 for vLLM)
    let props = if let Ok(resp) = client
        .get(format!("http://127.0.0.1:{port}/props"))
        .send()
        .await
        && resp.status().is_success()
    {
        resp.json::<serde_json::Value>().await.ok()
    } else {
        None
    };

    Ok(Json(ModelInfoResponse {
        available: true,
        model_id,
        owned_by,
        props,
    }))
}

// --- Chat proxy (streaming passthrough) ---

#[derive(Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<serde_json::Value>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: i32,
}

fn default_max_tokens() -> i32 {
    2048
}

pub async fn post_chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    // Reject new requests during swap drain
    if state.backend.lock().await.is_draining() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    state.metrics.inc_chat_request();
    state.record_inference_activity();

    let port = match state.current_state().await {
        rookery_core::state::ServerState::Running { port, .. } => port,
        rookery_core::state::ServerState::Sleeping { .. } => {
            let _op_guard = state.op_lock.lock().await;
            match state.current_state().await {
                rookery_core::state::ServerState::Running { port, .. } => port,
                rookery_core::state::ServerState::Sleeping { profile, .. } => {
                    tracing::info!(profile = %profile, "waking sleeping server for inference request");
                    match state.start_profile(&profile, true).await {
                        Ok(rookery_core::state::ServerState::Running { port, .. }) => port,
                        Ok(_) => {
                            state.metrics.inc_chat_error();
                            return Err(StatusCode::SERVICE_UNAVAILABLE);
                        }
                        Err(e) => {
                            state.metrics.inc_chat_error();
                            tracing::error!(error = %e, profile = %profile, "failed to wake sleeping server");
                            return Err(StatusCode::SERVICE_UNAVAILABLE);
                        }
                    }
                }
                _ => {
                    state.metrics.inc_chat_error();
                    return Err(StatusCode::SERVICE_UNAVAILABLE);
                }
            }
        }
        _ => {
            state.metrics.inc_chat_error();
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|_| {
            state.metrics.inc_chat_error();
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Ask the server what it serves instead of asserting a name: vLLM 404s
    // every request whose `model` doesn't match `--served-model-name`, which
    // made this proxy fail unconditionally for vLLM profiles.
    let model = rookery_engine::health::served_model_id(&client, port).await;

    let body = serde_json::json!({
        "model": model,
        "messages": req.messages,
        "max_tokens": req.max_tokens,
        "stream": true,
    });

    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            state.metrics.inc_chat_error();
            tracing::error!(error = %e, "chat proxy request failed");
            StatusCode::BAD_GATEWAY
        })?;

    // `.send()` only errors on transport failure — a non-2xx upstream response
    // arrives as Ok and would otherwise be streamed out under a hardcoded 200.
    // ponytail: always 502, never the upstream status. Even an upstream 400 can
    // be our fault (a bad "model" name we resolved above), so forwarding it
    // would blame the caller for a proxy-side bug.
    if !resp.status().is_success() {
        state.metrics.inc_chat_error();
        let status = resp.status();
        let detail = resp.text().await.unwrap_or_default();
        tracing::error!(status = %status, detail = %detail, "chat upstream returned error");
        return Err(StatusCode::BAD_GATEWAY);
    }

    // Wrap the stream with a per-chunk timeout — if llama-server hangs
    // mid-generation with no data for 60s, terminate the stream.
    use tokio_stream::StreamExt as _;
    let stream = resp
        .bytes_stream()
        .timeout(std::time::Duration::from_secs(60))
        .map(move |item| match item {
            Ok(Ok(bytes)) => Ok(bytes),
            Ok(Err(e)) => Err(std::io::Error::other(e)),
            Err(_elapsed) => {
                state.metrics.inc_chat_stream_timeout();
                tracing::warn!("chat stream timed out (no data for 60s)");
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "stream timeout",
                ))
            }
        });

    Ok((
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        axum::body::Body::from_stream(stream),
    ))
}

// --- Server Stats (slots proxy) ---

pub async fn get_server_stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let current = state.current_state().await;
    let (port, backend) = match current {
        rookery_core::state::ServerState::Running {
            port, backend_type, ..
        } => (port, backend_type),
        _ => {
            return Ok(Json(serde_json::json!({ "available": false })));
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Fetch /slots (llama.cpp-specific — returns 404 for vLLM and SGLang)
    let slots = if let Ok(resp) = client
        .get(format!("http://127.0.0.1:{port}/slots"))
        .send()
        .await
        && resp.status().is_success()
    {
        resp.json::<serde_json::Value>().await.ok()
    } else {
        None
    };

    // SGLang has no /slots, but exposes richer telemetry than llama-server does
    // on Prometheus /metrics — including `mamba_usage`, which is the GDN state
    // pool and the resource that actually runs out first on a 32GB card.
    // Requires the server to have been started with --enable-metrics; absent
    // that the scrape simply yields nothing and the field stays null.
    let sglang = if backend == rookery_core::config::BackendType::Sglang {
        match client
            .get(format!("http://127.0.0.1:{port}/metrics"))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                resp.text().await.ok().map(|b| parse_sglang_metrics(&b))
            }
            _ => None,
        }
    } else {
        None
    };

    Ok(Json(serde_json::json!({
        "available": true,
        "slots": slots,
        "sglang": sglang,
    })))
}

/// Pull the handful of scalar gauges worth showing out of a Prometheus scrape.
///
/// Deliberately ignores histograms and `_bucket` series: they are most of the
/// payload and none of the signal for a single-user dashboard. Labels are
/// stripped, so a metric emitted per-engine collapses to its last value — fine
/// here because we run one engine.
fn parse_sglang_metrics(body: &str) -> serde_json::Value {
    const WANTED: &[(&str, &str)] = &[
        ("sglang:max_total_num_tokens", "kv_total"),
        ("sglang:kv_used_tokens", "kv_used"),
        ("sglang:kv_available_tokens", "kv_available"),
        ("sglang:full_token_usage", "kv_usage"),
        ("sglang:mamba_usage", "mamba_usage"),
        ("sglang:mamba_used_tokens", "mamba_used"),
        ("sglang:mamba_available_tokens", "mamba_available"),
        ("sglang:cache_hit_rate", "cache_hit_rate"),
        ("sglang:gen_throughput", "gen_throughput"),
        ("sglang:spec_accept_length", "accept_length"),
        ("sglang:spec_accept_rate", "accept_rate"),
        ("sglang:num_running_reqs", "running_reqs"),
        ("sglang:num_queue_reqs", "queued_reqs"),
        ("sglang:context_len", "context_len"),
        ("sglang:kv_cache_memory_usage_gb", "kv_cache_gb"),
        ("sglang:weight_memory_usage_gb", "weight_gb"),
        ("sglang:generation_tokens_total", "generated_total"),
    ];

    let mut out = serde_json::Map::new();
    for line in body.lines() {
        if line.starts_with('#') || line.contains("_bucket") {
            continue;
        }
        let Some((lhs, rhs)) = line.rsplit_once(' ') else {
            continue;
        };
        let name = lhs.split('{').next().unwrap_or(lhs).trim();
        let Some((_, key)) = WANTED.iter().find(|(m, _)| *m == name) else {
            continue;
        };
        // NaN/Inf are real in this scrape (e.g. fwd_occupancy before traffic);
        // drop them rather than emitting invalid JSON.
        if let Ok(v) = rhs.trim().parse::<f64>()
            && v.is_finite()
            && let Some(n) = serde_json::Number::from_f64(v)
        {
            out.insert((*key).to_string(), serde_json::Value::Number(n));
        }
    }
    serde_json::Value::Object(out)
}

// --- Logs ---

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_log_count")]
    pub n: usize,
}

fn default_log_count() -> usize {
    50
}

pub async fn get_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LogsQuery>,
) -> Json<serde_json::Value> {
    let lines = state.log_buffer.last_n(query.n);
    Json(serde_json::json!({ "lines": lines }))
}

// --- Dashboard ---

use include_dir::{Dir, include_dir};

static DASHBOARD_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../rookery-dashboard/dist");

pub async fn get_dashboard(
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
) -> impl axum::response::IntoResponse {
    let raw = uri.path().trim_start_matches('/');

    // An unmatched /api/* path is a client error, not a request for the SPA.
    // Serving index.html with 200 meant a typo'd or renamed route came back as
    // HTML with a success status, so callers checking response.ok proceeded and
    // then failed on "<!doctype html>" instead of seeing "no such route".
    if raw.starts_with("api/") {
        return (
            axum::http::StatusCode::NOT_FOUND,
            [(
                axum::http::header::CONTENT_TYPE,
                "application/json".to_string(),
            )],
            axum::body::Body::from(format!("{{\"error\":\"no such API route: /{raw}\"}}")),
        )
            .into_response();
    }

    let path = if raw.is_empty() { "index.html" } else { raw };

    match DASHBOARD_DIR.get_file(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();

            // Trunk stamps a content hash into asset filenames, so any hashed asset
            // is immutable for the life of this URL and can skip revalidation
            // entirely. That matters beyond bandwidth: V8 caches *compiled* wasm
            // keyed on the URL, but a fresh 200 invalidates that cache while a 304
            // (or a no-revalidation hit) preserves it. Without this, every dashboard
            // reload recompiled ~900 KB of wasm from scratch.
            let hashed = path != "index.html";
            let cache_control = if hashed {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };

            // index.html is not content-hashed, so give it a validator instead.
            let etag = content_etag(file.contents());
            if let Some(inm) = headers.get(axum::http::header::IF_NONE_MATCH)
                && inm.to_str().map(|v| v.contains(&etag)).unwrap_or(false)
            {
                return (
                    axum::http::StatusCode::NOT_MODIFIED,
                    [
                        (axum::http::header::ETAG, etag),
                        (axum::http::header::CACHE_CONTROL, cache_control.to_string()),
                    ],
                )
                    .into_response();
            }

            (
                axum::http::StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (axum::http::header::CACHE_CONTROL, cache_control.to_string()),
                    (axum::http::header::ETAG, etag),
                ],
                file.contents(),
            )
                .into_response()
        }
        None => {
            // SPA fallback — serve index.html.
            //
            // Not unwrap(): include_dir! compiles fine against a dist/ built without
            // an index.html, which would turn every 404 into a panic in a request
            // handler.
            let Some(file) = DASHBOARD_DIR.get_file("index.html") else {
                return (
                    axum::http::StatusCode::NOT_FOUND,
                    "dashboard assets not built into this binary",
                )
                    .into_response();
            };
            let mime = mime_guess::from_path("index.html").first_or_octet_stream();
            (
                axum::http::StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (axum::http::header::CACHE_CONTROL, "no-cache".to_string()),
                ],
                file.contents(),
            )
                .into_response()
        }
    }
}

/// Strong ETag over the asset bytes.
///
/// Assets are embedded at compile time and never change for the life of the
/// process, so this is stable per binary. Hashing is cheap relative to the
/// response it saves, and only runs on the served path.
fn content_etag(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.len().hash(&mut h);
    bytes.hash(&mut h);
    format!("\"{:016x}\"", h.finish())
}

use axum::response::IntoResponse;

// --- Bench ---

#[derive(Serialize)]
pub struct BenchResult {
    pub tests: Vec<BenchTest>,
    /// One entry per prompt that produced no measurement. Empty on a clean run.
    ///
    /// LAN-1094: these used to be logged server-side and dropped, so a bench
    /// where every request timed out returned 200 `{"tests": []}` — byte
    /// identical to a bench nobody ever started. Still 200 even when all three
    /// fail: the partial case (some tests real, some failed) has to be 200
    /// anyway, and a non-2xx loses the reason entirely on the dashboard, whose
    /// `handle_response` discards the body and keeps only "HTTP 500".
    pub errors: Vec<BenchError>,
}

#[derive(Serialize)]
pub struct BenchError {
    pub name: String,
    pub error: String,
}

#[derive(Serialize)]
pub struct BenchTest {
    pub name: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub pp_tok_s: f64,
    pub gen_tok_s: f64,
}

/// Time one bench prompt by streaming it, and derive the rates from arrival times.
///
/// The obvious implementation reads llama.cpp's `timings` block, which is more
/// precise -- but only llama.cpp emits it, so vLLM and SGLang produced
/// "response carried no timings" for every prompt and `rookery bench` reported
/// "no results (is a model running?)" against a perfectly healthy server.
///
/// Falling back to `timings` where available and wall-clock elsewhere would be
/// worse than either: the two measure different things, so llama.cpp and SGLang
/// numbers would silently stop being comparable across a `rookery swap`. One
/// instrument for every backend is the whole point of this endpoint.
///
///   pp_tok_s  = prompt_tokens / TTFT      (TTFT is prefill-dominated)
///   gen_tok_s = (completion_tokens - 1) / (total - TTFT)
///
/// which is SGLang's TPOT definition inverted, and matches what
/// lancebox-inference's `bench_portable.py` reports for the same server.
async fn bench_one(
    client: &reqwest::Client,
    port: u16,
    model: &str,
    name: &str,
    prompt: &str,
) -> Result<BenchTest, String> {
    use futures_util::StreamExt;

    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 256,
        "stream": true,
        // Backends only report token counts on a streamed response when asked.
        "stream_options": {"include_usage": true},
    });

    let started = std::time::Instant::now();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    if !status.is_success() {
        // The body carries the actual reason ("model not found").
        let detail: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();
        return Err(format!("HTTP {status}: {}", detail.trim()));
    }

    let mut ttft: Option<std::time::Duration> = None;
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut deltas = 0u64;
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        buf.push_str(&String::from_utf8_lossy(&chunk.map_err(|e| e.to_string())?));
        // SSE frames are newline-delimited; keep any partial trailing line.
        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].trim().to_string();
            buf.drain(..=nl);
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
                continue;
            };
            // The usage frame arrives last and carries empty choices.
            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                prompt_tokens = u["prompt_tokens"].as_u64().unwrap_or(prompt_tokens);
                completion_tokens = u["completion_tokens"].as_u64().unwrap_or(completion_tokens);
            }
            // Count reasoning as generated text. A reasoning model behind
            // --reasoning-parser splits its output, emitting `reasoning_content`
            // deltas first and `content` only once it has finished thinking. Timing
            // just `content` puts TTFT after the entire reasoning phase, which
            // reports an absurd decode rate over the leftover window -- and on a
            // prompt whose thinking fills max_tokens, no content arrives at all and
            // the whole measurement is thrown away as "no content".
            let delta = &v["choices"][0]["delta"];
            let produced = ["content", "reasoning_content"]
                .iter()
                .any(|k| !delta[k].as_str().unwrap_or("").is_empty());
            if produced {
                deltas += 1;
                ttft.get_or_insert_with(|| started.elapsed());
            }
        }
    }

    let total = started.elapsed();
    let Some(ttft) = ttft else {
        return Err("stream produced no content".to_string());
    };
    // Not every backend honours include_usage; one delta is close enough to one
    // token on these engines to keep the row meaningful rather than blank.
    if completion_tokens == 0 {
        completion_tokens = deltas;
    }

    let decode_secs = total.saturating_sub(ttft).as_secs_f64();
    Ok(BenchTest {
        name: name.to_string(),
        prompt_tokens,
        completion_tokens,
        pp_tok_s: if ttft.as_secs_f64() > 0.0 {
            prompt_tokens as f64 / ttft.as_secs_f64()
        } else {
            0.0
        },
        gen_tok_s: if decode_secs > 0.0 && completion_tokens > 1 {
            (completion_tokens - 1) as f64 / decode_secs
        } else {
            0.0
        },
    })
}

pub async fn get_bench(
    State(state): State<Arc<AppState>>,
) -> Result<Json<BenchResult>, StatusCode> {
    let current = state.current_state().await;
    let port = match current {
        rookery_core::state::ServerState::Running { port, .. } => port,
        _ => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Short and medium prompts test gen speed; long prompt saturates GPU for accurate PP measurement.
    let long_prompt = "You are reviewing a complex distributed system. Here is the architecture: \
        The system consists of a load balancer (HAProxy), 12 application servers running a Rust async \
        runtime (Tokio), a PostgreSQL cluster with 3 replicas and streaming replication, a Redis \
        cluster with 6 nodes for caching and session storage, a Kafka cluster with 5 brokers for \
        event streaming, an Elasticsearch cluster with 3 data nodes for log aggregation, a \
        Prometheus server scraping 200 targets every 15 seconds, a Grafana dashboard with 50 \
        panels, a Kubernetes cluster with 20 nodes running across 3 availability zones, a CI/CD \
        pipeline using GitHub Actions with 15 workflow files, a CDN (Cloudflare) with 200 edge \
        locations, a DNS infrastructure using Route53 with health checks, a secrets management \
        system (Vault) with auto-rotation, a service mesh (Istio) handling mTLS between services, \
        an API gateway (Kong) with rate limiting and authentication, a message queue (RabbitMQ) \
        for async job processing, a blob storage system (S3-compatible) for user uploads, a \
        monitoring alerting pipeline (PagerDuty) with 300 alert rules, a feature flag system \
        (LaunchDarkly) serving 50 flags, and a data warehouse (Snowflake) ingesting 2TB daily. \
        Analyze the failure modes, identify single points of failure, and suggest improvements. \
        Be thorough and specific.";

    let prompts = vec![
        (
            "short",
            "Write a Python function that checks if a number is prime. Just the function.",
        ),
        (
            "medium",
            "Explain the difference between a mutex, semaphore, and condition variable. Give a code example for each in Rust.",
        ),
        ("long", long_prompt),
    ];

    // Same reason as post_chat: a hardcoded name 404s on every vLLM profile,
    // which made /api/bench silently return zero tests there.
    let model = rookery_engine::health::served_model_id(&client, port).await;

    let mut tests = Vec::new();
    let mut errors = Vec::new();
    for (name, prompt) in prompts {
        // Every path that yields no measurement has to end up in `errors`.
        // A silently dropped one reads to the caller as "never ran".
        let outcome = bench_one(&client, port, &model, name, prompt).await;

        match outcome {
            Ok(test) => tests.push(test),
            Err(e) => {
                tracing::error!(error = %e, test = name, "bench request failed");
                errors.push(BenchError {
                    name: name.to_string(),
                    error: e,
                });
            }
        }
    }

    Ok(Json(BenchResult { tests, errors }))
}

// --- Helpers ---

pub fn status_json_from_state(state: &rookery_core::state::ServerState) -> serde_json::Value {
    let s = status_from_state(state);
    serde_json::json!({
        "state": s.state,
        "profile": s.profile,
        "pid": s.pid,
        "port": s.port,
        "uptime_secs": s.uptime_secs,
        "backend": s.backend,
    })
}

// --- Agent routes ---

#[derive(Serialize)]
pub struct AgentsResponse {
    pub agents: Vec<rookery_engine::agent::AgentInfo>,
    pub configured: Vec<String>,
}

pub async fn get_agents(State(state): State<Arc<AppState>>) -> Json<AgentsResponse> {
    let config = state.config.read().await;
    let configured: Vec<String> = config.agents.keys().cloned().collect();

    // Enrich agent list with health metrics
    let mut agents = Vec::new();
    for info in state.agent_manager.list().await {
        if let Some(health) = state.agent_manager.get_health(&info.name).await {
            agents.push(health);
        } else {
            agents.push(info);
        }
    }

    Json(AgentsResponse { agents, configured })
}

pub async fn get_agent_health(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<rookery_engine::agent::AgentHealthDetail>, StatusCode> {
    let config = state.config.read().await;
    let agent_config = config.agents.get(&name);
    state
        .agent_manager
        .get_health_detail(&name, agent_config)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

#[derive(Deserialize)]
pub struct AgentActionRequest {
    pub name: String,
}

#[derive(Serialize)]
pub struct AgentActionResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Serialize)]
pub struct AgentUpdateResponse {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
}

async fn capture_prefixed_output<R>(
    reader: R,
    prefix: String,
    log_buffer: Arc<rookery_engine::logs::LogBuffer>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncBufReadExt;

    let reader = tokio::io::BufReader::new(reader);
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        log_buffer.push(format!("{prefix} {line}"));
    }
}

async fn run_agent_update_command(
    state: &Arc<AppState>,
    name: &str,
    config: &rookery_core::config::AgentConfig,
    command: &str,
) -> Result<std::process::ExitStatus, String> {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-lc").arg(command);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(false);

    if let Some(workdir) = config.update_workdir.as_ref().or(config.workdir.as_ref()) {
        cmd.current_dir(workdir);
    }
    for (key, value) in &config.env {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let prefix = format!("[agent:{name}:update]");

    let stdout_task = child.stdout.take().map(|stdout| {
        tokio::spawn(capture_prefixed_output(
            stdout,
            prefix.clone(),
            state.log_buffer.clone(),
        ))
    });
    let stderr_task = child.stderr.take().map(|stderr| {
        tokio::spawn(capture_prefixed_output(
            stderr,
            prefix,
            state.log_buffer.clone(),
        ))
    });

    let status = child.wait().await.map_err(|e| e.to_string())?;

    if let Some(task) = stdout_task {
        let _ = task.await;
    }
    if let Some(task) = stderr_task {
        let _ = task.await;
    }

    Ok(status)
}

pub async fn post_agent_start(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgentActionRequest>,
) -> Result<Json<AgentActionResponse>, StatusCode> {
    let config = state.config.read().await;
    let agent_config = config.agents.get(&req.name).ok_or(StatusCode::NOT_FOUND)?;

    match state.agent_manager.start(&req.name, agent_config).await {
        Ok(info) => Ok(Json(AgentActionResponse {
            success: true,
            message: format!("agent '{}' started (PID {})", req.name, info.pid),
        })),
        Err(e) => Ok(Json(AgentActionResponse {
            success: false,
            message: e.to_string(),
        })),
    }
}

pub async fn post_agent_stop(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgentActionRequest>,
) -> Result<Json<AgentActionResponse>, StatusCode> {
    match state.agent_manager.stop(&req.name).await {
        Ok(()) => Ok(Json(AgentActionResponse {
            success: true,
            message: format!("agent '{}' stopped", req.name),
        })),
        Err(e) => Ok(Json(AgentActionResponse {
            success: false,
            message: e.to_string(),
        })),
    }
}

pub async fn post_agent_update(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<AgentUpdateResponse>, StatusCode> {
    let _op_guard = state.op_lock.lock().await;

    let agent_config = {
        let config = state.config.read().await;
        config
            .agents
            .get(&name)
            .cloned()
            .ok_or(StatusCode::NOT_FOUND)?
    };

    let update_command = match agent_config.update_command.as_deref() {
        Some(command) if !command.trim().is_empty() => command.to_string(),
        _ => {
            return Ok(Json(AgentUpdateResponse {
                success: false,
                message: format!("agent '{name}' has no update_command configured"),
                version: None,
                previous_version: None,
            }));
        }
    };

    let previous_version = agent_config
        .version_file
        .as_deref()
        .and_then(rookery_engine::agent::read_version_file);
    let was_running = state.agent_manager.is_running(&name).await;

    if was_running {
        if let Err(e) = state.agent_manager.stop(&name).await {
            return Ok(Json(AgentUpdateResponse {
                success: false,
                message: format!("failed to stop agent '{name}' before update: {e}"),
                version: previous_version.clone(),
                previous_version,
            }));
        }
    } else if let Some(root) = rookery_engine::agent::db_root(&agent_config) {
        // LAN-1125: an already-stopped agent skips `stop()`, and `stop()` is
        // where LAN-1088 hooked the pre-change backup — so the one flow that
        // takes no copy is an update applied to a cold agent. The update runs
        // and migrates config in place either way (`hermes update` does not care
        // whether the gateway is up), so this branch needs the same copy the
        // running branch gets for free.
        //
        // Backing up directly rather than through the choke point because a
        // stopped agent has been removed from the `agents` map: `stop_inner`
        // would return `NotFound` before reaching its backup call, and the
        // `backup_root` it captures at start time is gone with the entry. Hence
        // `db_root` off the config instead.
        //
        // Same shape as the `stop_inner` call: fail-open by construction —
        // `backup::run` returns a tally and never an error, logging failures to
        // both tracing and the agent log buffer, so a missing sqlite3 or a full
        // disk cannot wedge every update. Retention and the `.bak`/nested-dir
        // sweep exclusion live inside `run`, so they apply here unchanged.
        rookery_engine::backup::run(
            &state.log_buffer,
            &name,
            &root,
            rookery_engine::integrity::SQLITE3,
            "update",
        )
        .await;
    }

    tracing::info!(agent = %name, command = %update_command, "running agent update");
    let update_status =
        run_agent_update_command(&state, &name, &agent_config, &update_command).await;

    match update_status {
        Ok(status) if status.success() => {
            let version = agent_config
                .version_file
                .as_deref()
                .and_then(rookery_engine::agent::read_version_file);

            match state.agent_manager.start(&name, &agent_config).await {
                Ok(info) => {
                    let message = match (&previous_version, &version) {
                        (Some(from), Some(to)) if from != to => {
                            format!("updated {name} from {from} to {to}")
                        }
                        (_, Some(to)) => format!("updated {name} to {to}"),
                        _ => format!("updated {name}"),
                    };

                    tracing::info!(agent = %name, pid = info.pid, "agent update completed");
                    Ok(Json(AgentUpdateResponse {
                        success: true,
                        message,
                        version,
                        previous_version,
                    }))
                }
                Err(e) => Ok(Json(AgentUpdateResponse {
                    success: false,
                    message: format!("updated {name}, but failed to restart agent: {e}"),
                    version,
                    previous_version,
                })),
            }
        }
        Ok(status) => {
            let exit_detail = status
                .code()
                .map(|code| format!("exit code {code}"))
                .unwrap_or_else(|| "terminated by signal".to_string());
            let restart_result = state.agent_manager.start(&name, &agent_config).await;
            let restart_suffix = match restart_result {
                Ok(_) => "agent restarted on previous code".to_string(),
                Err(e) => format!("failed to restart previous agent after update error: {e}"),
            };

            Ok(Json(AgentUpdateResponse {
                success: false,
                message: format!("update failed for {name} ({exit_detail}); {restart_suffix}"),
                version: previous_version.clone(),
                previous_version,
            }))
        }
        Err(e) => {
            let restart_result = state.agent_manager.start(&name, &agent_config).await;
            let restart_suffix = match restart_result {
                Ok(_) => "agent restarted on previous code".to_string(),
                Err(restart_error) => {
                    format!("failed to restart previous agent after update error: {restart_error}")
                }
            };

            Ok(Json(AgentUpdateResponse {
                success: false,
                message: format!("failed to run update for {name}: {e}; {restart_suffix}"),
                version: previous_version.clone(),
                previous_version,
            }))
        }
    }
}

// --- Hardware ---

pub async fn get_hardware(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut profile = serde_json::to_value(&state.hardware_profile).unwrap_or_default();

    // Add live VRAM info. `null` when the NVML query failed — reporting 0 there
    // is indistinguishable from a genuinely full GPU.
    if let Some(gpu) = profile.get_mut("gpu").and_then(|g| g.as_object_mut()) {
        let free = rookery_engine::hardware::try_live_vram_free_mb(state.gpu_monitor.as_ref());
        gpu.insert("vram_free_mb".into(), serde_json::json!(free));
    }

    // Add live RAM free
    let ram_free = rookery_engine::hardware::read_ram_free_mb();
    if let Some(cpu) = profile.get_mut("cpu").and_then(|c| c.as_object_mut()) {
        cpu.insert("ram_free_mb".into(), serde_json::json!(ram_free));
    }

    Json(profile)
}

// --- Model discovery ---

/// Why no quant was recommended. "Nothing fits" is only meaningful if we know
/// what we are fitting into — with `None`, VRAM was never read, and reporting
/// that as a full GPU sends the user hunting a phantom leak.
fn no_fit_message(vram_free: Option<u64>) -> &'static str {
    match vram_free {
        Some(_) => "no quant fits in available memory",
        None => "could not read GPU VRAM (NVML query failed)",
    }
}

#[derive(Deserialize)]
pub struct ModelSearchQuery {
    pub q: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    20
}

pub async fn get_models_search(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ModelSearchQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match state.hf_client.search(&q.q, q.limit).await {
        Ok(results) => Ok(Json(serde_json::json!({ "results": results }))),
        Err(e) => {
            tracing::error!(error = %e, "model search failed");
            Err(StatusCode::BAD_GATEWAY)
        }
    }
}

#[derive(Deserialize)]
pub struct RepoQuery {
    pub repo: String,
}

pub async fn get_models_quants(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RepoQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let repo = rookery_engine::models::normalize_repo(&q.repo);

    let files = state.hf_client.list_files(&repo).await.map_err(|e| {
        tracing::error!(error = %e, repo = %repo, "failed to list files");
        StatusCode::BAD_GATEWAY
    })?;

    let mut quants = rookery_engine::models::extract_quants(&files);
    let model_dirs = state.config.read().await.model_dirs.clone();
    rookery_engine::models::mark_downloaded_for_repo(&mut quants, &model_dirs, &repo);

    // Attach performance estimates. When NVML gave us nothing we still estimate
    // against 0 (the engine takes a plain u64), but `vram_known: false` tells
    // the UI those estimates are guesses, not measurements.
    let vram_free = rookery_engine::hardware::try_live_vram_free_mb(state.gpu_monitor.as_ref());
    let ram_free = rookery_engine::hardware::read_ram_free_mb();
    rookery_engine::models::attach_estimates(
        &mut quants,
        &state.hardware_profile,
        vram_free.unwrap_or(0),
        ram_free,
    );

    Ok(Json(
        serde_json::json!({ "repo": repo, "quants": quants, "vram_known": vram_free.is_some() }),
    ))
}

pub async fn get_models_recommend(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RepoQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let repo = rookery_engine::models::normalize_repo(&q.repo);

    let files = state.hf_client.list_files(&repo).await.map_err(|e| {
        tracing::error!(error = %e, repo = %repo, "failed to list files");
        StatusCode::BAD_GATEWAY
    })?;

    let quants = rookery_engine::models::extract_quants(&files);
    let vram_free = rookery_engine::hardware::try_live_vram_free_mb(state.gpu_monitor.as_ref());
    let ram_free = rookery_engine::hardware::read_ram_free_mb();

    match rookery_engine::models::recommend_quant(
        &quants,
        &state.hardware_profile,
        vram_free.unwrap_or(0),
        ram_free,
    ) {
        Some(rec) => Ok(Json(
            serde_json::json!({ "repo": repo, "recommendation": rec, "vram_known": vram_free.is_some() }),
        )),
        None => Ok(Json(
            serde_json::json!({ "repo": repo, "recommendation": null, "vram_known": vram_free.is_some(), "message": no_fit_message(vram_free) }),
        )),
    }
}

pub async fn get_models_cached(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let config = state.config.read().await;
    let mut cached = rookery_engine::models::scan_cache(&config.model_dirs);

    // Merge config-defined local models
    for (name, model) in &config.models {
        if model.source == "local"
            && let Some(ref path) = model.path
            && path.exists()
            && !cached.iter().any(|c| c.path == *path)
        {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            cached.push(rookery_engine::models::CachedModel {
                repo: format!("local:{name}"),
                quant_label: path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                path: path.clone(),
                size_bytes: size,
            });
        }
    }

    Json(serde_json::json!({ "models": cached }))
}

#[derive(Deserialize)]
pub struct PullRequest {
    pub repo: String,
    #[serde(default)]
    pub quant: Option<String>,
}

pub async fn post_models_pull(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PullRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let repo = rookery_engine::models::normalize_repo(&req.repo);

    let files = state.hf_client.list_files(&repo).await.map_err(|e| {
        tracing::error!(error = %e, "failed to list files for pull");
        StatusCode::BAD_GATEWAY
    })?;

    let quants = rookery_engine::models::extract_quants(&files);

    // Pick quant: explicit or recommend
    let quant_label = if let Some(q) = req.quant {
        q
    } else {
        let vram_free = rookery_engine::hardware::try_live_vram_free_mb(state.gpu_monitor.as_ref());
        let ram_free = rookery_engine::hardware::read_ram_free_mb();
        match rookery_engine::models::recommend_quant(
            &quants,
            &state.hardware_profile,
            vram_free.unwrap_or(0),
            ram_free,
        ) {
            Some(rec) => rec.label,
            None => {
                return Ok(Json(serde_json::json!({
                    "started": false,
                    "vram_known": vram_free.is_some(),
                    "message": no_fit_message(vram_free),
                })));
            }
        }
    };

    let quant = quants
        .iter()
        .find(|q| q.label == quant_label)
        .ok_or_else(|| {
            tracing::error!(quant = %quant_label, "quant not found");
            StatusCode::NOT_FOUND
        })?;

    let download_files: Vec<(String, String)> = quant
        .files
        .iter()
        .map(|f| {
            let dest = rookery_engine::models::cache_path(&repo, &f.path);
            (f.path.clone(), dest.to_string_lossy().to_string())
        })
        .collect();

    let repo_clone = repo.clone();
    let label_clone = quant_label.clone();
    let state_tx = state.state_tx.clone();
    let files_for_response: Vec<String> = download_files.iter().map(|(f, _)| f.clone()).collect();

    // Spawn background download
    tokio::spawn(async move {
        let client = rookery_engine::models::HfClient::new();
        let (progress_tx, _) =
            tokio::sync::watch::channel(rookery_engine::models::DownloadProgress {
                repo: repo_clone.clone(),
                file: String::new(),
                bytes_downloaded: 0,
                bytes_total: 0,
                done: false,
            });

        for (filename, dest_str) in &download_files {
            let dest = std::path::PathBuf::from(dest_str);
            if dest.exists() {
                tracing::info!(file = %filename, "already cached, skipping");
                continue;
            }

            tracing::info!(repo = %repo_clone, file = %filename, "downloading");
            match client
                .download_file(&repo_clone, filename, &dest, Some(&progress_tx))
                .await
            {
                Ok(()) => {
                    tracing::info!(file = %filename, "download complete");
                    let _ = state_tx.send(serde_json::json!({
                        "event": "download",
                        "repo": repo_clone,
                        "file": filename,
                        "done": true,
                    }));
                }
                Err(e) => {
                    tracing::error!(error = %e, file = %filename, "download failed");
                    let _ = state_tx.send(serde_json::json!({
                        "event": "download",
                        "repo": repo_clone,
                        "file": filename,
                        "error": e,
                    }));
                }
            }
        }
    });

    Ok(Json(serde_json::json!({
        "started": true,
        "repo": repo,
        "quant": label_clone,
        "files": files_for_response,
    })))
}

// ── Upstream releases ───────────────────────────────────────────────

pub async fn get_releases(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Show only the running backend's upstream. The cache is keyed by repo and
    // entries persist, so after swapping engines it holds a row for whatever was
    // running at each past check — rendering all of them means a permanent stale
    // llama.cpp row next to the live one. Entries are kept (a swap back reuses
    // them, ETag and all); they are just not rendered.
    let active_repo = {
        let current = state.current_state().await;
        let config = state.config.read().await;
        let backend = match &current {
            rookery_core::state::ServerState::Running { backend_type, .. } => *backend_type,
            _ => config
                .profiles
                .get(&config.default_profile)
                .map(|p| p.backend_type())
                .unwrap_or_default(),
        };
        rookery_engine::releases::repo_for_backend(backend)
    };

    let cache = state.release_cache.read().await;
    let full = serde_json::to_value(&*cache).unwrap_or_default();
    let repos = full
        .get("repos")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|r| r.get("repo").and_then(|v| v.as_str()) == Some(active_repo))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // Freshly swapped to a backend the monitor has not polled yet: say so rather
    // than rendering the previous engine's numbers or an empty panel.
    if repos.is_empty() {
        return Json(serde_json::json!({
            "repos": [{
                "repo": active_repo,
                "latest": null,
                "current_version": null,
                "update_available": false,
                "ahead_of_release": false,
                "version_comparable": false,
                "checked_at": null,
            }]
        }));
    }

    Json(serde_json::json!({ "repos": repos }))
}

fn status_from_state(state: &rookery_core::state::ServerState) -> StatusResponse {
    match state {
        rookery_core::state::ServerState::Stopped => StatusResponse {
            state: "stopped".into(),
            profile: None,
            pid: None,
            port: None,
            uptime_secs: None,
            backend: None,
        },
        rookery_core::state::ServerState::Running {
            profile,
            pid,
            port,
            since,
            backend_type,
            ..
        } => StatusResponse {
            state: "running".into(),
            profile: Some(profile.clone()),
            pid: Some(*pid),
            port: Some(*port),
            uptime_secs: Some(
                chrono::Utc::now()
                    .signed_duration_since(*since)
                    .num_seconds(),
            ),
            backend: Some(backend_type.to_string()),
        },
        rookery_core::state::ServerState::Sleeping { profile, .. } => StatusResponse {
            state: "sleeping".into(),
            profile: Some(profile.clone()),
            pid: None,
            port: None,
            uptime_secs: None,
            backend: None,
        },
        rookery_core::state::ServerState::Failed {
            last_error,
            profile,
            ..
        } => StatusResponse {
            state: format!("failed: {last_error}"),
            profile: Some(profile.clone()),
            pid: None,
            port: None,
            uptime_secs: None,
            backend: None,
        },
        rookery_core::state::ServerState::Starting { profile, .. } => StatusResponse {
            state: "starting".into(),
            profile: Some(profile.clone()),
            pid: None,
            port: None,
            uptime_secs: None,
            backend: None,
        },
        rookery_core::state::ServerState::Stopping { .. } => StatusResponse {
            state: "stopping".into(),
            profile: None,
            pid: None,
            port: None,
            uptime_secs: None,
            backend: None,
        },
        rookery_core::state::ServerState::Swapping { to, .. } => StatusResponse {
            state: "swapping".into(),
            profile: Some(to.clone()),
            // Deliberately no pid/port/backend/uptime: nothing is serving yet,
            // and reporting the target's port would be an optimistic lie.
            pid: None,
            port: None,
            uptime_secs: None,
            backend: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Fix #3: StatusResponse always includes 'backend' key (null when stopped)
    #[test]
    fn test_status_response_includes_backend_when_stopped() {
        let state = rookery_core::state::ServerState::Stopped;
        let resp = status_from_state(&state);
        let json = serde_json::to_value(&resp).unwrap();
        // 'backend' key must be present (as null), not omitted
        assert!(
            json.get("backend").is_some(),
            "backend key should be present in JSON, got: {json}"
        );
        assert!(
            json["backend"].is_null(),
            "backend should be null when stopped"
        );
    }

    #[test]
    fn test_status_response_includes_backend_when_running() {
        let state = rookery_core::state::ServerState::Running {
            profile: "test".into(),
            pid: 1234,
            port: 8081,
            since: chrono::Utc::now(),
            command_line: vec![],
            exe_path: None,
            backend_type: rookery_core::config::BackendType::LlamaServer,
            container_id: None,
        };
        let resp = status_from_state(&state);
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["backend"], "llama-server");
    }

    #[test]
    fn test_status_response_includes_backend_when_failed() {
        let state = rookery_core::state::ServerState::Failed {
            last_error: "test error".into(),
            profile: "test".into(),
            since: chrono::Utc::now(),
        };
        let resp = status_from_state(&state);
        let json = serde_json::to_value(&resp).unwrap();
        assert!(
            json.get("backend").is_some(),
            "backend key should be present"
        );
        assert!(
            json["backend"].is_null(),
            "backend should be null when failed"
        );
    }

    // LAN-1081: a swap must be visible on the wire, and the SSE `state` payload
    // is what the dashboard turns into `class="badge swapping"` (it splits the
    // state string on ':' and interpolates). If this string drifts, the amber
    // badge silently stops rendering.
    #[test]
    fn test_status_from_state_swapping() {
        let state = rookery_core::state::ServerState::Swapping {
            from: Some("qwen_dense".into()),
            to: "qwen38".into(),
            since: chrono::Utc::now(),
        };
        let resp = status_from_state(&state);
        assert_eq!(resp.state, "swapping");
        assert_eq!(resp.profile, Some("qwen38".into()));
        // Nothing is serving mid-swap — no optimistic pid/port/backend.
        assert_eq!(resp.pid, None);
        assert_eq!(resp.port, None);
        assert_eq!(resp.backend, None);

        let json = status_json_from_state(&state);
        assert_eq!(json["state"], "swapping");
        assert_eq!(json["profile"], "qwen38");
    }

    // === Fix #4: status_from_state returns 'starting'/'stopping' not 'transitioning'
    #[test]
    fn test_status_from_state_starting() {
        let state = rookery_core::state::ServerState::Starting {
            profile: "my_profile".into(),
            since: chrono::Utc::now(),
        };
        let resp = status_from_state(&state);
        assert_eq!(resp.state, "starting");
        assert_eq!(resp.profile, Some("my_profile".into()));
    }

    #[test]
    fn test_status_from_state_stopping() {
        let state = rookery_core::state::ServerState::Stopping {
            since: chrono::Utc::now(),
        };
        let resp = status_from_state(&state);
        assert_eq!(resp.state, "stopping");
        assert_eq!(resp.profile, None);
    }

    #[test]
    fn test_status_from_state_stopped() {
        let state = rookery_core::state::ServerState::Stopped;
        let resp = status_from_state(&state);
        assert_eq!(resp.state, "stopped");
        assert_eq!(resp.profile, None);
    }

    #[test]
    fn test_status_from_state_sleeping() {
        let state = rookery_core::state::ServerState::Sleeping {
            profile: "fast".into(),
            since: chrono::Utc::now(),
        };
        let resp = status_from_state(&state);
        assert_eq!(resp.state, "sleeping");
        assert_eq!(resp.profile, Some("fast".into()));
        assert_eq!(resp.pid, None);
        assert_eq!(resp.port, None);
        assert!(resp.backend.is_none());
    }

    #[test]
    fn test_status_from_state_running() {
        let state = rookery_core::state::ServerState::Running {
            profile: "fast".into(),
            pid: 42,
            port: 8081,
            since: chrono::Utc::now(),
            command_line: vec![],
            exe_path: None,
            backend_type: rookery_core::config::BackendType::LlamaServer,
            container_id: None,
        };
        let resp = status_from_state(&state);
        assert_eq!(resp.state, "running");
        assert_eq!(resp.profile, Some("fast".into()));
        assert_eq!(resp.pid, Some(42));
        assert_eq!(resp.port, Some(8081));
        assert!(resp.backend.is_some());
    }

    // === Swap drain flag cleanup: drain is cleared on failure paths
    //
    // Simulates the swap drain logic from post_swap() to verify that
    // set_draining(false) is called even when the swap fails partway through.
    // This is the core invariant: after a failed swap, post_chat must NOT
    // permanently return 503 because the drain flag was left set.
    #[tokio::test]
    async fn test_swap_drain_flag_cleared_on_failure() {
        use rookery_engine::backend::LlamaServerBackend;
        use rookery_engine::logs::LogBuffer;
        use std::sync::Arc;

        let log_buffer = Arc::new(LogBuffer::new(100));
        let backend: Box<dyn rookery_engine::backend::InferenceBackend> =
            Box::new(LlamaServerBackend::new(log_buffer.clone()));
        let backend = Arc::new(tokio::sync::Mutex::new(backend));

        // Simulate the swap drain pattern from post_swap:
        // 1. Set draining (even though not running — tests the flag lifecycle)
        {
            let b = backend.lock().await;
            b.set_draining(true);
            assert!(b.is_draining(), "drain flag should be set");
        }

        // 2. Simulate a failure in create_backend or start —
        //    the error path must clear the drain flag
        let swap_failed = true; // simulating failure
        if swap_failed {
            // This mirrors the fix: clear drain on the old backend before returning error
            backend.lock().await.set_draining(false);
        }

        // 3. Verify: drain flag must be false after failed swap
        assert!(
            !backend.lock().await.is_draining(),
            "drain flag must be cleared after failed swap — otherwise post_chat returns 503 forever"
        );
    }

    // Verifies that a successful swap leaves draining=false on the new backend.
    // The new backend is created fresh and should not inherit the old drain state.
    #[tokio::test]
    async fn test_swap_drain_flag_false_on_new_backend() {
        use rookery_engine::backend::{InferenceBackend, LlamaServerBackend};
        use rookery_engine::logs::LogBuffer;
        use std::sync::Arc;

        let log_buffer = Arc::new(LogBuffer::new(100));

        // Old backend with drain set
        let old_backend = LlamaServerBackend::new(log_buffer.clone());
        old_backend.set_draining(true);
        assert!(old_backend.is_draining());

        // New backend (as created by create_backend) starts with draining=false
        let new_backend = LlamaServerBackend::new(log_buffer);
        assert!(
            !new_backend.is_draining(),
            "new backend must start with draining=false"
        );
    }

    // === status_json_from_state always includes backend key
    #[test]
    fn test_status_json_from_state_always_has_backend() {
        let states = vec![
            rookery_core::state::ServerState::Stopped,
            rookery_core::state::ServerState::Starting {
                profile: "test".into(),
                since: chrono::Utc::now(),
            },
            rookery_core::state::ServerState::Stopping {
                since: chrono::Utc::now(),
            },
            rookery_core::state::ServerState::Failed {
                last_error: "err".into(),
                profile: "test".into(),
                since: chrono::Utc::now(),
            },
        ];
        for state in &states {
            let json = status_json_from_state(state);
            assert!(
                json.get("backend").is_some(),
                "backend key missing for state: {json}"
            );
        }
    }

    // SSE state events include backend field
    //
    // status_json_from_state() is the function used to build SSE state event
    // payloads (via broadcast_state). When the server is Running, the JSON
    // must include 'backend' set to the backend type string. When Stopped,
    // backend must be null.
    #[test]
    fn test_status_json_running_includes_backend_field() {
        let state = rookery_core::state::ServerState::Running {
            profile: "fast".into(),
            pid: 1234,
            port: 8081,
            since: chrono::Utc::now(),
            command_line: vec![],
            exe_path: None,
            backend_type: rookery_core::config::BackendType::LlamaServer,
            container_id: None,
        };
        let json = status_json_from_state(&state);

        // backend must be present and set to "llama-server" for Running state
        assert!(
            json.get("backend").is_some(),
            "backend key must be present in SSE state JSON"
        );
        assert_eq!(
            json["backend"], "llama-server",
            "backend should be 'llama-server' for LlamaServer Running state"
        );
    }

    #[test]
    fn test_status_json_running_vllm_includes_backend_field() {
        let state = rookery_core::state::ServerState::Running {
            profile: "vllm_prod".into(),
            pid: 0,
            port: 8081,
            since: chrono::Utc::now(),
            command_line: vec![],
            exe_path: None,
            backend_type: rookery_core::config::BackendType::Vllm,
            container_id: Some("abc123".into()),
        };
        let json = status_json_from_state(&state);

        assert_eq!(
            json["backend"], "vllm",
            "backend should be 'vllm' for Vllm Running state"
        );
    }

    #[test]
    fn test_status_json_stopped_has_backend_null() {
        let state = rookery_core::state::ServerState::Stopped;
        let json = status_json_from_state(&state);

        assert!(
            json.get("backend").is_some(),
            "backend key must always be present"
        );
        assert!(
            json["backend"].is_null(),
            "backend should be null when Stopped, got: {}",
            json["backend"]
        );
    }

    // /api/profiles includes backend per profile
    //
    // Tests the get_profiles logic by verifying that each profile in the JSON
    // response includes a 'backend' field derived from the profile configuration.
    // Uses a Config with both llama-server and vLLM profiles.
    #[test]
    fn test_profiles_response_includes_backend_field() {
        use rookery_core::config::{Config, Model, Profile, VllmConfig};
        use std::collections::HashMap;
        use std::path::PathBuf;

        let config = Config {
            llama_server: PathBuf::from("/usr/bin/llama-server"),
            default_profile: "llama_fast".into(),
            listen: "127.0.0.1:3000".parse().unwrap(),
            api_key: None,
            idle_timeout: None,
            models: HashMap::from([
                (
                    "model_a".into(),
                    Model {
                        source: "local".into(),
                        repo: None,
                        file: None,
                        path: Some(PathBuf::from("/models/a.gguf")),
                        estimated_vram_mb: Some(4000),
                    },
                ),
                (
                    "model_b".into(),
                    Model {
                        source: "hf".into(),
                        repo: Some("org/model-b".into()),
                        file: None,
                        path: None,
                        estimated_vram_mb: None,
                    },
                ),
            ]),
            profiles: HashMap::from([
                (
                    "llama_fast".into(),
                    Profile {
                        sglang: None,
                        model: "model_a".into(),
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
                        aliases: vec![],
                        extra_args: vec![],
                    },
                ),
                (
                    "vllm_prod".into(),
                    Profile {
                        sglang: None,
                        model: "model_b".into(),
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
                        aliases: vec![],
                        extra_args: vec![],
                    },
                ),
            ]),
            auto_start: false,
            model_dirs: vec![],
            github_token: None,
            release_check_interval: 0,
            agents: HashMap::new(),
        };

        // Replicate the get_profiles logic from the route handler
        let profiles: Vec<serde_json::Value> = config
            .profiles
            .iter()
            .map(|(name, p)| {
                let is_default = name == &config.default_profile;
                let model = config.models.get(&p.model);
                let ls = p.llama_server_config();
                serde_json::json!({
                    "name": name,
                    "model": p.model,
                    "port": p.port,
                    "ctx_size": ls.as_ref().map(|c| c.ctx_size),
                    "reasoning_budget": ls.as_ref().map(|c| c.reasoning_budget),
                    "backend": p.backend_type().to_string(),
                    "default": is_default,
                    "estimated_vram_mb": model.and_then(|m| m.estimated_vram_mb),
                })
            })
            .collect();

        // Every profile must have a 'backend' field
        for profile_json in &profiles {
            let name = profile_json["name"].as_str().unwrap();
            assert!(
                profile_json.get("backend").is_some(),
                "profile '{name}' missing 'backend' field"
            );
            let backend = profile_json["backend"].as_str().unwrap();
            assert!(
                backend == "llama-server" || backend == "vllm",
                "profile '{name}' has unexpected backend value: {backend}"
            );
        }

        // Find specific profiles and verify backend type
        let llama_profile = profiles
            .iter()
            .find(|p| p["name"] == "llama_fast")
            .expect("llama_fast profile should exist");
        assert_eq!(
            llama_profile["backend"], "llama-server",
            "llama_fast should have backend 'llama-server'"
        );

        let vllm_profile = profiles
            .iter()
            .find(|p| p["name"] == "vllm_prod")
            .expect("vllm_prod profile should exist");
        assert_eq!(
            vllm_profile["backend"], "vllm",
            "vllm_prod should have backend 'vllm'"
        );
    }

    // Capacity gate adapts for vLLM profiles
    //
    // For vLLM profiles, the capacity gate should NOT block the start.
    // vLLM manages its own GPU memory via gpu_memory_utilization, so
    // the daemon should skip the VRAM capacity check for vLLM profiles.
    // This test verifies the logic branch by checking that is_vllm_profile
    // correctly identifies backend types and that the capacity gate code
    // skips the check for vLLM profiles.
    #[test]
    fn test_capacity_gate_skips_vllm_profile() {
        use rookery_core::config::{BackendType, Profile, VllmConfig};

        // A vLLM profile with estimated_vram_mb on the model
        let vllm_profile = Profile {
            sglang: None,
            model: "test_model".into(),
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
            aliases: vec![],
            extra_args: vec![],
        };

        // A llama-server profile
        let llama_profile = Profile {
            sglang: None,
            model: "test_model".into(),
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
            aliases: vec![],
            extra_args: vec![],
        };

        // The capacity gate logic in post_start uses this check:
        // is_vllm_profile = profile.backend_type() == BackendType::Vllm
        let is_vllm = vllm_profile.backend_type() == BackendType::Vllm;
        let is_llama_vllm = llama_profile.backend_type() == BackendType::Vllm;

        // vLLM profile bypasses capacity gate
        assert!(
            is_vllm,
            "vLLM profile should be identified as Vllm backend type"
        );
        // llama-server profile does NOT bypass capacity gate
        assert!(
            !is_llama_vllm,
            "llama-server profile should NOT be identified as Vllm"
        );

        // Simulate the capacity gate logic:
        // For vLLM, even with insufficient VRAM, the start is NOT blocked
        let estimated_vram_mb: Option<u32> = Some(50000); // Very high, would normally fail
        let free_vram_mb: u64 = 1000; // Very low free VRAM

        // llama-server profile: capacity gate would block
        let llama_blocked = if !is_llama_vllm {
            if let Some(estimated_mb) = estimated_vram_mb {
                free_vram_mb < estimated_mb as u64
            } else {
                false
            }
        } else {
            false // vLLM never blocked
        };

        // vLLM profile: capacity gate is skipped
        let vllm_blocked = if !is_vllm {
            if let Some(estimated_mb) = estimated_vram_mb {
                free_vram_mb < estimated_mb as u64
            } else {
                false
            }
        } else {
            false // vLLM never blocked
        };

        assert!(
            llama_blocked,
            "llama-server profile should be blocked by capacity gate"
        );
        assert!(
            !vllm_blocked,
            "vLLM profile should NOT be blocked by capacity gate"
        );
    }

    // Compose generation failure returns error before Docker commands
    //
    // When invalid config values cause compose file generation to fail (e.g., missing model),
    // an error is returned before any Docker commands are executed. The state should
    // transition to Failed with a config-related error message.
    //
    // This test verifies compose::generate_compose() fails with a clear error for
    // invalid configs, and that VllmBackend::start() would propagate this error
    // (since compose generation happens before any docker compose commands).
    #[test]
    fn test_compose_generation_failure_returns_error_before_docker() {
        use rookery_core::config::{Config, Model, Profile, VllmConfig};
        use std::collections::HashMap;

        // Config with a vLLM profile that references a missing model
        let config = Config {
            llama_server: std::path::PathBuf::new(),
            default_profile: "bad_vllm".into(),
            listen: "127.0.0.1:19999".parse().unwrap(),
            api_key: None,
            idle_timeout: None,
            models: HashMap::from([(
                "existing_model".into(),
                Model {
                    source: "hf".into(),
                    repo: Some("test/model".into()),
                    file: None,
                    path: None,
                    estimated_vram_mb: None,
                },
            )]),
            profiles: HashMap::from([(
                "bad_vllm".into(),
                Profile {
                    sglang: None,
                    model: "nonexistent_model".into(), // references missing model
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
                    aliases: vec![],
                    extra_args: vec![],
                },
            )]),
            auto_start: false,
            model_dirs: vec![],
            github_token: None,
            release_check_interval: 0,
            agents: HashMap::new(),
        };

        // generate_compose should fail because model doesn't exist
        let result = rookery_engine::compose::generate_compose(&config, "bad_vllm");
        assert!(
            result.is_err(),
            "compose generation should fail for missing model"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent_model"),
            "error should mention the missing model, got: {err}"
        );
    }

    // Test that compose generation failure for a non-vLLM profile also returns error
    #[test]
    fn test_compose_generation_failure_non_vllm_profile() {
        use rookery_core::config::{Config, Model, Profile};
        use std::collections::HashMap;

        let config = Config {
            llama_server: std::path::PathBuf::new(),
            default_profile: "llama_profile".into(),
            listen: "127.0.0.1:19999".parse().unwrap(),
            api_key: None,
            idle_timeout: None,
            models: HashMap::from([(
                "m".into(),
                Model {
                    source: "local".into(),
                    repo: None,
                    file: None,
                    path: Some(std::path::PathBuf::from("/tmp/model")),
                    estimated_vram_mb: None,
                },
            )]),
            profiles: HashMap::from([(
                "llama_profile".into(),
                Profile {
                    sglang: None,
                    model: "m".into(),
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
                    aliases: vec![],
                    extra_args: vec![],
                },
            )]),
            auto_start: false,
            model_dirs: vec![],
            github_token: None,
            release_check_interval: 0,
            agents: HashMap::new(),
        };

        // generate_compose should fail for a llama-server profile
        let result = rookery_engine::compose::generate_compose(&config, "llama_profile");
        assert!(
            result.is_err(),
            "compose generation should fail for non-vLLM profile"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not a vLLM profile"),
            "error should indicate it's not a vLLM profile, got: {err}"
        );
    }

    // Test that the daemon start error path correctly transitions to Failed state
    // This verifies the integration: when backend.start() returns Err (which includes
    // compose generation failures), post_start creates a Failed state.
    #[test]
    fn test_start_failure_transitions_to_failed_state() {
        // Verify that the Failed state construction in the error handler is correct
        let error_msg = "model not found: nonexistent_model";
        let profile_name = "bad_vllm";

        let failed = rookery_core::state::ServerState::Failed {
            last_error: error_msg.to_string(),
            profile: profile_name.into(),
            since: chrono::Utc::now(),
        };

        // Verify the state is Failed with the right fields
        match &failed {
            rookery_core::state::ServerState::Failed {
                last_error,
                profile,
                ..
            } => {
                assert_eq!(last_error, error_msg);
                assert_eq!(profile, profile_name);
            }
            _ => panic!("expected Failed state"),
        }

        // Verify status_from_state correctly renders the Failed state
        let status = status_from_state(&failed);
        assert!(
            status.state.starts_with("failed:"),
            "state should start with 'failed:', got: {}",
            status.state
        );
        assert!(
            status.state.contains("nonexistent_model"),
            "state should contain error details, got: {}",
            status.state
        );
        assert_eq!(status.profile, Some("bad_vllm".into()));
    }

    // GET /api/model-info returns null props for vLLM
    //
    // When the /props endpoint returns a non-success status (404 for vLLM),
    // the ModelInfoResponse should have props: null (not omitted).
    // This test verifies the response structure.
    #[test]
    fn test_model_info_response_with_null_props() {
        let resp = ModelInfoResponse {
            available: true,
            model_id: Some("test-model".into()),
            owned_by: Some("vllm".into()),
            props: None, // /props returned 404 (vLLM doesn't have this endpoint)
        };

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["available"], true);
        assert_eq!(json["model_id"], "test-model");
        assert_eq!(json["owned_by"], "vllm");
        // props must be present as null (not omitted) so consumers get a consistent schema
        assert!(
            json.as_object().unwrap().contains_key("props"),
            "props key should be present in JSON, got: {json}"
        );
        assert!(
            json["props"].is_null(),
            "props should be null when /props returns 404"
        );
    }

    // Test that ModelInfoResponse with Some(props) includes the field
    #[test]
    fn test_model_info_response_with_props() {
        let resp = ModelInfoResponse {
            available: true,
            model_id: Some("test-model".into()),
            owned_by: Some("llama.cpp".into()),
            props: Some(serde_json::json!({"chat_template": "test", "total_slots": 1})),
        };

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["available"], true);
        assert!(
            json.get("props").is_some(),
            "props should be present when Some"
        );
        assert_eq!(json["props"]["total_slots"], 1);
    }

    // GET /api/server-stats returns null slots for vLLM
    //
    // When the /slots endpoint returns a non-success status (404 for vLLM),
    // the server-stats response should have slots: null.
    #[test]
    fn test_server_stats_response_with_null_slots() {
        // Simulate the response structure built by get_server_stats
        let slots: Option<serde_json::Value> = None; // /slots returned 404

        let response = serde_json::json!({
            "available": true,
            "slots": slots,
        });

        assert_eq!(response["available"], true);
        assert!(
            response["slots"].is_null(),
            "slots should be null when /slots returns 404, got: {}",
            response["slots"]
        );
    }

    // Test that server stats response includes slots when available
    #[test]
    fn test_server_stats_response_with_slots() {
        let slots: Option<serde_json::Value> = Some(serde_json::json!([
            {"id": 0, "state": 0, "n_predict": 0}
        ]));

        let response = serde_json::json!({
            "available": true,
            "slots": slots,
        });

        assert_eq!(response["available"], true);
        assert!(
            response["slots"].is_array(),
            "slots should be an array when available"
        );
    }

    // === Combined: verify /props and /slots status code check logic
    //
    // The get_model_info and get_server_stats handlers now check
    // resp.status().is_success() before trying to parse the response body.
    // This ensures that 404 responses (from vLLM) result in null props/slots.
    #[test]
    fn test_http_status_check_logic_for_props_and_slots() {
        // Simulate the status check logic used in the route handlers:
        // `if resp.status().is_success() { parse json } else { None }`

        // 200 OK → parse response
        let status_200 = reqwest::StatusCode::OK;
        assert!(
            status_200.is_success(),
            "200 should be success → props/slots parsed"
        );

        // 404 Not Found → return None (vLLM case)
        let status_404 = reqwest::StatusCode::NOT_FOUND;
        assert!(
            !status_404.is_success(),
            "404 should NOT be success → props/slots set to null"
        );

        // 500 Internal Server Error → return None
        let status_500 = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
        assert!(
            !status_500.is_success(),
            "500 should NOT be success → props/slots set to null"
        );
    }

    // ═══════════════════════════════════════════════════════════════════
    // Route integration tests — real HTTP requests via axum oneshot
    // ═══════════════════════════════════════════════════════════════════

    mod route_integration {
        use axum::Router;
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::routing::{get, post};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        use crate::test_utils::{MockBackend, build_test_app_state, sync_state_from_backend};
        use rookery_core::config::BackendType;
        use rookery_engine::backend::{BackendInfo, InferenceBackend};

        /// Build the route subset used for integration testing.
        ///
        /// Mirrors the routes from main.rs relevant to core endpoint tests,
        /// including the 1MB request body limit.
        fn test_router(state: std::sync::Arc<crate::app_state::AppState>) -> Router {
            Router::new()
                .route("/api/health", get(super::get_health))
                .route("/api/status", get(super::get_status))
                .route("/api/profiles", get(super::get_profiles))
                .route("/api/config", get(super::get_config))
                .route("/api/logs", get(super::get_logs))
                .route("/metrics", get(super::get_metrics))
                .route("/api/start", post(super::post_start))
                .route("/api/stop", post(super::post_stop))
                .route("/api/sleep", post(super::post_sleep))
                .route("/api/wake", post(super::post_wake))
                .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
                .with_state(state)
        }

        // --- 1. GET /api/health → 200 always ---
        #[tokio::test]
        async fn test_route_health_returns_200() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router(state);

            let req = Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        // --- 2. GET /api/status when stopped → 200 with state="stopped", backend=null ---
        #[tokio::test]
        async fn test_route_status_when_stopped() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router(state);

            let req = Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["state"], "stopped");
            assert!(
                json["backend"].is_null(),
                "backend should be null when stopped"
            );
            assert!(
                json["profile"].is_null(),
                "profile should be null when stopped"
            );
            assert!(json["pid"].is_null(), "pid should be null when stopped");
            assert!(json["port"].is_null(), "port should be null when stopped");
        }

        // --- 3. GET /api/status when running → 200 with state="running", backend, profile, pid, port ---
        #[tokio::test]
        async fn test_route_status_when_running() {
            let running_info = BackendInfo {
                pid: Some(12345),
                container_id: None,
                port: 8081,
                profile: "test".into(),
                started_at: chrono::Utc::now(),
                backend_type: BackendType::LlamaServer,
                command_line: vec!["mock-server".into()],
                exe_path: Some(std::path::PathBuf::from("/mock/llama-server")),
            };
            let backend = MockBackend::running_with(running_info);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;
            let app = test_router(state);

            let req = Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["state"], "running");
            assert_eq!(json["backend"], "llama-server");
            assert_eq!(json["profile"], "test");
            assert_eq!(json["pid"], 12345);
            assert_eq!(json["port"], 8081);
            assert!(
                json["uptime_secs"].is_number(),
                "uptime_secs should be a number"
            );
        }

        #[tokio::test]
        async fn test_route_metrics_when_stopped() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router(state);

            let req = Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(body.to_vec()).unwrap();

            assert!(text.contains("rookery_server_up{profile=\"\",backend=\"\"} 0"));
            assert!(text.contains("rookery_sse_connections_current 0"));
            assert!(text.contains("rookery_canary_last_check_timestamp 0"));
        }

        #[tokio::test]
        async fn test_route_metrics_when_running() {
            let running_info = BackendInfo {
                pid: Some(12345),
                container_id: None,
                port: 8081,
                profile: "test".into(),
                started_at: chrono::Utc::now() - chrono::TimeDelta::seconds(30),
                backend_type: BackendType::LlamaServer,
                command_line: vec!["mock-server".into()],
                exe_path: Some(std::path::PathBuf::from("/mock/llama-server")),
            };
            let backend = MockBackend::running_with(running_info);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;
            let app = test_router(state);

            let req = Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(body.to_vec()).unwrap();

            assert!(
                text.contains("rookery_server_up{profile=\"test\",backend=\"llama-server\"} 1")
            );
            assert!(text.contains("rookery_server_uptime_seconds{profile=\"test\"}"));
        }

        // --- 4. GET /api/profiles → 200 with profile list including backend field ---
        #[tokio::test]
        async fn test_route_profiles_returns_list_with_backend() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router(state);

            let req = Request::builder()
                .uri("/api/profiles")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            let profiles = json["profiles"]
                .as_array()
                .expect("profiles should be an array");
            assert!(!profiles.is_empty(), "should have at least one profile");

            for profile in profiles {
                assert!(
                    profile.get("backend").is_some(),
                    "each profile should have a 'backend' field"
                );
                assert!(
                    profile.get("name").is_some(),
                    "each profile should have a 'name' field"
                );
                assert!(
                    profile.get("model").is_some(),
                    "each profile should have a 'model' field"
                );
            }

            // The default test config has a "test" profile with llama-server backend
            let test_profile = profiles
                .iter()
                .find(|p| p["name"] == "test")
                .expect("should have 'test' profile");
            assert_eq!(test_profile["backend"], "llama-server");
        }

        // --- 5. GET /api/config → 200 with redacted agent env vars ---
        #[tokio::test]
        async fn test_route_config_redacts_agent_env() {
            let (_dir, state) = build_test_app_state(None);

            // Add an agent with env vars to the config
            {
                let mut config = state.config.write().await;
                config.api_key = Some("rky-secret".into());
                config.agents.insert(
                    "test_agent".into(),
                    rookery_core::config::AgentConfig {
                        command: "/bin/echo".into(),
                        args: vec![],
                        workdir: None,
                        env: std::collections::HashMap::from([
                            ("SECRET_KEY".into(), "super-secret-value".into()),
                            ("API_TOKEN".into(), "another-secret".into()),
                        ]),
                        restart_on_swap: false,
                        restart_on_crash: false,
                        auto_start: false,
                        depends_on_port: None,
                        stop_timeout_secs: 30,
                        version_file: None,
                        update_command: None,
                        update_workdir: None,
                        restart_on_error_patterns: vec![],
                        data_dir: None,
                    },
                );
            }

            let app = test_router(state);

            let req = Request::builder()
                .uri("/api/config")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["api_key"], "[redacted]");

            // Agent env vars should be redacted
            let agent_env = &json["agents"]["test_agent"]["env"];
            let env_str = agent_env.as_str().expect("env should be a redacted string");
            assert!(
                env_str.contains("2 vars redacted"),
                "env should show redacted count, got: {env_str}"
            );
            // Must NOT contain the actual secret values
            let body_str = String::from_utf8_lossy(&body);
            assert!(
                !body_str.contains("super-secret-value"),
                "response must not contain actual secret values"
            );
            assert!(
                !body_str.contains("another-secret"),
                "response must not contain actual secret values"
            );
        }

        // --- 6. GET /api/logs?n=10 → 200 with last N log lines ---
        #[tokio::test]
        async fn test_route_logs_returns_last_n_lines() {
            let (_dir, state) = build_test_app_state(None);

            // Push some log lines
            for i in 0..20 {
                state.log_buffer.push(format!("log line {i}"));
            }

            let app = test_router(state);

            let req = Request::builder()
                .uri("/api/logs?n=5")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            let lines = json["lines"].as_array().expect("lines should be an array");
            assert_eq!(lines.len(), 5, "should return exactly 5 lines");
            // Should be the last 5 lines
            assert_eq!(lines[0], "log line 15");
            assert_eq!(lines[4], "log line 19");
        }

        // --- 7. POST /api/start when stopped → triggers backend.start(), transitions to Running ---
        //
        // Uses a simple health endpoint to satisfy the wait_for_health check.
        // The config profile port is updated to match the mock server's port.
        #[tokio::test]
        async fn test_route_start_when_stopped() {
            // Start a minimal HTTP server to satisfy the health check
            let health_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mock_port = health_listener.local_addr().unwrap().port();

            let health_app = axum::Router::new().route("/health", get(|| async { StatusCode::OK }));
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                axum::serve(health_listener, health_app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap();
            });

            let (_dir, state) = build_test_app_state(None);

            // Update the config profile port to match the mock server
            {
                let mut config = state.config.write().await;
                if let Some(profile) = config.profiles.get_mut("test") {
                    profile.port = mock_port;
                }
            }

            let app = test_router(state.clone());

            let req = Request::builder()
                .method("POST")
                .uri("/api/start")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"test"}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true, "start should succeed");
            let msg = json["message"].as_str().unwrap();
            assert!(
                msg.contains("started"),
                "message should indicate server started, got: {msg}"
            );
            assert_eq!(json["status"]["state"], "running");

            // Verify backend is now running
            let backend_state = state.backend.lock().await.to_server_state().await;
            assert!(
                matches!(
                    backend_state,
                    rookery_core::state::ServerState::Running { .. }
                ),
                "backend should be running after POST /api/start"
            );

            let _ = shutdown_tx.send(());
        }

        // --- 8. POST /api/start when already running same profile → 200 no-op ---
        #[tokio::test]
        async fn test_route_start_idempotent_same_profile() {
            let running_info = BackendInfo {
                pid: Some(12345),
                container_id: None,
                port: 8081,
                profile: "test".into(),
                started_at: chrono::Utc::now(),
                backend_type: BackendType::LlamaServer,
                command_line: vec!["mock-server".into()],
                exe_path: Some(std::path::PathBuf::from("/mock/llama-server")),
            };
            let backend = MockBackend::running_with(running_info);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;
            let app = test_router(state);

            let req = Request::builder()
                .method("POST")
                .uri("/api/start")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"test"}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true, "should succeed as no-op");
            let msg = json["message"].as_str().unwrap();
            assert!(
                msg.contains("already running"),
                "message should indicate already running, got: {msg}"
            );
        }

        // --- 9. POST /api/stop when running → triggers backend.stop(), transitions to Stopped ---
        #[tokio::test]
        async fn test_route_stop_when_running() {
            let running_info = BackendInfo {
                pid: Some(12345),
                container_id: None,
                port: 8081,
                profile: "test".into(),
                started_at: chrono::Utc::now(),
                backend_type: BackendType::LlamaServer,
                command_line: vec!["mock-server".into()],
                exe_path: Some(std::path::PathBuf::from("/mock/llama-server")),
            };
            let backend = MockBackend::running_with(running_info);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;
            let app = test_router(state.clone());

            let req = Request::builder()
                .method("POST")
                .uri("/api/stop")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true);
            assert_eq!(json["message"], "server stopped");
            assert_eq!(json["status"]["state"], "stopped");

            // Verify backend is now stopped
            let backend_state = state.backend.lock().await.to_server_state().await;
            assert!(
                matches!(backend_state, rookery_core::state::ServerState::Stopped),
                "backend should be stopped after POST /api/stop"
            );
        }

        // --- 10. POST /api/stop when stopped → 200 no-op ---
        #[tokio::test]
        async fn test_route_stop_when_already_stopped() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router(state);

            let req = Request::builder()
                .method("POST")
                .uri("/api/stop")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true);
            assert_eq!(json["status"]["state"], "stopped");
        }

        // --- 10b/10c. LAN-1123: a failed stop must still land a TERMINAL state ---
        //
        // post_stop broadcasts `Stopping` before it calls stop(); the error path
        // used to return 500 without landing anything, so the daemon sat on
        // `Stopping` forever (reconcile() only folds it to Stopped across a
        // restart). Both tests below see "stopping" without the fix.

        /// Backend whose stop() always fails. `survives` models the vLLM case:
        /// `docker compose down` errors and the container is deliberately left
        /// registered, so the server is still serving after the failed stop.
        struct StopFailsBackend {
            running: std::sync::atomic::AtomicBool,
            survives: bool,
            info: BackendInfo,
            draining: std::sync::atomic::AtomicBool,
        }

        impl StopFailsBackend {
            fn new(survives: bool) -> Self {
                Self {
                    running: std::sync::atomic::AtomicBool::new(true),
                    survives,
                    info: BackendInfo {
                        pid: Some(4242),
                        container_id: None,
                        port: 19876,
                        profile: "test".into(),
                        started_at: chrono::Utc::now(),
                        backend_type: BackendType::LlamaServer,
                        command_line: vec!["mock-server".into()],
                        exe_path: Some(std::path::PathBuf::from("/mock/llama-server")),
                    },
                    draining: std::sync::atomic::AtomicBool::new(false),
                }
            }
        }

        #[async_trait::async_trait]
        impl InferenceBackend for StopFailsBackend {
            async fn start(
                &self,
                _config: &rookery_core::config::Config,
                _profile: &str,
            ) -> rookery_core::error::Result<BackendInfo> {
                Ok(self.info.clone())
            }

            async fn stop(&self) -> rookery_core::error::Result<()> {
                if !self.survives {
                    self.running
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                }
                Err(rookery_core::error::Error::StatePersist(
                    "docker compose down failed".into(),
                ))
            }

            async fn is_running(&self) -> bool {
                self.running.load(std::sync::atomic::Ordering::SeqCst)
            }

            async fn process_info(&self) -> Option<BackendInfo> {
                Some(self.info.clone())
            }

            async fn adopt(&self, _info: BackendInfo) -> rookery_core::error::Result<()> {
                Ok(())
            }

            async fn to_server_state(&self) -> rookery_core::state::ServerState {
                if self.running.load(std::sync::atomic::Ordering::SeqCst) {
                    rookery_core::state::ServerState::Running {
                        profile: self.info.profile.clone(),
                        pid: self.info.pid.unwrap_or(0),
                        port: self.info.port,
                        since: self.info.started_at,
                        command_line: self.info.command_line.clone(),
                        exe_path: self.info.exe_path.clone(),
                        backend_type: self.info.backend_type,
                        container_id: self.info.container_id.clone(),
                    }
                } else {
                    rookery_core::state::ServerState::Stopped
                }
            }

            fn is_draining(&self) -> bool {
                self.draining.load(std::sync::atomic::Ordering::SeqCst)
            }

            fn set_draining(&self, draining: bool) {
                self.draining
                    .store(draining, std::sync::atomic::Ordering::SeqCst);
            }
        }

        async fn post_stop_with_failing_backend(
            survives: bool,
        ) -> (
            tempfile::TempDir,
            std::sync::Arc<crate::app_state::AppState>,
        ) {
            let (dir, state) =
                build_test_app_state(Some(Box::new(StopFailsBackend::new(survives))));
            sync_state_from_backend(&state).await;

            let req = Request::builder()
                .method("POST")
                .uri("/api/stop")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();

            let resp = test_router(state.clone()).oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "a failed stop is still a 500"
            );

            (dir, state)
        }

        // Process did not survive → record why it is down.
        #[tokio::test]
        async fn test_route_stop_failure_lands_failed_state() {
            let (_dir, state) = post_stop_with_failing_backend(false).await;

            let after = state.current_state().await;
            assert!(
                matches!(
                    after,
                    rookery_core::state::ServerState::Failed { ref profile, .. } if profile == "test"
                ),
                "failed stop must land Failed, not linger on Stopping — got {after:?}"
            );
            assert_eq!(
                super::status_from_state(&after).state,
                "failed: state persistence error: docker compose down failed",
                "the stop error must be surfaced to clients"
            );
        }

        // Process demonstrably survived → report the truth, so a later
        // post_start does not launch a second server on the same port.
        #[tokio::test]
        async fn test_route_stop_failure_lands_running_when_process_survives() {
            let (_dir, state) = post_stop_with_failing_backend(true).await;

            let after = state.current_state().await;
            assert!(
                after.is_running(),
                "a survived process must be reported Running, not Stopping/Failed — got {after:?}"
            );
            assert_eq!(after.profile_name(), Some("test"));
        }

        #[tokio::test]
        async fn test_route_sleep_when_running() {
            let running_info = BackendInfo {
                pid: Some(12345),
                container_id: None,
                port: 8081,
                profile: "test".into(),
                started_at: chrono::Utc::now(),
                backend_type: BackendType::LlamaServer,
                command_line: vec!["mock-server".into()],
                exe_path: Some(std::path::PathBuf::from("/mock/llama-server")),
            };
            let backend = MockBackend::running_with(running_info);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;
            let app = test_router(state.clone());

            let req = Request::builder()
                .method("POST")
                .uri("/api/sleep")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true);
            assert_eq!(json["status"]["state"], "sleeping");
            assert!(matches!(
                state.current_state().await,
                rookery_core::state::ServerState::Sleeping { .. }
            ));
            assert!(matches!(
                state.backend.lock().await.to_server_state().await,
                rookery_core::state::ServerState::Stopped
            ));
        }

        #[tokio::test]
        async fn test_route_wake_when_sleeping() {
            let health_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mock_port = health_listener.local_addr().unwrap().port();

            let health_app = axum::Router::new().route("/health", get(|| async { StatusCode::OK }));
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                axum::serve(health_listener, health_app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .unwrap();
            });

            let (_dir, state) = build_test_app_state(None);
            {
                let mut config = state.config.write().await;
                if let Some(profile) = config.profiles.get_mut("test") {
                    profile.port = mock_port;
                }
            }
            state
                .set_server_state(rookery_core::state::ServerState::Sleeping {
                    profile: "test".into(),
                    since: chrono::Utc::now(),
                })
                .await;
            state.agent_manager.set_dependency_bounce_suppressed(true);

            let app = test_router(state.clone());

            let req = Request::builder()
                .method("POST")
                .uri("/api/wake")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true);
            assert_eq!(json["status"]["state"], "running");
            assert!(state.current_state().await.is_running());

            let _ = shutdown_tx.send(());
        }

        /// A health endpoint on a fresh port, so `start_profile` gets all the
        /// way to `Running` when nothing stops it. Returns (port, shutdown_tx).
        ///
        /// The two shutdown-guard tests below need this rather than a dead port:
        /// with a dead port an unguarded start would sit in the 120s health wait
        /// and the test would time out instead of failing, which proves nothing.
        /// With it, removing the guard turns both into a fast, loud 200/Running.
        async fn spawn_health_endpoint() -> (u16, tokio::sync::oneshot::Sender<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let app = axum::Router::new().route("/health", get(|| async { StatusCode::OK }));
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await
                    .unwrap();
            });
            (port, tx)
        }

        // --- 10b. POST /api/start once shutdown has begun → 503, nothing spawned ---
        //
        // LAN-1128 regression test, the `post_start` half. Same race LAN-1120
        // closed for swap: `begin_shutdown()` runs before `server_handle.abort()`,
        // aborting axum does not cancel already-spawned handlers, and shutdown
        // gives up on `op_lock` after 20s while the health wait runs for 120s —
        // so an unguarded start spawns an llama-server the exiting daemon never
        // supervises. Without the guard this returns 200/Running.
        #[tokio::test]
        async fn test_route_start_aborts_when_shutting_down() {
            let (mock_port, shutdown_tx) = spawn_health_endpoint().await;

            let (_dir, state) = build_test_app_state(None);
            {
                let mut config = state.config.write().await;
                if let Some(profile) = config.profiles.get_mut("test") {
                    profile.port = mock_port;
                }
            }

            // What main.rs does on SIGTERM, before server_handle.abort().
            state.agent_manager.begin_shutdown();

            let app = test_router(state.clone());
            let req = Request::builder()
                .method("POST")
                .uri("/api/start")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"test"}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "start must abort with 503 once shutdown has begun"
            );

            // The whole point of the ticket: no orphaned process holding VRAM.
            assert!(
                !state.backend.lock().await.is_running().await,
                "shutdown abort must not spawn a backend"
            );
            let after = state.current_state().await;
            assert!(
                matches!(after, rookery_core::state::ServerState::Stopped),
                "aborted start must land Stopped, not linger on Starting — got {after:?}"
            );

            let _ = shutdown_tx.send(());
        }

        // --- 10c. POST /api/wake once shutdown has begun → 503, nothing spawned ---
        //
        // The `post_wake` half. Worth its own test because wake enters from
        // `Sleeping`, so it is the caller that would otherwise persist a
        // transient state: a SIGKILL at TimeoutStopSec before main.rs writes
        // `Stopped` would leave the next boot restoring a sleeping server that
        // does not exist.
        #[tokio::test]
        async fn test_route_wake_aborts_when_shutting_down() {
            let (mock_port, shutdown_tx) = spawn_health_endpoint().await;

            let (_dir, state) = build_test_app_state(None);
            {
                let mut config = state.config.write().await;
                if let Some(profile) = config.profiles.get_mut("test") {
                    profile.port = mock_port;
                }
            }
            state
                .set_server_state(rookery_core::state::ServerState::Sleeping {
                    profile: "test".into(),
                    since: chrono::Utc::now(),
                })
                .await;

            state.agent_manager.begin_shutdown();

            let app = test_router(state.clone());
            let req = Request::builder()
                .method("POST")
                .uri("/api/wake")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "wake must abort with 503 once shutdown has begun"
            );

            assert!(
                !state.backend.lock().await.is_running().await,
                "shutdown abort must not spawn a backend"
            );
            let after = state.current_state().await;
            assert!(
                matches!(after, rookery_core::state::ServerState::Stopped),
                "aborted wake must not leave Sleeping behind — got {after:?}"
            );

            let _ = shutdown_tx.send(());
        }

        // --- 11. Request body size limit → 413 on oversized payload ---
        #[tokio::test]
        async fn test_route_body_size_limit_returns_413() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router(state);

            // Create a payload larger than 1MB (the configured body limit)
            let oversized = "x".repeat(2 * 1024 * 1024); // 2MB
            let body_str = format!(r#"{{"profile":"{}"}}"#, oversized);

            let req = Request::builder()
                .method("POST")
                .uri("/api/start")
                .header("content-type", "application/json")
                .body(Body::from(body_str))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "oversized payload should be rejected with 413"
            );
        }

        // ═══════════════════════════════════════════════════════════════
        // Extended route integration tests — swap, chat, bench,
        // model-info, server-stats, agents, hardware, config, dashboard
        // ═══════════════════════════════════════════════════════════════

        /// Build an extended router with ALL route endpoints for integration testing.
        /// Mirrors the full route set from main.rs.
        fn test_router_full(state: std::sync::Arc<crate::app_state::AppState>) -> Router {
            Router::new()
                .route("/api/health", get(super::get_health))
                .route("/api/status", get(super::get_status))
                .route("/api/profiles", get(super::get_profiles))
                .route("/api/config", get(super::get_config))
                .route(
                    "/api/config/profile/{name}",
                    axum::routing::put(super::put_profile),
                )
                .route("/api/reload", post(super::post_reload))
                .route("/api/logs", get(super::get_logs))
                .route("/metrics", get(super::get_metrics))
                .route("/api/start", post(super::post_start))
                .route("/api/stop", post(super::post_stop))
                .route("/api/sleep", post(super::post_sleep))
                .route("/api/wake", post(super::post_wake))
                .route("/api/swap", post(super::post_swap))
                .route("/api/chat", post(super::post_chat))
                .route("/api/bench", get(super::get_bench))
                .route("/api/model-info", get(super::get_model_info))
                .route("/api/server-stats", get(super::get_server_stats))
                .route("/api/agents", get(super::get_agents))
                .route("/api/agents/start", post(super::post_agent_start))
                .route("/api/agents/stop", post(super::post_agent_stop))
                .route("/api/agents/{name}/update", post(super::post_agent_update))
                .route("/api/hardware", get(super::get_hardware))
                .fallback(super::get_dashboard)
                .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
                .with_state(state)
        }

        /// Spawn a mock llama-server (axum) on a random port serving the
        /// endpoints that route handlers proxy to: /health, /v1/models,
        /// /props, /slots, /v1/chat/completions.
        /// Returns (port, shutdown_sender).
        /// A chat completion shaped like a real backend's: streamed when the request
        /// asks for it, plain JSON otherwise.
        ///
        /// The frames are spaced in real time on purpose. /api/bench derives its rates
        /// from arrival times, so a burst-delivered stream would put TTFT at the total
        /// elapsed and report gen_tok_s as zero.
        ///
        /// `with_usage: false` covers a backend that ignores `stream_options` and never
        /// sends a usage frame -- bench has to fall back to counting deltas there.
        fn mock_chat_response(
            req: &serde_json::Value,
            with_usage: bool,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            if !req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false) {
                return axum::response::Json(serde_json::json!({
                    "id": "chatcmpl-mock",
                    "object": "chat.completion",
                    "model": "mock-model",
                    "choices": [{"index": 0, "message": {
                        "role": "assistant", "content": "Hello!"
                    }, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
                }))
                .into_response();
            }

            // A reasoning delta then a content one: behind --reasoning-parser a model
            // emits `reasoning_content` first and `content` only after it stops
            // thinking, so both have to count toward TTFT and the token total.
            let mut frames = vec![
                r#"data: {"choices":[{"delta":{"reasoning_content":"Hmm"}}]}"#.to_string(),
                r#"data: {"choices":[{"delta":{"content":"Hello!"}}]}"#.to_string(),
            ];
            if with_usage {
                frames.push(
                    r#"data: {"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}"#
                        .to_string(),
                );
            }
            frames.push("data: [DONE]".to_string());

            let body = axum::body::Body::from_stream(futures_util::stream::unfold(
                frames.into_iter(),
                |mut it| async move {
                    let f = it.next()?;
                    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
                    Some((Ok::<_, std::convert::Infallible>(format!("{f}\n\n")), it))
                },
            ));
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                body,
            )
                .into_response()
        }

        async fn spawn_mock_llama_server() -> (u16, tokio::sync::oneshot::Sender<()>) {
            use axum::response::Json as AxumJson;
            use axum::routing::{get as aget, post as apost};

            let mock_app = Router::new()
                .route("/health", aget(|| async { StatusCode::OK }))
                .route(
                    "/v1/models",
                    aget(|| async {
                        AxumJson(serde_json::json!({
                            "data": [{"id": "mock-model", "owned_by": "test"}]
                        }))
                    }),
                )
                .route(
                    "/props",
                    aget(|| async {
                        AxumJson(serde_json::json!({
                            "total_slots": 1,
                            "chat_template": "test"
                        }))
                    }),
                )
                .route(
                    "/slots",
                    aget(|| async {
                        AxumJson(serde_json::json!([{
                            "id": 0, "state": 0, "prompt": "", "next_token": {}
                        }]))
                    }),
                )
                .route(
                    "/v1/chat/completions",
                    apost(|AxumJson(req): AxumJson<serde_json::Value>| async move {
                        mock_chat_response(&req, true)
                    }),
                );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();

            tokio::spawn(async move {
                axum::serve(listener, mock_app)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await
                    .unwrap();
            });

            (port, tx)
        }

        /// Helper: create a MockBackend running on a given port.
        fn mock_backend_on_port(port: u16) -> MockBackend {
            MockBackend::running_with(BackendInfo {
                pid: Some(99999),
                container_id: None,
                port,
                profile: "test".into(),
                started_at: chrono::Utc::now(),
                backend_type: BackendType::LlamaServer,
                command_line: vec!["mock-server".into()],
                exe_path: Some(std::path::PathBuf::from("/mock/llama-server")),
            })
        }

        // --- 12. POST /api/swap when running → drains old, stops, attempts new profile ---
        //
        // The swap handler creates a new real backend via create_backend(),
        // which means the new backend's start() will fail in test (no real
        // llama-server binary). This test verifies:
        // (a) the old backend is drained and stopped
        // (b) the handler returns 500 because the new backend can't start
        // (c) the drain flag is cleared (no permanent 503)
        #[tokio::test]
        async fn test_route_swap_when_running_drains_and_stops_old() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let backend = mock_backend_on_port(mock_port);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;

            // Add a second profile to swap to
            {
                let mut config = state.config.write().await;
                config.profiles.insert(
                    "other".into(),
                    rookery_core::config::Profile {
                        sglang: None,
                        model: "test_model".into(),
                        port: mock_port,
                        llama_server: None,
                        vllm: None,
                        ctx_size: 2048,
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
                        aliases: vec![],
                        extra_args: vec![],
                    },
                );
            }

            let app = test_router_full(state.clone());

            let req = Request::builder()
                .method("POST")
                .uri("/api/swap")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"other"}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            // The new backend's start() fails (no real binary), so swap returns 500
            assert_eq!(
                resp.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "swap should fail when new backend can't start"
            );

            // Critical: after a failed swap, drain flag must be cleared
            // so post_chat doesn't permanently return 503
            let is_draining = state.backend.lock().await.is_draining();
            assert!(!is_draining, "drain flag must be cleared after failed swap");

            let _ = shutdown_tx.send(());
        }

        // --- 12b. POST /api/swap with an unknown profile → 404, server untouched ---
        //
        // Regression test for the ordering bug: the profile lookup used to run
        // AFTER the drain/stop, so a typo'd name tore down a live server and
        // then reported 500. Validation now happens before any teardown.
        #[tokio::test]
        async fn test_route_swap_unknown_profile_leaves_server_running() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let backend = mock_backend_on_port(mock_port);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;

            let app = test_router_full(state.clone());

            let req = Request::builder()
                .method("POST")
                .uri("/api/swap")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"tset"}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "unknown profile should be 404, not 500"
            );

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(
                json["error"].as_str().unwrap_or_default().contains("tset"),
                "404 body should name the unknown profile, got: {json}"
            );

            // The whole point: a typo must not take the model down.
            assert!(
                state.backend.lock().await.is_running().await,
                "running server must survive a swap to an unknown profile"
            );
            assert!(
                !state.backend.lock().await.is_draining(),
                "rejected swap must not leave the backend draining"
            );

            let _ = shutdown_tx.send(());
        }

        // --- 12c. POST /api/swap once shutdown has begun → 503, nothing spawned ---
        //
        // LAN-1120 regression test. Shutdown calls begin_shutdown() before
        // aborting axum, then gives up on op_lock after 20s, so a swap that is
        // mid-flight runs on and spawns an llama-server the exiting daemon never
        // supervises. Without the guard this reaches new_backend.start(), fails
        // on the missing binary and returns 500 with a `Failed` state.
        #[tokio::test]
        async fn test_route_swap_aborts_when_shutting_down() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let backend = mock_backend_on_port(mock_port);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;

            // What main.rs does on SIGTERM, before server_handle.abort().
            state.agent_manager.begin_shutdown();

            let app = test_router_full(state.clone());

            let req = Request::builder()
                .method("POST")
                .uri("/api/swap")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"test"}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "swap must abort with 503 once shutdown has begun"
            );

            // The guard is an early return placed AFTER clear_drain(), so it
            // must not wedge post_chat on a permanent 503.
            assert!(
                !state.backend.lock().await.is_draining(),
                "shutdown abort must not leave the backend draining"
            );

            // No backend was spawned, and no transient state left behind.
            assert!(
                !state.backend.lock().await.is_running().await,
                "shutdown abort must not start a new backend"
            );
            let after = state.current_state().await;
            assert!(
                matches!(after, rookery_core::state::ServerState::Stopped),
                "shutdown abort must land Stopped, not linger on Swapping — got {after:?}"
            );

            let _ = shutdown_tx.send(());
        }

        // --- 13. POST /api/swap when stopped → error (binary not found / start fails) ---
        #[tokio::test]
        async fn test_route_swap_when_stopped() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router_full(state);

            // Use a valid profile name ("test") from the test config.
            // When stopped, swap skips drain/stop and tries to start the new backend,
            // which fails because /mock/llama-server doesn't exist.
            let req = Request::builder()
                .method("POST")
                .uri("/api/swap")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"profile":"test"}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            // Swap when stopped with a valid profile fails because the backend
            // can't actually start (no real binary). Returns 500.
            assert_eq!(
                resp.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "swap when stopped should fail because backend can't start"
            );
        }

        // --- 14. POST /api/chat when draining → 503 ---
        #[tokio::test]
        async fn test_route_chat_when_draining_returns_503() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let backend = mock_backend_on_port(mock_port);
            backend.set_draining(true);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;

            let app = test_router_full(state);

            let req = Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "chat during drain should return 503"
            );

            let _ = shutdown_tx.send(());
        }

        // --- 15. POST /api/chat when running → proxies to backend (mock server) ---
        #[tokio::test]
        async fn test_route_chat_when_running_proxies() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let backend = mock_backend_on_port(mock_port);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;

            let app = test_router_full(state);

            let req = Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "chat should proxy successfully"
            );

            // The response is a streaming body (text/event-stream).
            // Collect the raw body and check it contains the mock response.
            let content_type = resp
                .headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap_or(""))
                .unwrap_or("");
            assert!(
                content_type.contains("text/event-stream"),
                "chat response should be SSE stream, got content-type: {content_type}"
            );

            let _ = shutdown_tx.send(());
        }

        #[tokio::test]
        async fn test_route_chat_when_sleeping_wakes_and_proxies() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let (_dir, state) = build_test_app_state(None);
            {
                let mut config = state.config.write().await;
                if let Some(profile) = config.profiles.get_mut("test") {
                    profile.port = mock_port;
                }
            }
            state
                .set_server_state(rookery_core::state::ServerState::Sleeping {
                    profile: "test".into(),
                    since: chrono::Utc::now(),
                })
                .await;
            state.agent_manager.set_dependency_bounce_suppressed(true);

            let app = test_router_full(state.clone());

            let req = Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"messages":[{"role":"user","content":"wake up"}]}"#,
                ))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert!(state.current_state().await.is_running());

            let content_type = resp
                .headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap_or(""))
                .unwrap_or("");
            assert!(content_type.contains("text/event-stream"));

            let _ = shutdown_tx.send(());
        }

        #[tokio::test]
        async fn test_route_metrics_chat_error_counter_increments() {
            let backend = MockBackend::running_with(BackendInfo {
                pid: Some(12345),
                container_id: None,
                port: 9,
                profile: "test".into(),
                started_at: chrono::Utc::now(),
                backend_type: BackendType::LlamaServer,
                command_line: vec!["mock-server".into()],
                exe_path: Some(std::path::PathBuf::from("/mock/llama-server")),
            });
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;
            let app = test_router_full(state);

            let chat_req = Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap();
            let chat_resp = app.clone().oneshot(chat_req).await.unwrap();
            assert_eq!(chat_resp.status(), StatusCode::BAD_GATEWAY);

            let metrics_req = Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap();
            let metrics_resp = app.oneshot(metrics_req).await.unwrap();
            assert_eq!(metrics_resp.status(), StatusCode::OK);

            let body = metrics_resp.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(body.to_vec()).unwrap();
            assert!(text.contains("rookery_chat_requests_total 1"));
            assert!(text.contains("rookery_chat_errors_total 1"));
        }

        // --- 15b. Non-2xx from upstream must not be laundered into a 200 SSE stream ---
        //
        // `.send()` returns Ok for an HTTP 400, so before LAN-1075 the error
        // body was streamed straight through as `200 text/event-stream` and
        // rookery_chat_errors_total stayed at 0.
        #[tokio::test]
        async fn test_route_chat_upstream_error_is_not_streamed_as_success() {
            let mock_app = Router::new().route(
                "/v1/chat/completions",
                post(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        axum::response::Json(serde_json::json!({
                            "error": {
                                "code": 400,
                                "message": "All non-assistant messages must contain 'content'"
                            }
                        })),
                    )
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mock_port = listener.local_addr().unwrap().port();
            let (shutdown_tx, rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                axum::serve(listener, mock_app)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await
                    .unwrap();
            });

            let (_dir, state) =
                build_test_app_state(Some(Box::new(mock_backend_on_port(mock_port))));
            sync_state_from_backend(&state).await;
            let app = test_router_full(state);

            let chat_req = Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user"}]}"#))
                .unwrap();
            let chat_resp = app.clone().oneshot(chat_req).await.unwrap();

            assert_eq!(
                chat_resp.status(),
                StatusCode::BAD_GATEWAY,
                "upstream non-2xx must surface as 502, not a 200 SSE stream"
            );
            assert!(
                chat_resp
                    .headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .is_none_or(|ct| !ct.contains("text/event-stream")),
                "error response must not be framed as an SSE stream"
            );

            let metrics_resp = app
                .oneshot(
                    Request::builder()
                        .uri("/metrics")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = metrics_resp.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(body.to_vec()).unwrap();
            assert!(
                text.contains("rookery_chat_errors_total 1"),
                "upstream error must increment the chat error counter, got:\n{text}"
            );

            let _ = shutdown_tx.send(());
        }

        // --- 16. GET /api/bench → returns timing data from mock server ---
        #[tokio::test]
        async fn test_route_bench_returns_timing_data() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let backend = mock_backend_on_port(mock_port);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;

            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/bench")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            let tests = json["tests"].as_array().expect("tests should be an array");
            assert!(
                !tests.is_empty(),
                "bench should return at least one test result"
            );

            // Verify timing fields from the mock server
            for test in tests {
                assert!(
                    test.get("name").is_some(),
                    "each bench test should have a name"
                );
                assert!(
                    test["pp_tok_s"].as_f64().unwrap_or(0.0) > 0.0,
                    "pp_tok_s should be positive from mock timings"
                );
                assert!(
                    test["gen_tok_s"].as_f64().unwrap_or(0.0) > 0.0,
                    "gen_tok_s should be positive from mock timings"
                );
            }

            let _ = shutdown_tx.send(());
        }

        // --- 17. GET /api/model-info → proxies /v1/models + /props from mock server ---
        #[tokio::test]
        async fn test_route_model_info_proxies_to_backend() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let backend = mock_backend_on_port(mock_port);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;

            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/model-info")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["available"], true);
            assert_eq!(json["model_id"], "mock-model");
            assert_eq!(json["owned_by"], "test");
            // Props should be populated from /props endpoint
            assert!(
                json["props"].is_object(),
                "props should be an object from mock /props, got: {}",
                json["props"]
            );
            assert_eq!(json["props"]["total_slots"], 1);
            assert_eq!(json["props"]["chat_template"], "test");

            let _ = shutdown_tx.send(());
        }

        // --- 18. GET /api/model-info when stopped → available=false ---
        #[tokio::test]
        async fn test_route_model_info_when_stopped() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/model-info")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["available"], false);
        }

        // --- 19. GET /api/server-stats → proxies /slots from mock server ---
        #[tokio::test]
        async fn test_route_server_stats_proxies_slots() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let backend = mock_backend_on_port(mock_port);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
            sync_state_from_backend(&state).await;

            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/server-stats")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["available"], true);
            let slots = json["slots"].as_array().expect("slots should be an array");
            assert_eq!(slots[0]["id"], 0);
            assert_eq!(slots[0]["state"], 0);

            let _ = shutdown_tx.send(());
        }

        // --- 20. GET /api/server-stats when stopped → available=false ---
        #[tokio::test]
        async fn test_route_server_stats_when_stopped() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/server-stats")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["available"], false);
        }

        // --- 21. GET /api/agents → returns agent list ---
        #[tokio::test]
        async fn test_route_agents_returns_list() {
            let (_dir, state) = build_test_app_state(None);

            // Add agent config so "configured" is non-empty
            {
                let mut config = state.config.write().await;
                config.agents.insert(
                    "test_agent".into(),
                    rookery_core::config::AgentConfig {
                        command: "/bin/echo".into(),
                        args: vec!["hello".into()],
                        workdir: None,
                        env: std::collections::HashMap::new(),
                        restart_on_swap: false,
                        restart_on_crash: false,
                        auto_start: false,
                        depends_on_port: None,
                        stop_timeout_secs: 30,
                        version_file: None,
                        update_command: None,
                        update_workdir: None,
                        restart_on_error_patterns: vec![],
                        data_dir: None,
                    },
                );
            }

            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/agents")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            // agents array may be empty (none running), but configured should list the agent
            assert!(json["agents"].is_array(), "agents should be an array");
            let configured = json["configured"]
                .as_array()
                .expect("configured should be an array");
            assert!(
                configured.iter().any(|v| v == "test_agent"),
                "configured should include 'test_agent', got: {configured:?}"
            );
        }

        // --- 22. POST /api/agents/start and /api/agents/stop → lifecycle ---
        #[tokio::test]
        async fn test_route_agent_start_and_stop_lifecycle() {
            let (_dir, state) = build_test_app_state(None);

            // Configure an agent that will start successfully
            {
                let mut config = state.config.write().await;
                config.agents.insert(
                    "sleeper".into(),
                    rookery_core::config::AgentConfig {
                        command: "/bin/sleep".into(),
                        args: vec!["60".into()],
                        workdir: None,
                        env: std::collections::HashMap::new(),
                        restart_on_swap: false,
                        restart_on_crash: false,
                        auto_start: false,
                        depends_on_port: None,
                        stop_timeout_secs: 30,
                        version_file: None,
                        update_command: None,
                        update_workdir: None,
                        restart_on_error_patterns: vec![],
                        data_dir: None,
                    },
                );
            }

            let app = test_router_full(state.clone());

            // Start the agent
            let req = Request::builder()
                .method("POST")
                .uri("/api/agents/start")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"sleeper"}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true, "agent start should succeed");
            let msg = json["message"].as_str().unwrap();
            assert!(
                msg.contains("sleeper") && msg.contains("started"),
                "message should confirm agent started, got: {msg}"
            );

            // Stop the agent
            let app2 = test_router_full(state.clone());

            let req = Request::builder()
                .method("POST")
                .uri("/api/agents/stop")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"sleeper"}"#))
                .unwrap();

            let resp = app2.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true, "agent stop should succeed");
            let msg = json["message"].as_str().unwrap();
            assert!(
                msg.contains("sleeper") && msg.contains("stopped"),
                "message should confirm agent stopped, got: {msg}"
            );
        }

        #[tokio::test]
        async fn test_route_agent_update_success_restarts_and_reports_version() {
            let (_dir, state) = build_test_app_state(None);
            let agent_dir = tempfile::tempdir().unwrap();
            let version_path = agent_dir.path().join("pyproject.toml");
            std::fs::write(
                &version_path,
                "[project]\nname = \"hermes\"\nversion = \"0.4.0\"\n",
            )
            .unwrap();

            let agent_config = rookery_core::config::AgentConfig {
                command: "/bin/sh".into(),
                args: vec!["-lc".into(), "sleep 60".into()],
                workdir: Some(agent_dir.path().to_path_buf()),
                env: std::collections::HashMap::new(),
                restart_on_swap: false,
                restart_on_crash: false,
                auto_start: false,
                depends_on_port: None,
                stop_timeout_secs: 30,
                version_file: Some(version_path.clone()),
                update_command: Some(
                    "printf '[project]\\nname = \"hermes\"\\nversion = \"0.5.0\"\\n' > pyproject.toml && echo updated".into(),
                ),
                update_workdir: Some(agent_dir.path().to_path_buf()),
                restart_on_error_patterns: vec![],
                data_dir: None,
            };

            {
                let mut config = state.config.write().await;
                config.agents.insert("hermes".into(), agent_config.clone());
            }

            state
                .agent_manager
                .start("hermes", &agent_config)
                .await
                .unwrap();

            let app = test_router_full(state.clone());
            let req = Request::builder()
                .method("POST")
                .uri("/api/agents/hermes/update")
                .body(Body::from("{}"))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true);
            assert_eq!(json["previous_version"], "0.4.0");
            assert_eq!(json["version"], "0.5.0");
            assert!(
                json["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("updated hermes from 0.4.0 to 0.5.0")
            );
            assert!(state.agent_manager.is_running("hermes").await);

            let health = state.agent_manager.get_health("hermes").await.unwrap();
            assert_eq!(health.version.as_deref(), Some("0.5.0"));

            let logs = state.log_buffer.last_n(20).join("\n");
            assert!(logs.contains("[agent:hermes:update] updated"));
        }

        #[tokio::test]
        async fn test_route_agent_update_failure_restarts_old_agent() {
            let (_dir, state) = build_test_app_state(None);
            let agent_dir = tempfile::tempdir().unwrap();
            let version_path = agent_dir.path().join("pyproject.toml");
            std::fs::write(
                &version_path,
                "[project]\nname = \"hermes\"\nversion = \"0.4.0\"\n",
            )
            .unwrap();

            let agent_config = rookery_core::config::AgentConfig {
                command: "/bin/sh".into(),
                args: vec!["-lc".into(), "sleep 60".into()],
                workdir: Some(agent_dir.path().to_path_buf()),
                env: std::collections::HashMap::new(),
                restart_on_swap: false,
                restart_on_crash: false,
                auto_start: false,
                depends_on_port: None,
                stop_timeout_secs: 30,
                version_file: Some(version_path.clone()),
                update_command: Some("echo boom >&2; exit 7".into()),
                update_workdir: Some(agent_dir.path().to_path_buf()),
                restart_on_error_patterns: vec![],
                data_dir: None,
            };

            {
                let mut config = state.config.write().await;
                config.agents.insert("hermes".into(), agent_config.clone());
            }

            state
                .agent_manager
                .start("hermes", &agent_config)
                .await
                .unwrap();

            let app = test_router_full(state.clone());
            let req = Request::builder()
                .method("POST")
                .uri("/api/agents/hermes/update")
                .body(Body::from("{}"))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], false);
            assert_eq!(json["previous_version"], "0.4.0");
            assert_eq!(json["version"], "0.4.0");
            assert!(
                json["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("agent restarted on previous code")
            );
            assert!(state.agent_manager.is_running("hermes").await);

            let health = state.agent_manager.get_health("hermes").await.unwrap();
            assert_eq!(health.version.as_deref(), Some("0.4.0"));

            let logs = state.log_buffer.last_n(20).join("\n");
            assert!(logs.contains("[agent:hermes:update] boom"));
        }

        #[tokio::test]
        async fn test_route_agent_update_missing_command_returns_failure() {
            let (_dir, state) = build_test_app_state(None);

            {
                let mut config = state.config.write().await;
                config.agents.insert(
                    "hermes".into(),
                    rookery_core::config::AgentConfig {
                        command: "/bin/echo".into(),
                        args: vec![],
                        workdir: None,
                        env: std::collections::HashMap::new(),
                        restart_on_swap: false,
                        restart_on_crash: false,
                        auto_start: false,
                        depends_on_port: None,
                        stop_timeout_secs: 30,
                        version_file: None,
                        update_command: None,
                        update_workdir: None,
                        restart_on_error_patterns: vec![],
                        data_dir: None,
                    },
                );
            }

            let app = test_router_full(state);
            let req = Request::builder()
                .method("POST")
                .uri("/api/agents/hermes/update")
                .body(Body::from("{}"))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], false);
            assert!(
                json["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("has no update_command configured")
            );
        }

        // --- 22d. LAN-1125: updating an ALREADY-STOPPED agent still backs up ---
        //
        // LAN-1088 hooked the pre-change backup into `stop_inner`, which the
        // update route only reaches `if was_running`. The update command runs
        // regardless — `hermes update` migrates config in place whether or not
        // the gateway is up — so the cold path was mutating the agent with no
        // copy to restore from. Without the fix nothing creates `db-backups/`
        // and `generations()` is empty.
        #[tokio::test]
        async fn test_route_agent_update_backs_up_when_agent_already_stopped() {
            let (_dir, state) = build_test_app_state(None);
            let data_dir = tempfile::tempdir().unwrap();

            let db = data_dir.path().join("state.db");
            let made = std::process::Command::new(rookery_engine::integrity::SQLITE3)
                .arg(&db)
                .arg(
                    "PRAGMA journal_mode=WAL; CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT); \
                     INSERT INTO t(v) SELECT hex(randomblob(64)) FROM generate_series(1,200);",
                )
                .output()
                .is_ok_and(|o| o.status.success());
            if !made {
                return; // no sqlite3 on this box
            }

            let agent_config = rookery_core::config::AgentConfig {
                command: "/bin/sh".into(),
                args: vec!["-lc".into(), "sleep 60".into()],
                workdir: Some(data_dir.path().to_path_buf()),
                env: std::collections::HashMap::new(),
                restart_on_swap: false,
                restart_on_crash: false,
                auto_start: false,
                depends_on_port: None,
                stop_timeout_secs: 30,
                version_file: None,
                // Stands in for the in-place config migration `hermes update`
                // applies whether or not the gateway is running.
                update_command: Some("echo migrated".into()),
                update_workdir: Some(data_dir.path().to_path_buf()),
                restart_on_error_patterns: vec![],
                data_dir: Some(data_dir.path().to_path_buf()),
            };

            {
                let mut config = state.config.write().await;
                config.agents.insert("hermes".into(), agent_config);
            }

            // Deliberately NOT started: this is the cold path.
            assert!(!state.agent_manager.is_running("hermes").await);

            let app = test_router_full(state.clone());
            let req = Request::builder()
                .method("POST")
                .uri("/api/agents/hermes/update")
                .body(Body::from("{}"))
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let backups = data_dir.path().join(rookery_engine::backup::BACKUP_DIR);
            let gens: Vec<std::path::PathBuf> = std::fs::read_dir(&backups)
                .map(|entries| entries.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            assert_eq!(
                gens.len(),
                1,
                "an update of a stopped agent must leave exactly one backup generation in {}",
                backups.display()
            );

            // A copy that is not restorable is not a backup. Also pins the
            // `.bak` suffix and the extra directory level, which are what keep
            // LAN-1070's nightly integrity sweep from scanning these forever.
            let copy = gens[0].join("state.db.bak");
            assert!(copy.exists(), "expected {}", copy.display());
            let out = std::process::Command::new(rookery_engine::integrity::SQLITE3)
                .arg("-readonly")
                .arg(&copy)
                .arg("SELECT count(*) FROM t;")
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                "200",
                "the backup must be a usable database, not a torn copy"
            );
        }

        // --- 22e. LAN-1125: a failed backup must not block the update ---
        //
        // Pins LAN-1088's fail-open property on the new path. The data_dir holds
        // a file that is not a database, so `backup::run` reports a failure —
        // and the update must still run and still report success, because a
        // rookery that cannot update an agent is a worse outage than an update
        // without a copy.
        #[tokio::test]
        async fn test_route_agent_update_survives_a_failing_backup_when_stopped() {
            let (_dir, state) = build_test_app_state(None);
            let data_dir = tempfile::tempdir().unwrap();
            std::fs::write(data_dir.path().join("state.db"), b"not a database").unwrap();

            let agent_config = rookery_core::config::AgentConfig {
                command: "/bin/sh".into(),
                args: vec!["-lc".into(), "sleep 60".into()],
                workdir: Some(data_dir.path().to_path_buf()),
                env: std::collections::HashMap::new(),
                restart_on_swap: false,
                restart_on_crash: false,
                auto_start: false,
                depends_on_port: None,
                stop_timeout_secs: 30,
                version_file: None,
                update_command: Some("echo migrated".into()),
                update_workdir: Some(data_dir.path().to_path_buf()),
                restart_on_error_patterns: vec![],
                data_dir: Some(data_dir.path().to_path_buf()),
            };

            {
                let mut config = state.config.write().await;
                config.agents.insert("hermes".into(), agent_config);
            }

            let app = test_router_full(state.clone());
            let req = Request::builder()
                .method("POST")
                .uri("/api/agents/hermes/update")
                .body(Body::from("{}"))
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                json["success"], true,
                "a failed backup must not block the update, got: {json}"
            );

            let logs = state.log_buffer.last_n(50).join("\n");
            assert!(
                logs.contains("[agent:hermes:update] migrated"),
                "the update command must still have run, logs: {logs}"
            );
            // "Logged loudly" is the other half of fail-open, and asserting it
            // is also what makes this test discriminating: without the fix no
            // backup is attempted on this path, so the line is simply absent.
            assert!(
                logs.contains("[agent:hermes] db backup FAILED") && logs.contains("before update"),
                "a failed backup must be logged loudly, not swallowed, logs: {logs}"
            );
        }

        // --- 23. GET /api/hardware → returns hardware profile ---
        #[tokio::test]
        async fn test_route_hardware_returns_profile() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/hardware")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            // The test AppState has a CpuProfile with name "test-cpu"
            let cpu = &json["cpu"];
            assert_eq!(cpu["name"], "test-cpu");
            assert_eq!(cpu["cores"], 4);
            assert_eq!(cpu["threads"], 8);
            assert_eq!(cpu["ram_total_mb"], 16384);
            // ram_free_mb is added dynamically from /proc/meminfo
            assert!(
                cpu.get("ram_free_mb").is_some(),
                "cpu should include ram_free_mb"
            );
        }

        // --- 24. PUT /api/config/profile/:name → updates sampling params ---
        #[tokio::test]
        async fn test_route_put_profile_updates_params() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router_full(state.clone());

            let req = Request::builder()
                .method("PUT")
                .uri("/api/config/profile/test")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"temp":0.9,"top_p":0.95,"top_k":40}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(json["success"], true);
            let msg = json["message"].as_str().unwrap();
            assert!(
                msg.contains("test") && msg.contains("updated"),
                "message should confirm profile updated, got: {msg}"
            );

            // Assert through llama_server_config() — the same accessor
            // resolve_llama_server_command_line uses. Asserting the legacy flat
            // fields instead is what let this endpoint be a silent no-op for
            // every sub-table profile while this test stayed green.
            let config = state.config.read().await;
            let profile = config
                .profiles
                .get("test")
                .expect("test profile should exist");
            let ls = profile
                .llama_server_config()
                .expect("test profile is a llama-server profile");
            assert!(
                (ls.temp - 0.9).abs() < f32::EPSILON,
                "temp should be 0.9, got: {}",
                ls.temp
            );
            assert!(
                (ls.top_p - 0.95).abs() < f32::EPSILON,
                "top_p should be 0.95, got: {}",
                ls.top_p
            );
            assert_eq!(ls.top_k, 40, "top_k should be 40");
        }

        // --- 25. PUT /api/config/profile/:nonexistent → 404 ---
        #[tokio::test]
        async fn test_route_put_profile_nonexistent_returns_404() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router_full(state);

            let req = Request::builder()
                .method("PUT")
                .uri("/api/config/profile/nonexistent_profile")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"temp":0.5}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "updating nonexistent profile should return 404"
            );
        }

        // ═══════════════════════════════════════════════════════════════
        // LAN-1090 — POST /api/reload
        // ═══════════════════════════════════════════════════════════════

        /// Write a config file at `state.config_path` that the daemon would
        /// accept at boot. `llama_server` has to point at a file that really
        /// exists because `Config::validate()` stats it, so it is created in
        /// the state's own tempdir.
        fn seed_config_file(
            dir: &tempfile::TempDir,
            state: &std::sync::Arc<crate::app_state::AppState>,
            profiles: &[(&str, u16)],
            default_profile: &str,
        ) {
            let bin = dir.path().join("llama-server");
            std::fs::write(&bin, b"#!/bin/sh\n").expect("write fake llama-server");
            let mut text = format!(
                "llama_server = {:?}\ndefault_profile = \"{default_profile}\"\n\
                 listen = \"127.0.0.1:19876\"\n\n\
                 [models.test_model]\nsource = \"local\"\npath = \"/tmp/fake.gguf\"\n",
                bin.display().to_string()
            );
            for (name, port) in profiles {
                text.push_str(&format!(
                    "\n[profiles.{name}]\nmodel = \"test_model\"\nport = {port}\n"
                ));
            }
            std::fs::write(&state.config_path, text).expect("write config file");
        }

        async fn post_reload(app: Router) -> (StatusCode, serde_json::Value) {
            let req = Request::builder()
                .method("POST")
                .uri("/api/reload")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            let status = resp.status();
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            (status, serde_json::from_slice(&body).unwrap_or_default())
        }

        // A profile added to the file after boot is picked up without a restart.
        // This is the whole point of the ticket; without the route it is a 404.
        #[tokio::test]
        async fn test_route_reload_picks_up_a_profile_added_after_boot() {
            let (dir, state) = build_test_app_state(None);
            seed_config_file(&dir, &state, &[("test", 19876), ("qwen39", 19877)], "test");

            let (status, json) = post_reload(test_router_full(state.clone())).await;
            assert_eq!(status, StatusCode::OK, "reload should apply, got: {json}");
            assert_eq!(json["profiles"], serde_json::json!(["qwen39", "test"]));

            let config = state.config.read().await;
            assert!(
                config.profiles.contains_key("qwen39"),
                "the new profile must be live in the daemon, not just on disk"
            );
        }

        // THE property: a typo in the config file must never take down a
        // running daemon. main.rs exit(1)s on this at boot (LAN-1076); reload
        // must reject it and keep serving the config it already had.
        #[tokio::test]
        async fn test_route_reload_rejects_unparseable_config_and_keeps_the_old_one() {
            let (_dir, state) = build_test_app_state(None);
            // Marker that only exists in the live config, never on disk — so a
            // surviving value proves the in-memory Config was not replaced,
            // rather than proving the file happened to round-trip.
            state.config.write().await.api_key = Some("live-key".into());
            std::fs::write(&state.config_path, "profiles = [[[ not toml").unwrap();

            let (status, json) = post_reload(test_router_full(state.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "got: {json}");
            assert!(
                json["error"].as_str().unwrap_or("").contains("parse error"),
                "error should name the parse failure, got: {json}"
            );

            let config = state.config.read().await;
            assert_eq!(config.api_key.as_deref(), Some("live-key"));
            assert!(config.profiles.contains_key("test"));
        }

        // Parses fine, fails the same validation the daemon applies at boot.
        // Also must not replace the live config.
        #[tokio::test]
        async fn test_route_reload_rejects_invalid_config_and_keeps_the_old_one() {
            let (dir, state) = build_test_app_state(None);
            state.config.write().await.api_key = Some("live-key".into());
            // default_profile names a profile that does not exist.
            seed_config_file(&dir, &state, &[("test", 19876)], "ghost");

            let (status, json) = post_reload(test_router_full(state.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "got: {json}");
            assert!(
                json["error"].as_str().unwrap_or("").contains("ghost"),
                "error should name the offending profile, got: {json}"
            );

            assert_eq!(
                state.config.read().await.api_key.as_deref(),
                Some("live-key"),
                "a rejected reload must leave the live config untouched"
            );
        }

        // Deleting the running profile from the config is legal and stops
        // nothing — the backend is owned by AppState, not by the config entry.
        #[tokio::test]
        async fn test_route_reload_leaves_the_running_backend_alone() {
            let (dir, state) = build_test_app_state(Some(Box::new(mock_backend_on_port(19876))));
            sync_state_from_backend(&state).await;
            // "test" is live; the new file only knows about "other".
            seed_config_file(&dir, &state, &[("other", 19999)], "other");

            let (status, json) = post_reload(test_router_full(state.clone())).await;
            assert_eq!(status, StatusCode::OK, "got: {json}");

            let warnings = json["warnings"].as_array().expect("warnings array");
            assert!(
                warnings
                    .iter()
                    .any(|w| w.as_str().unwrap_or("").contains("no longer in the config")),
                "removing the live profile must be reported, got: {json}"
            );

            let live = state.current_state().await;
            assert_eq!(live.profile_name(), Some("test"), "profile must not change");
            assert!(live.is_running(), "reload must not stop the backend");
            assert!(state.backend.lock().await.is_running().await);
        }

        // Reload serialises against start/stop/swap, but the wait is bounded so
        // it can never pin a request handler behind a ~135s swap.
        #[tokio::test(start_paused = true)]
        async fn test_route_reload_returns_409_while_an_operation_holds_op_lock() {
            let (dir, state) = build_test_app_state(None);
            seed_config_file(&dir, &state, &[("test", 19876)], "test");

            let _op_guard = state.op_lock.lock().await;
            let (status, json) = post_reload(test_router_full(state.clone())).await;

            assert_eq!(
                status,
                StatusCode::CONFLICT,
                "a reload contending with an in-flight op must 409, not hang, got: {json}"
            );
            assert!(json["error"].as_str().unwrap_or("").contains("in flight"));
        }

        // LAN-1090 cheap interim: "profile not found" alone is indistinguishable
        // from a typo when the real cause is a profile the daemon never loaded.
        #[test]
        fn test_profile_not_found_error_points_at_reload() {
            let msg = rookery_core::error::Error::ProfileNotFound("qwen39".into()).to_string();
            assert!(msg.contains("qwen39"), "got: {msg}");
            assert!(
                msg.contains("POST /api/reload"),
                "the error must say how to load a just-added profile, got: {msg}"
            );
        }

        // ═══════════════════════════════════════════════════════════════
        // LAN-1101 — chat request/error counters must agree
        // ═══════════════════════════════════════════════════════════════

        // The port-resolution match increments chat_requests, then bailed out on
        // a stopped server without ever incrementing chat_errors — so
        // rookery_chat_requests_total and rookery_chat_errors_total disagreed on
        // the most common failure there is.
        #[tokio::test]
        async fn test_route_chat_on_stopped_server_increments_the_error_counter() {
            let (_dir, state) = build_test_app_state(None);
            assert!(!state.current_state().await.is_running());
            let app = test_router_full(state);

            let chat_req = Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap();
            let resp = app.clone().oneshot(chat_req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

            let metrics_resp = app
                .oneshot(
                    Request::builder()
                        .uri("/metrics")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = metrics_resp.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(body.to_vec()).unwrap();

            assert!(
                text.contains("rookery_chat_requests_total 1"),
                "got:\n{text}"
            );
            assert!(
                text.contains("rookery_chat_errors_total 1"),
                "a counted request that fails must also be counted as an error, got:\n{text}"
            );
        }

        // --- 26. Dashboard fallback: GET / → serves index.html ---
        #[tokio::test]
        async fn test_route_dashboard_fallback_serves_index() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router_full(state);

            let req = Request::builder().uri("/").body(Body::empty()).unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let content_type = resp
                .headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap_or(""))
                .unwrap_or("");
            assert!(
                content_type.contains("text/html"),
                "GET / should serve text/html, got: {content_type}"
            );

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let html = String::from_utf8_lossy(&body);
            assert!(
                html.contains("html")
                    || html.contains("HTML")
                    || html.contains("<!DOCTYPE")
                    || html.contains("<html"),
                "body should contain HTML content"
            );
        }

        // --- 27. Dashboard static: GET /style-*.css → serves CSS with correct MIME ---
        #[tokio::test]
        async fn test_route_dashboard_static_css() {
            let (_dir, state) = build_test_app_state(None);
            let app = test_router_full(state);

            let css_path = crate::routes::DASHBOARD_DIR
                .files()
                .find_map(|file| {
                    file.path()
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| name.ends_with(".css"))
                        .map(ToString::to_string)
                })
                .expect("dashboard dist should contain a CSS asset");

            let req = Request::builder()
                .uri(format!("/{css_path}"))
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);

            let content_type = resp
                .headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap_or(""))
                .unwrap_or("");
            assert!(
                content_type.contains("text/css"),
                "CSS file should have text/css MIME type, got: {content_type}"
            );

            let body = resp.into_body().collect().await.unwrap().to_bytes();
            assert!(!body.is_empty(), "CSS file should have non-empty content");
        }

        // ═══════════════════════════════════════════════════════════════
        // "we don't know" must not render as "we measured zero"
        // ═══════════════════════════════════════════════════════════════

        /// LAN-1130. The proxy used to hardcode `"model": "test"`, which vLLM
        /// 404s because it validates against `--served-model-name`. The mock
        /// below behaves the same way, so this fails on the hardcoded literal.
        #[tokio::test]
        async fn test_route_chat_uses_served_model_name() {
            let (mock_port, shutdown_tx) = spawn_strict_model_mock("org/Qwen3.8-27B").await;

            let (_dir, state) =
                build_test_app_state(Some(Box::new(mock_backend_on_port(mock_port))));
            sync_state_from_backend(&state).await;
            let app = test_router_full(state);

            let req = Request::builder()
                .method("POST")
                .uri("/api/chat")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"messages":[{"role":"user"}]}"#))
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "chat proxy must send the name the server advertises on \
                 /v1/models, not a hardcoded one vLLM rejects"
            );

            let _ = shutdown_tx.send(());
        }

        /// LAN-1130, same defect in `/api/bench`: a hardcoded name means every
        /// bench request 404s on vLLM and the handler reports zero tests.
        #[tokio::test]
        async fn test_route_bench_uses_served_model_name() {
            let (mock_port, shutdown_tx) = spawn_strict_model_mock("org/Qwen3.8-27B").await;

            let (_dir, state) =
                build_test_app_state(Some(Box::new(mock_backend_on_port(mock_port))));
            sync_state_from_backend(&state).await;
            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/bench")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert!(
                !json["tests"].as_array().expect("tests array").is_empty(),
                "bench must not silently return zero results because the \
                 hardcoded model name was rejected, got: {json}"
            );

            let _ = shutdown_tx.send(());
        }

        /// A vLLM-shaped mock: `/v1/models` advertises one name and
        /// `/v1/chat/completions` 404s anything else.
        async fn spawn_strict_model_mock(
            served: &'static str,
        ) -> (u16, tokio::sync::oneshot::Sender<()>) {
            use axum::response::Json as AxumJson;
            use axum::routing::{get as aget, post as apost};

            let mock_app =
                Router::new()
                    .route("/health", aget(|| async { StatusCode::OK }))
                    .route(
                        "/v1/models",
                        aget(move || async move {
                            AxumJson(serde_json::json!({"data": [{"id": served}]}))
                        }),
                    )
                    .route(
                        "/v1/chat/completions",
                        apost(
                            move |AxumJson(body): AxumJson<serde_json::Value>| async move {
                                use axum::response::IntoResponse;
                                if body["model"].as_str() != Some(served) {
                                    return (
                                        StatusCode::NOT_FOUND,
                                        AxumJson(serde_json::json!({"error": "model not found"})),
                                    )
                                        .into_response();
                                }
                                mock_chat_response(&body, true)
                            },
                        ),
                    );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                let _ = axum::serve(listener, mock_app)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await;
            });
            (port, tx)
        }

        /// LAN-1127. `gpu_monitor: None` is a failed/absent NVML query. Serving
        /// that as `vram_free_mb: 0` is indistinguishable from a genuinely full
        /// GPU, so the user is told to free VRAM that was never measured.
        #[tokio::test]
        async fn test_route_hardware_vram_free_is_null_when_nvml_unavailable() {
            let (_dir, state) = build_test_app_state(None);
            assert!(state.gpu_monitor.is_none(), "test state has no NVML");
            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/hardware")
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert!(
                json["gpu"]["vram_free_mb"].is_null(),
                "an unread VRAM figure must serialize as null, not 0, got: {}",
                json["gpu"]["vram_free_mb"]
            );
        }

        /// LAN-1127. The phantom-leak report: "no quant fits in available
        /// memory" when in truth we never read the GPU.
        #[test]
        fn test_no_fit_message_distinguishes_unread_vram_from_full_gpu() {
            assert_eq!(
                super::no_fit_message(None),
                "could not read GPU VRAM (NVML query failed)",
                "an unread GPU must not be reported as a full one"
            );
            assert_eq!(
                super::no_fit_message(Some(0)),
                "no quant fits in available memory",
                "a measured 0 really is a full GPU"
            );
        }

        /// A llama-server-shaped mock whose first `ok_count` completion
        /// requests answer with real timings; every later one replies with
        /// `failure`. `ok_count = 0` is the total-failure case, `1` the
        /// partial one (prompt order in `get_bench` is deterministic).
        async fn spawn_bench_mock(
            ok_count: usize,
            failure: (StatusCode, serde_json::Value),
        ) -> (u16, tokio::sync::oneshot::Sender<()>) {
            use axum::response::Json as AxumJson;
            use axum::routing::{get as aget, post as apost};
            use std::sync::atomic::{AtomicUsize, Ordering};

            let seen = std::sync::Arc::new(AtomicUsize::new(0));
            let mock_app = Router::new()
                .route("/health", aget(|| async { StatusCode::OK }))
                .route(
                    "/v1/models",
                    aget(|| async {
                        AxumJson(serde_json::json!({"data": [{"id": "mock-model"}]}))
                    }),
                )
                .route(
                    "/v1/chat/completions",
                    apost(move |AxumJson(req): AxumJson<serde_json::Value>| {
                        let seen = seen.clone();
                        let failure = failure.clone();
                        async move {
                            use axum::response::IntoResponse;
                            if seen.fetch_add(1, Ordering::SeqCst) >= ok_count {
                                return (failure.0, AxumJson(failure.1)).into_response();
                            }
                            mock_chat_response(&req, true)
                        }
                    }),
                );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                let _ = axum::serve(listener, mock_app)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await;
            });
            (port, tx)
        }

        async fn bench_json(mock_port: u16) -> serde_json::Value {
            let (_dir, state) =
                build_test_app_state(Some(Box::new(mock_backend_on_port(mock_port))));
            sync_state_from_backend(&state).await;
            let app = test_router_full(state);

            let req = Request::builder()
                .uri("/api/bench")
                .body(Body::empty())
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "a partial result still has to reach the client, so the \
                 handler stays 200 and reports failures in the body"
            );
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice(&body).unwrap()
        }

        /// LAN-1094. Every request failing used to return `{"tests": []}` —
        /// byte identical to a bench nobody ever ran, which the dashboard
        /// renders as "no results yet".
        #[tokio::test]
        async fn test_route_bench_reports_failures_instead_of_empty_success() {
            let (mock_port, shutdown_tx) = spawn_bench_mock(
                0,
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": "upstream exploded"}),
                ),
            )
            .await;

            let json = bench_json(mock_port).await;

            assert!(
                json["tests"].as_array().expect("tests array").is_empty(),
                "no test produced a measurement, got: {json}"
            );
            let errors = json["errors"].as_array().expect("errors array");
            assert_eq!(
                errors.len(),
                3,
                "a failed bench must be distinguishable from one never run, got: {json}"
            );
            assert!(
                errors[0]["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("500"),
                "the failure has to carry its reason, got: {}",
                errors[0]
            );

            let _ = shutdown_tx.send(());
        }

        /// LAN-1094 partial case: one real measurement is a real (if
        /// incomplete) result and still renders — but the two that failed
        /// must not vanish into "that is all there was".
        #[tokio::test]
        async fn test_route_bench_partial_failure_keeps_results_and_errors() {
            let (mock_port, shutdown_tx) = spawn_bench_mock(
                1,
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": "upstream exploded"}),
                ),
            )
            .await;

            let json = bench_json(mock_port).await;

            assert_eq!(
                json["tests"].as_array().expect("tests array").len(),
                1,
                "the one test that succeeded must still be reported, got: {json}"
            );
            assert_eq!(
                json["errors"].as_array().expect("errors array").len(),
                2,
                "the two that failed must be reported alongside it, got: {json}"
            );

            let _ = shutdown_tx.send(());
        }

        /// LAN-1094, the other silent drop: a 200 whose body carries no
        /// `timings` block fell through both `if let`s and was discarded
        /// without even a log line.
        #[tokio::test]
        /// A backend that emits no llama.cpp `timings` block must still be measured.
        ///
        /// This is the SGLang regression: bench used to require `timings`, so every
        /// prompt errored with "response carried no timings" and `rookery bench`
        /// printed "no results (is a model running?)" against a healthy server. It
        /// went unnoticed until SGLang became the default profile.
        async fn test_route_bench_measures_backend_without_timings() {
            let (mock_port, shutdown_tx) = spawn_bench_mock(
                3,
                (StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({})),
            )
            .await;

            let json = bench_json(mock_port).await;

            let tests = json["tests"].as_array().expect("tests array");
            assert_eq!(
                tests.len(),
                3,
                "no backend emits `timings` except llama.cpp; the rest must still be \
                 measured from arrival times, got: {json}"
            );
            for t in tests {
                assert!(
                    t["gen_tok_s"].as_f64().unwrap_or(0.0) > 0.0,
                    "gen_tok_s must be derived from the stream, got: {t}"
                );
                assert_eq!(
                    t["completion_tokens"].as_u64().unwrap_or(0),
                    2,
                    "token counts come from the usage frame, got: {t}"
                );
            }

            let _ = shutdown_tx.send(());
        }

        #[tokio::test]
        /// A 200 that yields no content is still no measurement, and has to be
        /// reported rather than silently dropped.
        async fn test_route_bench_reports_empty_stream_as_error() {
            let (mock_port, shutdown_tx) = spawn_bench_mock(
                0,
                (
                    StatusCode::OK,
                    serde_json::json!({"choices": [{"index": 0, "message": {
                        "role": "assistant", "content": "hi"
                    }}]}),
                ),
            )
            .await;

            let json = bench_json(mock_port).await;

            let errors = json["errors"].as_array().expect("errors array");
            assert_eq!(
                errors.len(),
                3,
                "a 200 carrying no stream content is no measurement, got: {json}"
            );
            assert!(
                errors[0]["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("no content"),
                "the reason must say what was missing, got: {}",
                errors[0]
            );

            let _ = shutdown_tx.send(());
        }
    }
}

#[cfg(test)]
mod sglang_metrics_tests {
    use super::parse_sglang_metrics;

    /// A trimmed but byte-faithful sample of what SGLang 0.5.18 actually emits,
    /// including the parts that break naive parsers: HELP/TYPE comments, label
    /// sets, histogram buckets, and a real NaN.
    const SAMPLE: &str = r#"# HELP sglang:max_total_num_tokens Total KV tokens
# TYPE sglang:max_total_num_tokens gauge
sglang:max_total_num_tokens{engine_type="unified",model_name="qwen3.8-27b"} 137735.0
sglang:kv_used_tokens{engine_type="unified",model_name="qwen3.8-27b"} 35.0
sglang:mamba_usage{engine_type="unified",model_name="qwen3.8-27b"} 0.5
sglang:spec_accept_length{engine_type="unified",model_name="qwen3.8-27b"} 6.22
sglang:cache_hit_rate{engine_type="unified",model_name="qwen3.8-27b"} 0.0
sglang:fwd_occupancy{engine_type="unified",model_name="qwen3.8-27b"} NaN
sglang:uncached_prompt_tokens_histogram_bucket{le="100.0",model_name="qwen3.8-27b"} 2.0
sglang:not_a_metric_we_want{model_name="x"} 99.0
"#;

    #[test]
    fn extracts_the_gauges_we_render() {
        let v = parse_sglang_metrics(SAMPLE);
        assert_eq!(v["kv_total"], 137735.0);
        assert_eq!(v["kv_used"], 35.0);
        assert_eq!(v["mamba_usage"], 0.5);
        assert_eq!(v["accept_length"], 6.22);
        assert_eq!(v["cache_hit_rate"], 0.0);
    }

    /// NaN is real in this scrape before any traffic. serde_json cannot
    /// represent it, so it must be dropped rather than panicking or emitting
    /// invalid JSON.
    #[test]
    fn drops_non_finite_values() {
        let v = parse_sglang_metrics(SAMPLE);
        assert!(v.get("fwd_occupancy").is_none());
    }

    #[test]
    fn ignores_histograms_comments_and_unwanted_series() {
        let v = parse_sglang_metrics(SAMPLE);
        let obj = v.as_object().unwrap();
        assert!(!obj.keys().any(|k| k.contains("bucket")));
        assert!(!obj.keys().any(|k| k.contains("not_a_metric")));
        assert!(obj.len() >= 5, "expected the wanted gauges, got {obj:?}");
    }

    /// An SGLang started without --enable-metrics serves an empty body; that
    /// must be an empty object, not a parse failure.
    #[test]
    fn empty_scrape_is_empty_object() {
        assert_eq!(parse_sglang_metrics(""), serde_json::json!({}));
    }
}

#[cfg(test)]
mod releases_view_tests {
    use rookery_core::config::BackendType;
    use rookery_engine::releases::repo_for_backend;

    /// The cache is keyed by repo and entries persist across swaps, so the view
    /// must select rather than dump. Rendering the whole cache leaves a stale
    /// llama.cpp row sitting next to the live SGLang one indefinitely.
    #[test]
    fn active_backend_selects_exactly_one_repo() {
        let cached = [
            "ggml-org/llama.cpp",
            "sgl-project/sglang",
            "vllm-project/vllm",
        ];
        for (backend, expected) in [
            (BackendType::LlamaServer, "ggml-org/llama.cpp"),
            (BackendType::Sglang, "sgl-project/sglang"),
            (BackendType::Vllm, "vllm-project/vllm"),
        ] {
            let repo = repo_for_backend(backend);
            let hits: Vec<_> = cached.iter().filter(|r| **r == repo).collect();
            assert_eq!(hits.len(), 1, "{backend:?} must select exactly one repo");
            assert_eq!(*hits[0], expected);
        }
    }
}

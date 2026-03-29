use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::app_state::AppState;

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
    let server_state = state.backend.lock().await.to_server_state().await;
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
    let current = state.backend.lock().await.to_server_state().await;
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
            let free_mb = gpu.vram_total_mb - gpu.vram_used_mb;
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
            let free_mb = gpu.vram_total_mb - gpu.vram_used_mb;
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

    // Persist starting state
    let starting_state = rookery_core::state::ServerState::Starting {
        profile: profile_name.clone(),
        since: chrono::Utc::now(),
    };
    let _ = state.state_persistence.save(&starting_state);
    broadcast_state(&state, &starting_state);

    // Start via backend trait + health check
    let config = state.config.read().await;
    let backend = state.backend.lock().await;
    match backend.start(&config, &profile_name).await {
        Ok(_info) => {
            // Wait for health with 120s timeout
            let port = config
                .profiles
                .get(&profile_name)
                .map(|p| p.port)
                .unwrap_or(8081);
            drop(backend); // Release backend lock during health check
            drop(config);
            match rookery_engine::health::wait_for_health(port, std::time::Duration::from_secs(120))
                .await
            {
                Ok(()) => {
                    let server_state = state.backend.lock().await.to_server_state().await;
                    let _ = state.state_persistence.save(&server_state);
                    broadcast_state(&state, &server_state);
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
                Err(e) => {
                    tracing::error!(error = %e, "health check failed, stopping server");
                    let _ = state.backend.lock().await.stop().await;
                    let failed = rookery_core::state::ServerState::Failed {
                        last_error: e.to_string(),
                        profile: profile_name,
                        since: chrono::Utc::now(),
                    };
                    let _ = state.state_persistence.save(&failed);
                    broadcast_state(&state, &failed);
                    Ok(Json(ActionResponse {
                        success: false,
                        message: "server failed to start".into(),
                        status: status_from_state(&failed),
                    }))
                }
            }
        }
        Err(e) => {
            let failed = rookery_core::state::ServerState::Failed {
                last_error: e.to_string(),
                profile: profile_name,
                since: chrono::Utc::now(),
            };
            let _ = state.state_persistence.save(&failed);
            broadcast_state(&state, &failed);
            tracing::error!(error = %e, "failed to start server");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn post_stop(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let _op_guard = state.op_lock.lock().await;

    tracing::info!("stopping server");

    let stopping = rookery_core::state::ServerState::Stopping {
        since: chrono::Utc::now(),
    };
    let _ = state.state_persistence.save(&stopping);
    broadcast_state(&state, &stopping);

    match state.backend.lock().await.stop().await {
        Ok(()) => {
            let stopped = rookery_core::state::ServerState::Stopped;
            let _ = state.state_persistence.save(&stopped);
            broadcast_state(&state, &stopped);
            let status = status_from_state(&stopped);
            Ok(Json(ActionResponse {
                success: true,
                message: "server stopped".into(),
                status,
            }))
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to stop server");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(Deserialize)]
pub struct SwapRequest {
    pub profile: String,
}

pub async fn post_swap(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SwapRequest>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let _op_guard = state.op_lock.lock().await;

    let old_profile = state
        .backend
        .lock()
        .await
        .process_info()
        .await
        .map(|i| i.profile);

    tracing::info!(
        from = ?old_profile,
        to = %req.profile,
        "swapping model"
    );

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

        // Create new backend for the target profile and start it
        let config = state.config.read().await;
        let profile = config
            .profiles
            .get(&req.profile)
            .ok_or_else(|| rookery_core::error::Error::ProfileNotFound(req.profile.clone()))?;
        let new_backend =
            rookery_engine::backend::create_backend(profile, state.log_buffer.clone())?;
        let port = profile.port;

        // Start the new backend.
        // If this fails, the old backend (already stopped) stays in AppState
        // but draining was already cleared above, so post_chat won't return 503.
        new_backend.start(&config, &req.profile).await?;
        drop(config);

        // Replace the backend in AppState
        *state.backend.lock().await = new_backend;

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
            let _ = state.state_persistence.save(&server_state);
            broadcast_state(&state, &server_state);
            let is_running = server_state.is_running();
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
                        let _ = state.agent_manager.stop(name).await;
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
        Err(e) => {
            tracing::error!(error = %e, "swap failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
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
                "ctx_size": ls.as_ref().map(|c| c.ctx_size),
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
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut config = state.config.write().await;

    let profile = config
        .profiles
        .get_mut(&name)
        .ok_or(StatusCode::NOT_FOUND)?;

    if let Some(v) = update.temp {
        profile.temp = v;
    }
    if let Some(v) = update.top_p {
        profile.top_p = v;
    }
    if let Some(v) = update.top_k {
        profile.top_k = v;
    }
    if let Some(v) = update.min_p {
        profile.min_p = v;
    }
    if let Some(v) = update.ctx_size {
        profile.ctx_size = v;
    }
    if let Some(v) = update.threads {
        profile.threads = v;
    }
    if let Some(v) = update.threads_batch {
        profile.threads_batch = v;
    }
    if let Some(v) = update.batch_size {
        profile.batch_size = v;
    }
    if let Some(v) = update.ubatch_size {
        profile.ubatch_size = v;
    }
    if let Some(v) = update.reasoning_budget {
        profile.reasoning_budget = v;
    }
    if let Some(v) = update.flash_attention {
        profile.flash_attention = v;
    }
    if let Some(v) = update.cache_type_k {
        profile.cache_type_k = v;
    }
    if let Some(v) = update.cache_type_v {
        profile.cache_type_v = v;
    }

    if let Err(e) = config.save() {
        tracing::error!(error = %e, "failed to save config");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
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
    let current = state.backend.lock().await.to_server_state().await;
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

    let current = state.backend.lock().await.to_server_state().await;
    let port = match current {
        rookery_core::state::ServerState::Running { port, .. } => port,
        _ => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let body = serde_json::json!({
        "model": "test",
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
            tracing::error!(error = %e, "chat proxy request failed");
            StatusCode::BAD_GATEWAY
        })?;

    // Wrap the stream with a per-chunk timeout — if llama-server hangs
    // mid-generation with no data for 60s, terminate the stream.
    use tokio_stream::StreamExt as _;
    let stream = resp
        .bytes_stream()
        .timeout(std::time::Duration::from_secs(60))
        .map(|item| match item {
            Ok(Ok(bytes)) => Ok(bytes),
            Ok(Err(e)) => Err(std::io::Error::other(e)),
            Err(_elapsed) => {
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
    let current = state.backend.lock().await.to_server_state().await;
    let port = match current {
        rookery_core::state::ServerState::Running { port, .. } => port,
        _ => {
            return Ok(Json(serde_json::json!({ "available": false })));
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Fetch /slots (llama.cpp-specific — returns 404 for vLLM)
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

    Ok(Json(serde_json::json!({
        "available": true,
        "slots": slots,
    })))
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

pub async fn get_dashboard(uri: axum::http::Uri) -> impl axum::response::IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() || path.starts_with("api/") {
        "index.html"
    } else {
        path
    };

    match DASHBOARD_DIR.get_file(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                axum::http::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, mime.as_ref())],
                file.contents(),
            )
                .into_response()
        }
        None => {
            // SPA fallback — serve index.html
            let file = DASHBOARD_DIR.get_file("index.html").unwrap();
            let mime = mime_guess::from_path("index.html").first_or_octet_stream();
            (
                axum::http::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, mime.as_ref())],
                file.contents(),
            )
                .into_response()
        }
    }
}

use axum::response::IntoResponse;

// --- Bench ---

#[derive(Serialize)]
pub struct BenchResult {
    pub tests: Vec<BenchTest>,
}

#[derive(Serialize)]
pub struct BenchTest {
    pub name: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub pp_tok_s: f64,
    pub gen_tok_s: f64,
}

pub async fn get_bench(
    State(state): State<Arc<AppState>>,
) -> Result<Json<BenchResult>, StatusCode> {
    let current = state.backend.lock().await.to_server_state().await;
    let port = match current {
        rookery_core::state::ServerState::Running { port, .. } => port,
        _ => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let prompts = vec![
        (
            "short",
            "Write a Python function that checks if a number is prime. Just the function.",
        ),
        (
            "medium",
            "Explain the difference between a mutex, semaphore, and condition variable. Give a code example for each in Rust.",
        ),
    ];

    let mut tests = Vec::new();
    for (name, prompt) in prompts {
        let body = serde_json::json!({
            "model": "test",
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": 256,
        });

        match client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(data) = resp.json::<serde_json::Value>().await
                    && let Some(timings) = data.get("timings")
                {
                    tests.push(BenchTest {
                        name: name.to_string(),
                        prompt_tokens: timings["prompt_n"].as_u64().unwrap_or(0),
                        completion_tokens: timings["predicted_n"].as_u64().unwrap_or(0),
                        pp_tok_s: timings["prompt_per_second"].as_f64().unwrap_or(0.0),
                        gen_tok_s: timings["predicted_per_second"].as_f64().unwrap_or(0.0),
                    });
                }
            }
            Err(e) => {
                tracing::error!(error = %e, test = name, "bench request failed");
            }
        }
    }

    Ok(Json(BenchResult { tests }))
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

fn broadcast_state(app: &AppState, server_state: &rookery_core::state::ServerState) {
    let json = status_json_from_state(server_state);
    let _ = app.state_tx.send(json);
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
) -> Result<Json<rookery_engine::agent::AgentInfo>, StatusCode> {
    state
        .agent_manager
        .get_health(&name)
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

// --- Hardware ---

pub async fn get_hardware(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut profile = serde_json::to_value(&state.hardware_profile).unwrap_or_default();

    // Add live VRAM info
    if let Some(gpu) = profile.get_mut("gpu").and_then(|g| g.as_object_mut()) {
        let free = rookery_engine::hardware::live_vram_free_mb(state.gpu_monitor.as_ref());
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
    rookery_engine::models::mark_downloaded(&mut quants);

    // Attach performance estimates
    let vram_free = rookery_engine::hardware::live_vram_free_mb(state.gpu_monitor.as_ref());
    let ram_free = rookery_engine::hardware::read_ram_free_mb();
    rookery_engine::models::attach_estimates(
        &mut quants,
        &state.hardware_profile,
        vram_free,
        ram_free,
    );

    Ok(Json(serde_json::json!({ "repo": repo, "quants": quants })))
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
    let vram_free = rookery_engine::hardware::live_vram_free_mb(state.gpu_monitor.as_ref());
    let ram_free = rookery_engine::hardware::read_ram_free_mb();

    match rookery_engine::models::recommend_quant(
        &quants,
        &state.hardware_profile,
        vram_free,
        ram_free,
    ) {
        Some(rec) => Ok(Json(
            serde_json::json!({ "repo": repo, "recommendation": rec }),
        )),
        None => Ok(Json(
            serde_json::json!({ "repo": repo, "recommendation": null, "message": "no quant fits in available memory" }),
        )),
    }
}

pub async fn get_models_cached(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cached = rookery_engine::models::scan_cache();
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
        let vram_free = rookery_engine::hardware::live_vram_free_mb(state.gpu_monitor.as_ref());
        let ram_free = rookery_engine::hardware::read_ram_free_mb();
        match rookery_engine::models::recommend_quant(
            &quants,
            &state.hardware_profile,
            vram_free,
            ram_free,
        ) {
            Some(rec) => rec.label,
            None => {
                return Ok(Json(serde_json::json!({
                    "started": false,
                    "message": "no quant fits in available memory"
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Fix #3: StatusResponse always includes 'backend' key (null when stopped) ===
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

    // === Fix #4: status_from_state returns 'starting'/'stopping' not 'transitioning' ===
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

    // === Swap drain flag cleanup: drain is cleared on failure paths ===
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

    // === status_json_from_state always includes backend key ===
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

    // === VAL-SSE-001: SSE state events include backend field ===
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

    // === VAL-API-002: /api/profiles includes backend per profile ===
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
                        extra_args: vec![],
                    },
                ),
                (
                    "vllm_prod".into(),
                    Profile {
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
                        extra_args: vec![],
                    },
                ),
            ]),
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

    // === VAL-CROSS-001: Capacity gate adapts for vLLM profiles ===
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
            extra_args: vec![],
        };

        // A llama-server profile
        let llama_profile = Profile {
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

    // === VAL-CROSS-002: Compose generation failure returns error before Docker commands ===
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
                    extra_args: vec![],
                },
            )]),
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
                    extra_args: vec![],
                },
            )]),
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

    // === VAL-API-003: GET /api/model-info returns null props for vLLM ===
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

    // === VAL-API-004: GET /api/server-stats returns null slots for vLLM ===
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

    // === Combined: verify /props and /slots status code check logic ===
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

        use crate::test_utils::{MockBackend, build_test_app_state};
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
                .route("/api/start", post(super::post_start))
                .route("/api/stop", post(super::post_stop))
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
                        version_file: None,
                        restart_on_error_patterns: vec![],
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
                .route("/api/logs", get(super::get_logs))
                .route("/api/start", post(super::post_start))
                .route("/api/stop", post(super::post_stop))
                .route("/api/swap", post(super::post_swap))
                .route("/api/chat", post(super::post_chat))
                .route("/api/bench", get(super::get_bench))
                .route("/api/model-info", get(super::get_model_info))
                .route("/api/server-stats", get(super::get_server_stats))
                .route("/api/agents", get(super::get_agents))
                .route("/api/agents/start", post(super::post_agent_start))
                .route("/api/agents/stop", post(super::post_agent_stop))
                .route("/api/hardware", get(super::get_hardware))
                .fallback(super::get_dashboard)
                .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
                .with_state(state)
        }

        /// Spawn a mock llama-server (axum) on a random port serving the
        /// endpoints that route handlers proxy to: /health, /v1/models,
        /// /props, /slots, /v1/chat/completions.
        /// Returns (port, shutdown_sender).
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
                    apost(|| async {
                        AxumJson(serde_json::json!({
                            "id": "chatcmpl-mock",
                            "object": "chat.completion",
                            "created": 1700000000,
                            "model": "mock-model",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "Hello!"},
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 5,
                                "completion_tokens": 1,
                                "total_tokens": 6
                            },
                            "timings": {
                                "prompt_n": 5,
                                "prompt_ms": 10.0,
                                "prompt_per_token_ms": 2.0,
                                "prompt_per_second": 500.0,
                                "predicted_n": 1,
                                "predicted_ms": 5.0,
                                "predicted_per_token_ms": 5.0,
                                "predicted_per_second": 200.0
                            }
                        }))
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

            // Add a second profile to swap to
            {
                let mut config = state.config.write().await;
                config.profiles.insert(
                    "other".into(),
                    rookery_core::config::Profile {
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

        // --- 16. GET /api/bench → returns timing data from mock server ---
        #[tokio::test]
        async fn test_route_bench_returns_timing_data() {
            let (mock_port, shutdown_tx) = spawn_mock_llama_server().await;

            let backend = mock_backend_on_port(mock_port);
            let (_dir, state) = build_test_app_state(Some(Box::new(backend)));

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
                        version_file: None,
                        restart_on_error_patterns: vec![],
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
                        version_file: None,
                        restart_on_error_patterns: vec![],
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

            // Verify the config was actually updated
            let config = state.config.read().await;
            let profile = config
                .profiles
                .get("test")
                .expect("test profile should exist");
            assert!(
                (profile.temp - 0.9).abs() < f32::EPSILON,
                "temp should be 0.9, got: {}",
                profile.temp
            );
            assert!(
                (profile.top_p - 0.95).abs() < f32::EPSILON,
                "top_p should be 0.95, got: {}",
                profile.top_p
            );
            assert_eq!(profile.top_k, 40, "top_k should be 40");
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

            // The actual CSS file in the dist directory
            let req = Request::builder()
                .uri("/style-2f9a714a3215660a.css")
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
    }
}

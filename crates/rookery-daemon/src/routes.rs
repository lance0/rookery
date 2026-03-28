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
    {
        let config = state.config.read().await;
        profile_name = config
            .resolve_profile_name(req.profile.as_deref())
            .to_string();

        estimated_vram_mb = config
            .profiles
            .get(&profile_name)
            .and_then(|p| config.models.get(&p.model))
            .and_then(|m| m.estimated_vram_mb);
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

    // Capacity gate: check VRAM before starting
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
    let swap_result: std::result::Result<
        rookery_core::state::ServerState,
        rookery_core::error::Error,
    > = async {
        // Drain in-flight requests if currently running
        {
            let backend = state.backend.lock().await;
            if backend.is_running().await {
                backend.set_draining(true);
                tracing::info!("draining in-flight requests (5s)");
            }
        }

        // Drain period
        if state.backend.lock().await.is_running().await {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            state.backend.lock().await.stop().await?;
        }
        // Draining is cleared implicitly: the old backend is replaced below,
        // and the new backend starts with draining=false.

        // Create new backend for the target profile and start it
        let config = state.config.read().await;
        let profile = config
            .profiles
            .get(&req.profile)
            .ok_or_else(|| rookery_core::error::Error::ProfileNotFound(req.profile.clone()))?;
        let new_backend =
            rookery_engine::backend::create_backend(profile, state.log_buffer.clone())?;
        let port = profile.port;

        // Start the new backend
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
    #[serde(skip_serializing_if = "Option::is_none")]
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

    // Fetch /props
    let props = if let Ok(resp) = client
        .get(format!("http://127.0.0.1:{port}/props"))
        .send()
        .await
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

    // Fetch /slots
    let slots = if let Ok(resp) = client
        .get(format!("http://127.0.0.1:{port}/slots"))
        .send()
        .await
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
}

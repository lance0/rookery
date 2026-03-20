use axum::extract::{Query, State};
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
}

pub async fn get_status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let server_state = state.process_manager.to_server_state().await;

    let (state_name, profile, pid, port, uptime_secs) = match &server_state {
        rookery_core::state::ServerState::Stopped => ("stopped".into(), None, None, None, None),
        rookery_core::state::ServerState::Starting { profile, since } => (
            "starting".into(),
            Some(profile.clone()),
            None,
            None,
            Some(chrono::Utc::now().signed_duration_since(*since).num_seconds()),
        ),
        rookery_core::state::ServerState::Running {
            profile,
            pid,
            port,
            since,
            ..
        } => (
            "running".into(),
            Some(profile.clone()),
            Some(*pid),
            Some(*port),
            Some(chrono::Utc::now().signed_duration_since(*since).num_seconds()),
        ),
        rookery_core::state::ServerState::Stopping { since } => (
            "stopping".into(),
            None,
            None,
            None,
            Some(chrono::Utc::now().signed_duration_since(*since).num_seconds()),
        ),
        rookery_core::state::ServerState::Failed {
            last_error,
            profile,
            ..
        } => (
            format!("failed: {last_error}"),
            Some(profile.clone()),
            None,
            None,
            None,
        ),
    };

    Json(StatusResponse {
        state: state_name,
        profile,
        pid,
        port,
        uptime_secs,
    })
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
    let profile_name = state
        .config
        .resolve_profile_name(req.profile.as_deref())
        .to_string();

    // Idempotent: if already running with same profile, no-op
    let current = state.process_manager.to_server_state().await;
    if let rookery_core::state::ServerState::Running { ref profile, pid, port, .. } = current {
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
    if let Some(ref monitor) = state.gpu_monitor {
        let model_name = state.config.profiles.get(&profile_name)
            .map(|p| p.model.as_str());
        if let Some(model_name) = model_name {
            if let Some(model) = state.config.models.get(model_name) {
                if let Some(estimated_mb) = model.estimated_vram_mb {
                    if let Ok(stats) = monitor.stats() {
                        if let Some(gpu) = stats.first() {
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
                    }
                }
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

    match state
        .process_manager
        .start_and_wait(&state.config, &profile_name)
        .await
    {
        Ok(server_state) => {
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
    tracing::info!("stopping server");

    let stopping = rookery_core::state::ServerState::Stopping {
        since: chrono::Utc::now(),
    };
    let _ = state.state_persistence.save(&stopping);
    broadcast_state(&state, &stopping);

    match state.process_manager.stop().await {
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
    let old_profile = state
        .process_manager
        .process_info()
        .await
        .map(|i| i.profile);

    tracing::info!(
        from = ?old_profile,
        to = %req.profile,
        "swapping model"
    );

    match state
        .process_manager
        .swap(&state.config, &req.profile)
        .await
    {
        Ok(server_state) => {
            let _ = state.state_persistence.save(&server_state);
            broadcast_state(&state, &server_state);
            let is_running = server_state.is_running();
            let status = status_from_state(&server_state);

            // Restart agents that have restart_on_swap = true
            if is_running {
                for (name, agent_config) in &state.config.agents {
                    if agent_config.restart_on_swap && state.agent_manager.is_running(name).await {
                        tracing::info!(agent = %name, "restarting agent after swap");
                        let _ = state.agent_manager.stop(name).await;
                        let _ = state.agent_manager.start(name, agent_config).await;
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
    let profiles: Vec<serde_json::Value> = state
        .config
        .profiles
        .iter()
        .map(|(name, p)| {
            let is_default = name == &state.config.default_profile;
            let model = state.config.models.get(&p.model);
            serde_json::json!({
                "name": name,
                "model": p.model,
                "port": p.port,
                "ctx_size": p.ctx_size,
                "reasoning_budget": p.reasoning_budget,
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

pub async fn get_dashboard() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("dashboard.html"))
}

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
    let current = state.process_manager.to_server_state().await;
    let port = match current {
        rookery_core::state::ServerState::Running { port, .. } => port,
        _ => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let prompts = vec![
        ("short", "Write a Python function that checks if a number is prime. Just the function."),
        ("medium", "Explain the difference between a mutex, semaphore, and condition variable. Give a code example for each in Rust."),
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
                if let Ok(data) = resp.json::<serde_json::Value>().await {
                    if let Some(timings) = data.get("timings") {
                        tests.push(BenchTest {
                            name: name.to_string(),
                            prompt_tokens: timings["prompt_n"].as_u64().unwrap_or(0),
                            completion_tokens: timings["predicted_n"].as_u64().unwrap_or(0),
                            pp_tok_s: timings["prompt_per_second"].as_f64().unwrap_or(0.0),
                            gen_tok_s: timings["predicted_per_second"].as_f64().unwrap_or(0.0),
                        });
                    }
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
    let running = state.agent_manager.list().await;
    let configured: Vec<String> = state.config.agents.keys().cloned().collect();
    Json(AgentsResponse {
        agents: running,
        configured,
    })
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
    let config = state
        .config
        .agents
        .get(&req.name)
        .ok_or(StatusCode::NOT_FOUND)?;

    match state.agent_manager.start(&req.name, config).await {
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

fn status_from_state(state: &rookery_core::state::ServerState) -> StatusResponse {
    match state {
        rookery_core::state::ServerState::Stopped => StatusResponse {
            state: "stopped".into(),
            profile: None,
            pid: None,
            port: None,
            uptime_secs: None,
        },
        rookery_core::state::ServerState::Running {
            profile,
            pid,
            port,
            since,
            ..
        } => StatusResponse {
            state: "running".into(),
            profile: Some(profile.clone()),
            pid: Some(*pid),
            port: Some(*port),
            uptime_secs: Some(chrono::Utc::now().signed_duration_since(*since).num_seconds()),
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
        },
        _ => StatusResponse {
            state: "transitioning".into(),
            profile: None,
            pid: None,
            port: None,
            uptime_secs: None,
        },
    }
}

use axum::extract::State;
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

    tracing::info!(profile = %profile_name, "starting server");

    // Persist starting state
    let starting_state = rookery_core::state::ServerState::Starting {
        profile: profile_name.clone(),
        since: chrono::Utc::now(),
    };
    let _ = state.state_persistence.save(&starting_state);

    match state
        .process_manager
        .start_and_wait(&state.config, &profile_name)
        .await
    {
        Ok(server_state) => {
            let _ = state.state_persistence.save(&server_state);
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

    match state.process_manager.stop().await {
        Ok(()) => {
            let stopped = rookery_core::state::ServerState::Stopped;
            let _ = state.state_persistence.save(&stopped);
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

pub async fn get_health() -> StatusCode {
    StatusCode::OK
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

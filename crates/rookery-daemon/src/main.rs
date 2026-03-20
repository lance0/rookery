mod app_state;
mod routes;

use app_state::AppState;
use axum::routing::{get, post};
use axum::Router;
use rookery_core::config::Config;
use rookery_core::state::StatePersistence;
use rookery_engine::gpu::GpuMonitor;
use rookery_engine::logs::LogBuffer;
use rookery_engine::agent::AgentManager;
use rookery_engine::process::ProcessManager;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rookery=info".parse().unwrap()),
        )
        .init();

    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to load config: {e}");
            eprintln!("config path: {}", Config::config_path().display());
            std::process::exit(1);
        }
    };

    let listen = config.listen;

    // Init GPU monitor (non-fatal if NVML unavailable)
    let gpu_monitor = match GpuMonitor::new() {
        Ok(m) => {
            tracing::info!("NVML initialized");
            Some(m)
        }
        Err(e) => {
            tracing::warn!(error = %e, "NVML unavailable, GPU monitoring disabled");
            None
        }
    };

    // Init log buffer
    let log_buffer = Arc::new(LogBuffer::new(10_000));

    // Init process manager and agent manager
    let process_manager = ProcessManager::new(log_buffer.clone());
    let agent_manager = AgentManager::new(log_buffer.clone());

    // Load and reconcile persisted state
    let state_persistence = StatePersistence::new();
    if let Ok(prev_state) = state_persistence.load() {
        let reconciled = state_persistence.reconcile(prev_state);
        tracing::info!(state = ?format!("{:?}", reconciled), "reconciled previous state");
        let _ = state_persistence.save(&reconciled);
    }

    let state = Arc::new(AppState {
        config,
        process_manager,
        agent_manager,
        gpu_monitor,
        log_buffer,
        state_persistence,
    });

    let app = Router::new()
        .route("/api/health", get(routes::get_health))
        .route("/api/status", get(routes::get_status))
        .route("/api/gpu", get(routes::get_gpu))
        .route("/api/start", post(routes::post_start))
        .route("/api/stop", post(routes::post_stop))
        .route("/api/agents", get(routes::get_agents))
        .route("/api/agents/start", post(routes::post_agent_start))
        .route("/api/agents/stop", post(routes::post_agent_stop))
        .with_state(state);

    tracing::info!(%listen, "rookeryd starting");

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .expect("failed to bind");

    tracing::info!(%listen, "rookeryd listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    tracing::info!("rookeryd shut down");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install CTRL+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received CTRL+C"),
        () = terminate => tracing::info!("received SIGTERM"),
    }
}

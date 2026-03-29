mod app_state;
pub mod canary;
mod routes;
mod sse;
#[cfg(test)]
pub mod test_utils;

use app_state::AppState;
use axum::Router;
use axum::routing::{get, post, put};
use rookery_core::config::Config;
use rookery_core::state::{AgentPersistence, StatePersistence};
use rookery_engine::agent::AgentManager;
use rookery_engine::backend::{self, BackendInfo, InferenceBackend};
use rookery_engine::gpu::GpuMonitor;
use rookery_engine::logs::LogBuffer;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, broadcast};

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

    // Init backend (defaults to LlamaServerBackend for the default profile)
    // This will be replaced during reconciliation if a different profile was running.
    let initial_backend: Box<dyn InferenceBackend> = {
        let default_profile_name = &config.default_profile;
        let profile_for_backend = if let Some(profile) = config.profiles.get(default_profile_name) {
            profile
        } else {
            config
                .profiles
                .values()
                .next()
                .unwrap_or_else(|| panic!("no profiles configured"))
        };
        match backend::create_backend(profile_for_backend, log_buffer.clone()) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "default profile backend unavailable, falling back to LlamaServerBackend");
                // Fallback: create a LlamaServerBackend as a placeholder
                Box::new(rookery_engine::backend::LlamaServerBackend::new(
                    log_buffer.clone(),
                ))
            }
        }
    };
    let backend: Arc<tokio::sync::Mutex<Box<dyn InferenceBackend>>> =
        Arc::new(tokio::sync::Mutex::new(initial_backend));
    let agent_manager = Arc::new(AgentManager::new(log_buffer.clone()));

    // State change broadcast channel
    let (state_tx, _) = broadcast::channel::<serde_json::Value>(64);

    // Load and reconcile persisted state
    let state_persistence = StatePersistence::new();
    let tracked_pid = if let Ok(prev_state) = state_persistence.load() {
        let reconciled = state_persistence.reconcile(prev_state);
        let pid = reconciled.pid();
        tracing::info!(state = ?format!("{:?}", reconciled), "reconciled previous state");
        let _ = state_persistence.save(&reconciled);

        // Adopt the running process via the backend trait — but verify it's actually healthy first
        if let rookery_core::state::ServerState::Running {
            ref profile,
            pid: running_pid,
            port,
            ref since,
            ref command_line,
            ref exe_path,
            ref backend_type,
            ref container_id,
            ..
        } = reconciled
        {
            if !rookery_engine::process::is_pid_alive(running_pid) {
                tracing::warn!(
                    pid = running_pid,
                    "adopted process is a zombie — marking stopped"
                );
                let stopped = rookery_core::state::ServerState::Stopped;
                let _ = state_persistence.save(&stopped);
            } else if rookery_engine::health::check_health(port, std::time::Duration::from_secs(3))
                .await
            {
                // Health endpoint responds — now verify inference actually works
                if rookery_engine::health::check_inference(port, std::time::Duration::from_secs(10))
                    .await
                {
                    tracing::info!(
                        pid = running_pid,
                        port,
                        "adopted process is healthy (inference canary passed)"
                    );

                    // Create the correct backend for the reconciled profile and adopt
                    let adopt_info = BackendInfo {
                        pid: Some(running_pid),
                        container_id: container_id.clone(),
                        port,
                        profile: profile.clone(),
                        started_at: *since,
                        backend_type: *backend_type,
                        command_line: command_line.clone(),
                        exe_path: exe_path.clone(),
                    };

                    // If the reconciled profile has a different backend type, create the right backend
                    if let Some(profile_cfg) = config.profiles.get(profile) {
                        match backend::create_backend(profile_cfg, log_buffer.clone()) {
                            Ok(correct_backend) => {
                                if let Err(e) = correct_backend.adopt(adopt_info).await {
                                    tracing::warn!(error = %e, "failed to adopt into backend");
                                }
                                *backend.lock().await = correct_backend;
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "failed to create backend for reconciled profile");
                            }
                        }
                    } else {
                        // Profile no longer in config — adopt into current backend
                        if let Err(e) = backend.lock().await.adopt(adopt_info).await {
                            tracing::warn!(error = %e, "failed to adopt into backend");
                        }
                    }
                } else {
                    tracing::warn!(
                        pid = running_pid,
                        port,
                        "adopted process failed inference canary — marking stopped"
                    );
                    let stopped = rookery_core::state::ServerState::Stopped;
                    let _ = state_persistence.save(&stopped);
                }
            } else {
                tracing::warn!(
                    pid = running_pid,
                    port,
                    "adopted process failed health check — marking stopped"
                );
                let stopped = rookery_core::state::ServerState::Stopped;
                let _ = state_persistence.save(&stopped);
            }
        }

        pid
    } else {
        None
    };

    // Kill orphan llama-server processes hogging VRAM
    if let Some(ref monitor) = gpu_monitor {
        let orphans = monitor.find_orphan_llama_servers(tracked_pid);
        for orphan in &orphans {
            tracing::warn!(
                pid = orphan.pid,
                vram_mb = orphan.vram_mb,
                "killing orphan llama-server ({}MB VRAM)",
                orphan.vram_mb
            );
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(orphan.pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        if !orphans.is_empty() {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            // SIGKILL any that didn't exit
            for orphan in &orphans {
                let proc_path = std::path::PathBuf::from(format!("/proc/{}", orphan.pid));
                if proc_path.exists() {
                    tracing::warn!(pid = orphan.pid, "orphan didn't exit, sending SIGKILL");
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(orphan.pid as i32),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                }
            }
        }
    }

    // Reconcile persisted agent state — adopt running agents
    let agent_persistence = AgentPersistence::new();
    if let Ok(agent_state) = agent_persistence.load() {
        let reconciled = agent_persistence.reconcile(agent_state);
        for (name, entry) in &reconciled.agents {
            agent_manager
                .adopt(name, entry, config.agents.get(name))
                .await;
        }
        let _ = agent_persistence.save(&reconciled);

        // Restart adopted agents that need fresh connections to llama-server.
        // After a daemon restart, agents may hold stale CLOSE-WAIT sockets to
        // the old llama-server process and silently fail to send requests.
        //
        // We remove the stale adopted entry first, then start fresh. If the
        // agent uses --replace (like hermes), the new process handles killing
        // the old one via its own PID file. This avoids a race between our
        // stop() SIGTERM and --replace's SIGTERM hitting the same process.
        for name in reconciled.agents.keys() {
            if let Some(agent_config) = config.agents.get(name)
                && agent_config.restart_on_swap
            {
                tracing::info!(agent = %name, "bouncing adopted agent for fresh connection");
                // Remove stale tracking without sending SIGTERM — let --replace handle it
                agent_manager.remove_tracking(name).await;
                match agent_manager.start(name, agent_config).await {
                    Ok(info) => {
                        agent_manager
                            .record_restart(name, "daemon_restart", 0, 0)
                            .await;
                        tracing::info!(agent = %name, pid = info.pid, "agent restarted");
                    }
                    Err(e) => {
                        tracing::warn!(agent = %name, error = %e, "agent restart failed on daemon startup, retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        match agent_manager.start(name, agent_config).await {
                            Ok(info) => {
                                agent_manager
                                    .record_restart(name, "daemon_restart", 0, 0)
                                    .await;
                                tracing::info!(agent = %name, pid = info.pid, "agent restarted on retry");
                            }
                            Err(e) => {
                                tracing::error!(agent = %name, error = %e, "agent restart failed after retry")
                            }
                        }
                    }
                }
            }
        }
    }

    // Auto-start agents configured with auto_start = true
    for (name, agent_config) in &config.agents {
        if agent_config.auto_start && !agent_manager.is_running(name).await {
            tracing::info!(agent = %name, "auto-starting agent");
            match agent_manager.start(name, agent_config).await {
                Ok(info) => tracing::info!(agent = %name, pid = info.pid, "agent auto-started"),
                Err(e) => tracing::warn!(agent = %name, error = %e, "failed to auto-start agent"),
            }
        }
    }

    // Spawn agent watchdog for restart_on_crash support
    let watchdog_configs: std::collections::HashMap<String, _> = config
        .agents
        .iter()
        .filter(|(_, c)| c.restart_on_crash)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !watchdog_configs.is_empty() {
        tracing::info!(
            agents = ?watchdog_configs.keys().collect::<Vec<_>>(),
            "starting agent watchdog"
        );
        agent_manager.spawn_watchdog(config.agents.clone());
    }

    // Build hardware profile and HF client
    let hardware_profile = rookery_engine::hardware::build_hardware_profile(gpu_monitor.as_ref());
    tracing::info!(gpu = ?hardware_profile.gpu.as_ref().map(|g| &g.name), cpu = %hardware_profile.cpu.name, "hardware profile built");
    let hf_client = rookery_engine::models::HfClient::new();

    let state = Arc::new(AppState {
        config_path: rookery_core::config::Config::config_path(),
        config: Arc::new(RwLock::new(config)),
        backend,
        agent_manager,
        gpu_monitor,
        log_buffer,
        state_persistence,
        state_tx,
        op_lock: Mutex::new(()),
        hf_client,
        hardware_profile,
    });

    let shutdown_state = state.clone();

    // Spawn inference canary — periodic minimal completion request to detect
    // CUDA zombie state where /health responds but inference is broken.
    // Also triggers immediately on CUDA error patterns in llama-server stderr.
    let canary_state = state.clone();
    let canary_agent_mgr = state.agent_manager.clone();
    tokio::spawn(async move {
        const CANARY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

        loop {
            // Re-subscribe to the current backend's error channel each cycle.
            let mut cuda_error_rx = canary_state.backend.lock().await.subscribe_errors();

            // Wait for poll interval, CUDA error, or shutdown
            tokio::select! {
                _ = tokio::time::sleep(CANARY_INTERVAL) => {}
                _ = canary_agent_mgr.shutdown_notified() => {
                    tracing::info!("inference canary: shutdown, exiting");
                    return;
                }
                _ = cuda_error_rx.changed() => {
                    tracing::warn!("CUDA error detected, running immediate inference canary");
                }
            }

            if canary_agent_mgr.is_shutting_down() {
                return;
            }

            canary::run_canary_check(
                &canary_state.backend,
                &canary_state.config,
                &canary_state.state_persistence,
                &canary_state.op_lock,
            )
            .await;
        }
    });

    let app = Router::new()
        .route("/api/health", get(routes::get_health))
        .route("/api/status", get(routes::get_status))
        .route("/api/gpu", get(routes::get_gpu))
        .route("/api/logs", get(routes::get_logs))
        .route("/api/events", get(sse::get_events))
        .route("/api/start", post(routes::post_start))
        .route("/api/stop", post(routes::post_stop))
        .route("/api/swap", post(routes::post_swap))
        .route("/api/profiles", get(routes::get_profiles))
        .route("/api/bench", get(routes::get_bench))
        .route("/api/agents", get(routes::get_agents))
        .route("/api/agents/start", post(routes::post_agent_start))
        .route("/api/agents/stop", post(routes::post_agent_stop))
        .route("/api/agents/{name}/health", get(routes::get_agent_health))
        .route("/api/config", get(routes::get_config))
        .route("/api/config/profile/{name}", put(routes::put_profile))
        .route("/api/model-info", get(routes::get_model_info))
        .route("/api/server-stats", get(routes::get_server_stats))
        .route("/api/chat", post(routes::post_chat))
        .route("/api/hardware", get(routes::get_hardware))
        .route("/api/models/search", get(routes::get_models_search))
        .route("/api/models/quants", get(routes::get_models_quants))
        .route("/api/models/recommend", get(routes::get_models_recommend))
        .route("/api/models/cached", get(routes::get_models_cached))
        .route("/api/models/pull", post(routes::post_models_pull))
        .fallback(routes::get_dashboard)
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024)) // 1MB request body limit
        .with_state(state);

    tracing::info!(%listen, "rookeryd starting");
    tracing::info!(%listen, "dashboard at http://{listen}/");

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .expect("failed to bind");

    tracing::info!(%listen, "rookeryd listening");

    // Run axum in a spawned task so we can abort it on shutdown
    let server_handle = tokio::spawn(axum::serve(listener, app).into_future());

    // Wait for shutdown signal
    shutdown_signal().await;

    // Set shutdown flag so watchdog stops restarting agents
    shutdown_state.agent_manager.begin_shutdown();

    // Abort the axum server (don't wait for SSE streams to drain)
    server_handle.abort();

    // Clean up child processes
    tracing::info!("shutting down — stopping agents and server");
    shutdown_state.agent_manager.stop_all().await;
    let _ = shutdown_state.backend.lock().await.stop().await;
    let stopped = rookery_core::state::ServerState::Stopped;
    let _ = shutdown_state.state_persistence.save(&stopped);
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

mod app_state;
mod routes;
mod sse;

use app_state::AppState;
use axum::routing::{get, post, put};
use axum::Router;
use rookery_core::config::Config;
use rookery_core::state::{AgentPersistence, StatePersistence};
use rookery_engine::agent::AgentManager;
use rookery_engine::gpu::GpuMonitor;
use rookery_engine::logs::LogBuffer;
use rookery_engine::process::ProcessManager;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};

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

        // Adopt the running process — but verify it's actually healthy first
        if let rookery_core::state::ServerState::Running {
            ref profile,
            pid: running_pid,
            port,
            ref since,
            ref command_line,
            ref exe_path,
            ..
        } = reconciled
        {
            if rookery_engine::health::check_health(port, std::time::Duration::from_secs(3)).await {
                // Health endpoint responds — now verify inference actually works
                if rookery_engine::health::check_inference(port, std::time::Duration::from_secs(10)).await {
                    tracing::info!(pid = running_pid, port, "adopted process is healthy (inference canary passed)");
                    process_manager
                        .adopt(rookery_engine::process::ProcessInfo {
                            pid: running_pid,
                            port,
                            profile: profile.clone(),
                            started_at: *since,
                            command_line: command_line.clone(),
                            exe_path: exe_path.clone().unwrap_or_default(),
                        })
                        .await;
                } else {
                    tracing::warn!(pid = running_pid, port, "adopted process failed inference canary — marking stopped");
                    let stopped = rookery_core::state::ServerState::Stopped;
                    let _ = state_persistence.save(&stopped);
                }
            } else {
                tracing::warn!(pid = running_pid, port, "adopted process failed health check — marking stopped");
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
            agent_manager.adopt(name, entry, config.agents.get(name)).await;
        }
        let _ = agent_persistence.save(&reconciled);

        // Restart adopted agents that need fresh connections to llama-server.
        // After a daemon restart, agents may hold stale CLOSE-WAIT sockets to
        // the old llama-server process and silently fail to send requests.
        for (name, _entry) in &reconciled.agents {
            if let Some(agent_config) = config.agents.get(name) {
                if agent_config.restart_on_swap {
                    tracing::info!(agent = %name, "bouncing adopted agent for fresh connection");
                    let _ = agent_manager.stop(name).await;
                    match agent_manager.start(name, agent_config).await {
                        Ok(info) => tracing::info!(agent = %name, pid = info.pid, "agent restarted"),
                        Err(e) => tracing::warn!(agent = %name, error = %e, "failed to restart adopted agent"),
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
    let hardware_profile =
        rookery_engine::hardware::build_hardware_profile(gpu_monitor.as_ref());
    tracing::info!(gpu = ?hardware_profile.gpu.as_ref().map(|g| &g.name), cpu = %hardware_profile.cpu.name, "hardware profile built");
    let hf_client = rookery_engine::models::HfClient::new();

    let state = Arc::new(AppState {
        config: Arc::new(RwLock::new(config)),
        process_manager,
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
    let mut cuda_error_rx = state.process_manager.subscribe_cuda_errors();
    tokio::spawn(async move {
        const CANARY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
        const CANARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

        loop {
            // Wait for either the regular interval or a CUDA error trigger
            tokio::select! {
                _ = tokio::time::sleep(CANARY_INTERVAL) => {}
                _ = cuda_error_rx.changed() => {
                    tracing::warn!("CUDA error detected, running immediate inference canary");
                }
            }

            // Only check when server is running and not mid-swap
            if canary_state.process_manager.is_draining() {
                continue;
            }
            let current = canary_state.process_manager.to_server_state().await;
            let (profile, port) = match current {
                rookery_core::state::ServerState::Running { ref profile, port, .. } => {
                    (profile.clone(), port)
                }
                _ => continue,
            };

            if rookery_engine::health::check_inference(port, CANARY_TIMEOUT).await {
                tracing::debug!(port, "inference canary passed");
                continue;
            }

            // First failure — retry once after 5s to avoid false positives
            tracing::warn!(port, "inference canary failed, retrying in 5s");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            if rookery_engine::health::check_inference(port, CANARY_TIMEOUT).await {
                tracing::info!(port, "inference canary passed on retry");
                continue;
            }

            // Two consecutive failures — server is broken, restart it
            tracing::error!(port, profile = %profile, "inference canary failed twice, restarting server");

            // Acquire op_lock to serialize with manual start/stop/swap
            let _op_guard = canary_state.op_lock.lock().await;

            // Re-check state under lock — someone may have stopped/swapped already
            let current = canary_state.process_manager.to_server_state().await;
            if !current.is_running() {
                tracing::info!("server already stopped, skipping canary restart");
                continue;
            }

            let _ = canary_state.process_manager.stop().await;
            let stopped = rookery_core::state::ServerState::Stopped;
            let _ = canary_state.state_persistence.save(&stopped);

            let config = canary_state.config.read().await;
            match canary_state.process_manager.start_and_wait(&config, &profile).await {
                Ok(server_state) => {
                    let _ = canary_state.state_persistence.save(&server_state);
                    if server_state.is_running() {
                        tracing::info!(profile = %profile, "server restarted by inference canary");
                    } else {
                        tracing::error!(profile = %profile, "server failed to restart after canary");
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, profile = %profile, "canary restart failed");
                }
            }
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
        .with_state(state);

    tracing::info!(%listen, "rookeryd starting");
    tracing::info!(%listen, "dashboard at http://{listen}/");

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .expect("failed to bind");

    tracing::info!(%listen, "rookeryd listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    // Clean up child processes on shutdown
    tracing::info!("shutting down — stopping agents and server");
    shutdown_state.agent_manager.stop_all().await;
    let _ = shutdown_state.process_manager.stop().await;
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

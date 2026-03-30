mod app_state;
mod auth;
pub mod canary;
mod metrics;
mod routes;
mod sse;
#[cfg(test)]
pub mod test_utils;

use app_state::AppState;
use axum::Router;
use axum::middleware;
use axum::routing::{get, post, put};
use rookery_core::config::Config;
use rookery_core::state::{AgentPersistence, ServerState, StatePersistence};
use rookery_engine::agent::AgentManager;
use rookery_engine::backend::{self, BackendInfo, InferenceBackend};
use rookery_engine::gpu::GpuMonitor;
use rookery_engine::logs::LogBuffer;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use tokio::sync::{Mutex, RwLock, broadcast};

async fn reconciled_backend_alive(
    backend_type: rookery_core::config::BackendType,
    running_pid: u32,
    backend: &dyn InferenceBackend,
) -> bool {
    match backend_type {
        rookery_core::config::BackendType::LlamaServer => {
            rookery_engine::process::is_pid_alive(running_pid)
        }
        rookery_core::config::BackendType::Vllm => backend.is_running().await,
    }
}

#[tokio::main]
async fn main() {
    // Handle --version / -V before initializing anything
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("rookeryd {}", env!("CARGO_PKG_VERSION"));
        return;
    }

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
    let initial_server_state = if let Ok(prev_state) = state_persistence.load() {
        let reconciled = state_persistence.reconcile(prev_state);
        let mut final_state = reconciled.clone();
        tracing::info!(state = ?format!("{:?}", reconciled), "reconciled previous state");

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
            if let Some(profile_cfg) = config.profiles.get(profile) {
                match backend::create_backend(profile_cfg, log_buffer.clone()) {
                    Ok(correct_backend) => {
                        if !reconciled_backend_alive(
                            *backend_type,
                            running_pid,
                            correct_backend.as_ref(),
                        )
                        .await
                        {
                            tracing::warn!(
                                backend_type = ?backend_type,
                                pid = running_pid,
                                container_id = ?container_id,
                                "adopted backend is no longer running, marking stopped"
                            );
                            final_state = ServerState::Stopped;
                        } else if rookery_engine::health::check_health(
                            port,
                            std::time::Duration::from_secs(3),
                        )
                        .await
                        {
                            // Health endpoint responds — now verify inference actually works
                            if rookery_engine::health::check_inference(
                                port,
                                std::time::Duration::from_secs(10),
                            )
                            .await
                            {
                                tracing::info!(
                                    backend_type = ?backend_type,
                                    pid = running_pid,
                                    container_id = ?container_id,
                                    port,
                                    "adopted backend is healthy (inference canary passed)"
                                );

                                let adopt_info = BackendInfo {
                                    pid: match backend_type {
                                        rookery_core::config::BackendType::LlamaServer => {
                                            Some(running_pid)
                                        }
                                        rookery_core::config::BackendType::Vllm => None,
                                    },
                                    container_id: container_id.clone(),
                                    port,
                                    profile: profile.clone(),
                                    started_at: *since,
                                    backend_type: *backend_type,
                                    command_line: command_line.clone(),
                                    exe_path: exe_path.clone(),
                                };

                                match correct_backend.adopt(adopt_info).await {
                                    Ok(()) => {
                                        *backend.lock().await = correct_backend;
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, "failed to adopt into backend");
                                        final_state = ServerState::Stopped;
                                    }
                                }
                            } else {
                                tracing::warn!(
                                    backend_type = ?backend_type,
                                    pid = running_pid,
                                    container_id = ?container_id,
                                    port,
                                    "adopted backend failed inference canary — marking stopped"
                                );
                                final_state = ServerState::Stopped;
                            }
                        } else {
                            tracing::warn!(
                                backend_type = ?backend_type,
                                pid = running_pid,
                                container_id = ?container_id,
                                port,
                                "adopted backend failed health check — marking stopped"
                            );
                            final_state = ServerState::Stopped;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, profile = %profile, "failed to create backend for reconciled profile");
                        final_state = ServerState::Stopped;
                    }
                }
            } else {
                tracing::warn!(profile = %profile, "reconciled profile missing from config, marking stopped");
                final_state = ServerState::Stopped;
            }
        }

        let _ = state_persistence.save(&final_state);
        final_state
    } else {
        ServerState::Stopped
    };
    let tracked_pid = initial_server_state.pid();

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

    // Spawn agent watchdog for restart_on_crash support
    // Note: agent auto-start is deferred until after profile auto-start (below)
    // so agents that depend on the inference port can connect immediately.
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
        if initial_server_state.is_sleeping() {
            agent_manager.set_dependency_bounce_suppressed(true);
        }
        agent_manager.spawn_watchdog(config.agents.clone());
    }

    // Build hardware profile and HF client
    let hardware_profile = rookery_engine::hardware::build_hardware_profile(gpu_monitor.as_ref());
    tracing::info!(gpu = ?hardware_profile.gpu.as_ref().map(|g| &g.name), cpu = %hardware_profile.cpu.name, "hardware profile built");
    let hf_client = rookery_engine::models::HfClient::new();
    let metrics = Arc::new(metrics::RuntimeMetrics::new());
    let should_auto_start = config.auto_start;
    let auto_start_profile = config.resolve_profile_name(None).to_string();

    let state = Arc::new(AppState {
        config_path: rookery_core::config::Config::config_path(),
        config: Arc::new(RwLock::new(config)),
        backend,
        agent_manager,
        metrics,
        gpu_monitor,
        log_buffer,
        state_persistence,
        server_state: RwLock::new(initial_server_state.clone()),
        state_tx,
        last_inference_at: AtomicI64::new(if initial_server_state.is_running() {
            chrono::Utc::now().timestamp()
        } else {
            0
        }),
        op_lock: Mutex::new(()),
        hf_client,
        hardware_profile,
    });

    // Auto-start default profile if configured and no server is running
    if should_auto_start
        && !initial_server_state.is_running()
        && !initial_server_state.is_sleeping()
    {
        tracing::info!(profile = %auto_start_profile, "auto-starting default profile");
        match state.start_profile(&auto_start_profile, true).await {
            Ok(_) => tracing::info!(profile = %auto_start_profile, "default profile auto-started"),
            Err(e) => {
                tracing::warn!(profile = %auto_start_profile, error = %e, "failed to auto-start default profile")
            }
        }
    }

    // Auto-start agents configured with auto_start = true
    // This happens AFTER profile auto-start so agents that depend_on_port can connect.
    {
        let config = state.config.read().await;
        for (name, agent_config) in &config.agents {
            if agent_config.auto_start && !state.agent_manager.is_running(name).await {
                tracing::info!(agent = %name, "auto-starting agent");
                match state.agent_manager.start(name, agent_config).await {
                    Ok(info) => tracing::info!(agent = %name, pid = info.pid, "agent auto-started"),
                    Err(e) => {
                        tracing::warn!(agent = %name, error = %e, "failed to auto-start agent")
                    }
                }
            }
        }
    }

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

            canary::run_canary_check(&canary_state, Some(canary_agent_mgr.shutdown_flag())).await;
        }
    });

    // Spawn idle auto-sleep watcher.
    let idle_state = state.clone();
    tokio::spawn(async move {
        const IDLE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

        loop {
            tokio::select! {
                _ = tokio::time::sleep(IDLE_POLL_INTERVAL) => {}
                _ = idle_state.agent_manager.shutdown_notified() => {
                    tracing::info!("idle sleep watcher: shutdown, exiting");
                    return;
                }
            }

            if idle_state.agent_manager.is_shutting_down() {
                return;
            }

            let idle_timeout = idle_state.config.read().await.idle_timeout.unwrap_or(0);
            if idle_timeout == 0 {
                continue;
            }

            if !idle_state.current_state().await.is_running() {
                continue;
            }

            let idle_for_secs = chrono::Utc::now().timestamp() - idle_state.last_inference_at();
            if idle_for_secs < idle_timeout as i64 {
                continue;
            }

            let _op_guard = idle_state.op_lock.lock().await;
            if idle_state.agent_manager.is_shutting_down() {
                return;
            }
            if !idle_state.current_state().await.is_running() {
                continue;
            }
            let idle_for_secs = chrono::Utc::now().timestamp() - idle_state.last_inference_at();
            if idle_for_secs < idle_timeout as i64 {
                continue;
            }

            if let Some(profile) = idle_state.current_state().await.profile_name() {
                tracing::info!(
                    profile,
                    idle_timeout_secs = idle_timeout,
                    idle_for_secs,
                    "idle timeout reached, putting server to sleep"
                );
            }

            if let Err(e) = idle_state.sleep_server().await {
                tracing::warn!(error = %e, "failed to put server to sleep");
            }
        }
    });

    let auth_layer = middleware::from_fn_with_state(state.clone(), auth::require_api_key);

    let protected_api = Router::new()
        .route("/status", get(routes::get_status))
        .route("/gpu", get(routes::get_gpu))
        .route("/logs", get(routes::get_logs))
        .route("/events", get(sse::get_events))
        .route("/start", post(routes::post_start))
        .route("/stop", post(routes::post_stop))
        .route("/sleep", post(routes::post_sleep))
        .route("/wake", post(routes::post_wake))
        .route("/swap", post(routes::post_swap))
        .route("/profiles", get(routes::get_profiles))
        .route("/bench", get(routes::get_bench))
        .route("/agents", get(routes::get_agents))
        .route("/agents/start", post(routes::post_agent_start))
        .route("/agents/stop", post(routes::post_agent_stop))
        .route("/agents/{name}/update", post(routes::post_agent_update))
        .route("/agents/{name}/health", get(routes::get_agent_health))
        .route("/config", get(routes::get_config))
        .route("/config/profile/{name}", put(routes::put_profile))
        .route("/model-info", get(routes::get_model_info))
        .route("/server-stats", get(routes::get_server_stats))
        .route("/chat", post(routes::post_chat))
        .route("/hardware", get(routes::get_hardware))
        .route("/models/search", get(routes::get_models_search))
        .route("/models/quants", get(routes::get_models_quants))
        .route("/models/recommend", get(routes::get_models_recommend))
        .route("/models/cached", get(routes::get_models_cached))
        .route("/models/pull", post(routes::post_models_pull))
        .route_layer(auth_layer);

    let app = Router::new()
        .route("/api/health", get(routes::get_health))
        .nest("/api", protected_api)
        .route("/metrics", get(routes::get_metrics))
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
    shutdown_state
        .agent_manager
        .set_dependency_bounce_suppressed(false);
    let _ = shutdown_state.backend.lock().await.stop().await;
    shutdown_state.set_server_state(ServerState::Stopped).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rookery_core::config::BackendType;
    use rookery_core::error::{Error, Result};
    use tokio::sync::watch;

    struct MockBackend {
        running: bool,
    }

    #[async_trait]
    impl InferenceBackend for MockBackend {
        async fn start(&self, _config: &Config, _profile: &str) -> Result<BackendInfo> {
            Err(Error::ConfigValidation("not used in test".into()))
        }

        async fn stop(&self) -> Result<()> {
            Ok(())
        }

        async fn is_running(&self) -> bool {
            self.running
        }

        async fn process_info(&self) -> Option<BackendInfo> {
            None
        }

        async fn adopt(&self, _info: BackendInfo) -> Result<()> {
            Ok(())
        }

        async fn to_server_state(&self) -> ServerState {
            ServerState::Stopped
        }

        fn is_draining(&self) -> bool {
            false
        }

        fn set_draining(&self, _draining: bool) {}

        fn subscribe_errors(&self) -> watch::Receiver<bool> {
            let (_tx, rx) = watch::channel(false);
            rx
        }
    }

    #[tokio::test]
    async fn test_reconciled_backend_alive_uses_pid_check_for_llama_server() {
        let backend = MockBackend { running: false };
        let alive =
            reconciled_backend_alive(BackendType::LlamaServer, std::process::id(), &backend).await;
        assert!(alive, "llama-server reconciliation should use PID liveness");
    }

    #[tokio::test]
    async fn test_reconciled_backend_alive_uses_backend_check_for_vllm_true() {
        let backend = MockBackend { running: true };
        let alive = reconciled_backend_alive(BackendType::Vllm, 0, &backend).await;
        assert!(alive, "vLLM reconciliation should use backend.is_running()");
    }

    #[tokio::test]
    async fn test_reconciled_backend_alive_uses_backend_check_for_vllm_false() {
        let backend = MockBackend { running: false };
        let alive = reconciled_backend_alive(BackendType::Vllm, std::process::id(), &backend).await;
        assert!(
            !alive,
            "vLLM reconciliation must ignore PID liveness and use backend.is_running()"
        );
    }
}

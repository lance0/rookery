//! Inference canary — periodic minimal completion request to detect
//! CUDA zombie state where /health responds but inference is broken.
//!
//! The canary logic was extracted from the `tokio::spawn` block in `main()`
//! so it can be unit-tested without starting the full daemon.

use std::sync::Arc;
use std::time::Duration;

use crate::app_state::AppState;

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
    state: &Arc<AppState>,
    shutdown: Option<&std::sync::atomic::AtomicBool>,
) -> bool {
    let is_shutdown = || {
        shutdown
            .map(|s| s.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(false)
    };
    state.metrics.record_canary_check();

    // Only check when server is running and not mid-swap
    if state.backend.lock().await.is_draining() || is_shutdown() {
        return false;
    }
    let current = state.current_state().await;
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
    if is_shutdown() {
        return false;
    }
    state.metrics.inc_canary_failure();
    tracing::warn!(port, "inference canary failed, retrying in 5s");
    tokio::time::sleep(CANARY_RETRY_DELAY).await;
    if is_shutdown() {
        return false;
    }

    if rookery_engine::health::check_inference(port, CANARY_TIMEOUT).await {
        tracing::info!(port, "inference canary passed on retry");
        return false;
    }

    // Two consecutive failures — server is broken, restart it
    if is_shutdown() {
        return false;
    }
    state.metrics.inc_canary_restart();
    tracing::error!(port, profile = %profile, "inference canary failed twice, restarting server");

    // Acquire op_lock to serialize with manual start/stop/swap
    let _op_guard = state.op_lock.lock().await;
    if is_shutdown() {
        return false;
    }

    // Re-check state under lock — someone may have stopped/swapped already
    let current = state.current_state().await;
    if !current.is_running() {
        tracing::info!("server already stopped, skipping canary restart");
        return false;
    }

    let _ = state.backend.lock().await.stop().await;
    let stopped = rookery_core::state::ServerState::Stopped;
    state.set_server_state(stopped).await;

    if is_shutdown() {
        tracing::info!("canary: shutdown during restart, aborting");
        return true;
    }

    match state.start_profile(&profile, false).await {
        Ok(server_state) => {
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

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::routing::{get, post};
    use rookery_core::config::{BackendType, Config, Model, Profile};
    use rookery_core::error::{Error, Result};
    use rookery_core::state::ServerState;
    use rookery_engine::backend::{BackendInfo, InferenceBackend};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use tokio::sync::watch;

    use crate::test_utils::build_test_app_state;

    // ── Minimal mock HTTP server for canary tests ─────────────────────

    /// A lightweight mock server that serves /health and /v1/chat/completions.
    /// Used to make `check_inference()` and `wait_for_health()` succeed or fail.
    struct MockHttpServer {
        port: u16,
        shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
        handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl MockHttpServer {
        /// Start a mock server that responds 200 to health and completions.
        async fn healthy() -> Self {
            let app = axum::Router::new()
                .route("/health", get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }))
                .route(
                    "/v1/chat/completions",
                    post(|| async {
                        axum::Json(serde_json::json!({
                            "id": "mock",
                            "object": "chat.completion",
                            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        }))
                    }),
                );

            Self::start_with_router(app).await
        }

        async fn start_with_router(app: axum::Router) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("failed to bind mock server");
            let port = listener.local_addr().unwrap().port();

            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let handle = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("mock server failed");
            });

            Self {
                port,
                shutdown_tx: Some(shutdown_tx),
                handle: Some(handle),
            }
        }

        fn port(&self) -> u16 {
            self.port
        }

        async fn shutdown(mut self) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.await;
            }
        }
    }

    impl Drop for MockHttpServer {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.handle.take() {
                handle.abort();
            }
        }
    }

    // ── Mock InferenceBackend for canary tests ────────────────────────

    /// A mock backend specifically designed for canary tests.
    ///
    /// Reports configurable running/draining state and tracks stop/start calls.
    /// The port field controls which HTTP endpoint `check_inference` connects to.
    struct CanaryMockBackend {
        running: AtomicBool,
        draining: AtomicBool,
        port: tokio::sync::Mutex<u16>,
        profile: tokio::sync::Mutex<String>,
        start_succeeds: AtomicBool,
        /// Port to use after start() — should point to a healthy mock server.
        start_port: tokio::sync::Mutex<u16>,
        stop_count: AtomicU32,
        start_count: AtomicU32,
        cuda_error_tx: watch::Sender<bool>,
    }

    impl CanaryMockBackend {
        fn new(port: u16) -> Self {
            let (cuda_error_tx, _) = watch::channel(false);
            Self {
                running: AtomicBool::new(true),
                draining: AtomicBool::new(false),
                port: tokio::sync::Mutex::new(port),
                profile: tokio::sync::Mutex::new("test".into()),
                start_succeeds: AtomicBool::new(true),
                start_port: tokio::sync::Mutex::new(port),
                stop_count: AtomicU32::new(0),
                start_count: AtomicU32::new(0),
                cuda_error_tx,
            }
        }

        fn set_running(&self, running: bool) {
            self.running.store(running, Ordering::SeqCst);
        }

        fn set_start_succeeds(&self, succeeds: bool) {
            self.start_succeeds.store(succeeds, Ordering::SeqCst);
        }

        async fn set_start_port(&self, port: u16) {
            *self.start_port.lock().await = port;
        }

        fn trigger_cuda_error(&self) {
            let _ = self.cuda_error_tx.send(true);
        }
    }

    #[async_trait]
    impl InferenceBackend for CanaryMockBackend {
        async fn start(&self, _config: &Config, profile: &str) -> Result<BackendInfo> {
            self.start_count.fetch_add(1, Ordering::SeqCst);
            if !self.start_succeeds.load(Ordering::SeqCst) {
                return Err(Error::ConfigValidation("mock start failure".into()));
            }
            let new_port = *self.start_port.lock().await;
            *self.port.lock().await = new_port;
            *self.profile.lock().await = profile.to_string();
            self.running.store(true, Ordering::SeqCst);
            Ok(BackendInfo {
                pid: Some(99999),
                container_id: None,
                port: new_port,
                profile: profile.to_string(),
                started_at: chrono::Utc::now(),
                backend_type: BackendType::LlamaServer,
                command_line: vec!["mock".into()],
                exe_path: Some(PathBuf::from("/mock")),
            })
        }

        async fn stop(&self) -> Result<()> {
            self.stop_count.fetch_add(1, Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }

        async fn is_running(&self) -> bool {
            self.running.load(Ordering::SeqCst)
        }

        async fn process_info(&self) -> Option<BackendInfo> {
            if self.running.load(Ordering::SeqCst) {
                let port = *self.port.lock().await;
                let profile = self.profile.lock().await.clone();
                Some(BackendInfo {
                    pid: Some(99999),
                    container_id: None,
                    port,
                    profile,
                    started_at: chrono::Utc::now(),
                    backend_type: BackendType::LlamaServer,
                    command_line: vec![],
                    exe_path: None,
                })
            } else {
                None
            }
        }

        async fn adopt(&self, _info: BackendInfo) -> Result<()> {
            Ok(())
        }

        async fn to_server_state(&self) -> ServerState {
            if self.running.load(Ordering::SeqCst) {
                let port = *self.port.lock().await;
                let profile = self.profile.lock().await.clone();
                ServerState::Running {
                    profile,
                    pid: 99999,
                    port,
                    since: chrono::Utc::now(),
                    command_line: vec![],
                    exe_path: None,
                    backend_type: BackendType::LlamaServer,
                    container_id: None,
                }
            } else {
                ServerState::Stopped
            }
        }

        fn is_draining(&self) -> bool {
            self.draining.load(Ordering::SeqCst)
        }

        fn set_draining(&self, draining: bool) {
            self.draining.store(draining, Ordering::SeqCst);
        }

        fn subscribe_errors(&self) -> watch::Receiver<bool> {
            self.cuda_error_tx.subscribe()
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────

    fn test_config(port: u16) -> Config {
        Config {
            llama_server: PathBuf::from("/mock/llama-server"),
            default_profile: "test".into(),
            listen: "127.0.0.1:19876".parse().unwrap(),
            api_key: None,
            idle_timeout: None,
            models: HashMap::from([(
                "test_model".into(),
                Model {
                    source: "local".into(),
                    repo: None,
                    file: None,
                    path: Some(PathBuf::from("/tmp/fake.gguf")),
                    estimated_vram_mb: None,
                },
            )]),
            profiles: HashMap::from([(
                "test".into(),
                Profile {
                    model: "test_model".into(),
                    port,
                    llama_server: None,
                    vllm: None,
                    ctx_size: 1024,
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
            )]),
            auto_start: false,
            model_dirs: vec![],
            agents: HashMap::new(),
        }
    }

    /// Get an OS-assigned port that has no listener (connection refused).
    async fn dead_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    async fn build_canary_state(
        backend: Box<dyn InferenceBackend>,
        config: Config,
    ) -> (tempfile::TempDir, Arc<AppState>) {
        let (dir, state) = build_test_app_state(Some(backend));
        *state.config.write().await = config;
        let current = state.backend.lock().await.to_server_state().await;
        state.set_server_state(current).await;
        (dir, state)
    }

    // ── Tests ─────────────────────────────────────────────────────────

    // === Test 1: Canary check succeeds when backend is healthy — no restart
    #[tokio::test]
    async fn test_canary_healthy_backend_no_restart() {
        let server = MockHttpServer::healthy().await;
        let port = server.port();

        let (_dir, state) =
            build_canary_state(Box::new(CanaryMockBackend::new(port)), test_config(port)).await;

        let restarted = run_canary_check(&state, None).await;

        assert!(!restarted, "healthy backend should not trigger restart");
        assert!(state.current_state().await.is_running());

        server.shutdown().await;
    }

    // === Test 2: Canary triggers restart after inference check fails twice
    #[tokio::test]
    async fn test_canary_restart_after_two_inference_failures() {
        let dead = dead_port().await;

        // Healthy server for post-restart health check
        let healthy_server = MockHttpServer::healthy().await;
        let healthy_port = healthy_server.port();

        let mock_backend = CanaryMockBackend::new(dead);
        mock_backend.set_start_port(healthy_port).await;
        let (_dir, state) =
            build_canary_state(Box::new(mock_backend), test_config(healthy_port)).await;

        let restarted = run_canary_check(&state, None).await;

        assert!(restarted, "should restart after two inference failures");
        assert!(
            state.current_state().await.is_running(),
            "backend should be running after successful restart"
        );

        healthy_server.shutdown().await;
    }

    // === Test 3: Canary skips check when backend is draining

    #[tokio::test]
    async fn test_canary_skips_when_draining() {
        let mock_backend = CanaryMockBackend::new(1);
        mock_backend.draining.store(true, Ordering::SeqCst);

        let (_dir, state) = build_canary_state(Box::new(mock_backend), test_config(1)).await;

        let restarted = run_canary_check(&state, None).await;

        assert!(!restarted, "should skip check when draining");
    }

    // === Test 4: Canary skips check when backend is not running

    #[tokio::test]
    async fn test_canary_skips_when_not_running() {
        let mock_backend = CanaryMockBackend::new(1);
        mock_backend.set_running(false);

        let (_dir, state) = build_canary_state(Box::new(mock_backend), test_config(1)).await;

        let restarted = run_canary_check(&state, None).await;

        assert!(!restarted, "should skip check when not running");
    }

    // === Test 5: CUDA error on watch channel triggers immediate canary check

    //
    // The canary loop in main() uses `cuda_error_rx.changed()` in a
    // `tokio::select!` to break out of the sleep interval. This test
    // verifies the watch channel fires when a CUDA error is sent.
    #[tokio::test]
    async fn test_cuda_error_triggers_canary_via_watch_channel() {
        let mock_backend = CanaryMockBackend::new(1);

        let mut rx = mock_backend.subscribe_errors();
        assert!(!*rx.borrow(), "initial state should be false");

        mock_backend.trigger_cuda_error();

        let changed = tokio::time::timeout(Duration::from_secs(1), rx.changed()).await;
        assert!(changed.is_ok(), "watch channel should notify on CUDA error");
        assert!(*rx.borrow(), "CUDA error flag should be true after trigger");
    }

    // === Test 6: Canary acquires op_lock during restart
    // Verifies the canary serializes restart with manual start/stop/swap.
    //
    // The canary does two inference checks with a CANARY_RETRY_DELAY (5s) between
    // them before acquiring the lock. We use `start_paused = true` so the retry
    // delay auto-advances, but the lock is held throughout, proving the canary
    // blocks on the lock (not on the retry delay).
    #[tokio::test(start_paused = true)]
    async fn test_canary_acquires_op_lock_during_restart() {
        // Resume time briefly to bind real ports, then pause again
        tokio::time::resume();
        let dead = dead_port().await;
        let healthy_server = MockHttpServer::healthy().await;
        let healthy_port = healthy_server.port();
        tokio::time::pause();

        let mock_backend = CanaryMockBackend::new(dead);
        mock_backend.set_start_port(healthy_port).await;
        let (_dir, state) =
            build_canary_state(Box::new(mock_backend), test_config(healthy_port)).await;

        // Hold the op_lock — canary should block until we release it
        let guard = state.op_lock.lock().await;

        // Spawn canary — it will block on op_lock after two failed inference checks.
        // With paused time, the CANARY_RETRY_DELAY auto-advances instantly, so the
        // canary quickly reaches the lock acquisition point.
        let state_clone = state.clone();
        let canary_handle = tokio::spawn(async move { run_canary_check(&state_clone, None).await });

        // Yield to let the canary task run through inference checks and reach the lock.
        // With paused time, all sleeps complete instantly, but the canary will block
        // on op_lock.lock().await since we hold the guard.
        tokio::task::yield_now().await;
        // Advance time to let any internal timeouts/sleeps resolve
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;

        assert!(
            !canary_handle.is_finished(),
            "canary should be blocked waiting for op_lock"
        );

        // Release the lock — canary can now proceed with restart
        drop(guard);

        // Resume real time for the health check HTTP request after restart
        tokio::time::resume();

        let result = tokio::time::timeout(Duration::from_secs(30), canary_handle).await;
        assert!(result.is_ok(), "canary should complete after lock released");
        assert!(result.unwrap().unwrap(), "canary should have restarted");

        healthy_server.shutdown().await;
    }

    // === Test 7: Restart transitions: Running → stop → start → Running
    #[tokio::test]
    async fn test_canary_restart_transitions_running_stop_start_running() {
        let dead = dead_port().await;

        let healthy_server = MockHttpServer::healthy().await;
        let healthy_port = healthy_server.port();

        let mock_backend = CanaryMockBackend::new(dead);
        mock_backend.set_start_port(healthy_port).await;
        let (_dir, state) =
            build_canary_state(Box::new(mock_backend), test_config(healthy_port)).await;

        // Initial state: Running
        assert!(state.current_state().await.is_running());

        let restarted = run_canary_check(&state, None).await;
        assert!(restarted, "should have restarted");

        // Final state: Running again
        let final_state = state.current_state().await;
        assert!(final_state.is_running(), "should be Running after restart");

        // Persisted state should also be Running
        let persisted = state.state_persistence.load().unwrap();
        assert!(persisted.is_running(), "persisted state should be Running");

        healthy_server.shutdown().await;
    }

    // === Test 8: Restart failure transitions to Failed state
    // Uses tokio::time::pause to make the 120s health timeout instant.
    #[tokio::test(start_paused = true)]
    async fn test_canary_restart_failure_transitions_to_failed() {
        // Resume time briefly to bind a real port, then pause again
        tokio::time::resume();
        let dead = dead_port().await;
        tokio::time::pause();

        // After start(), port still points to dead port → wait_for_health fails
        let mock_backend = CanaryMockBackend::new(dead);
        mock_backend.set_start_port(dead).await;
        let (_dir, state) = build_canary_state(Box::new(mock_backend), test_config(dead)).await;

        let restarted = run_canary_check(&state, None).await;
        assert!(restarted, "should have attempted restart");

        let persisted = state.state_persistence.load().unwrap();
        assert!(
            matches!(persisted, ServerState::Failed { .. }),
            "persisted state should be Failed, got {persisted:?}"
        );
    }

    // === Test 9: Canary skips restart if server stopped during lock wait
    #[tokio::test]
    async fn test_canary_skips_restart_if_stopped_during_lock_wait() {
        let dead = dead_port().await;

        let (_dir, state) =
            build_canary_state(Box::new(CanaryMockBackend::new(dead)), test_config(dead)).await;

        // Hold the lock
        let guard = state.op_lock.lock().await;

        let state_clone = state.clone();
        let canary_handle = tokio::spawn(async move { run_canary_check(&state_clone, None).await });

        // Wait for canary to reach the lock
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Stop the backend while holding the lock (simulating manual stop)
        state.backend.lock().await.stop().await.unwrap();
        state.set_server_state(ServerState::Stopped).await;

        // Release the lock — canary re-checks state and finds it stopped
        drop(guard);

        let result = tokio::time::timeout(Duration::from_secs(5), canary_handle).await;
        assert!(result.is_ok(), "canary should complete");
        assert!(
            !result.unwrap().unwrap(),
            "should NOT restart — server was stopped during lock wait"
        );
    }

    // === Test 10: start() failure still returns true (restart attempted)
    #[tokio::test]
    async fn test_canary_start_failure_returns_true() {
        let dead = dead_port().await;

        let mock_backend = CanaryMockBackend::new(dead);
        mock_backend.set_start_succeeds(false);
        let (_dir, state) = build_canary_state(Box::new(mock_backend), test_config(dead)).await;

        let restarted = run_canary_check(&state, None).await;
        assert!(restarted, "should return true even when start() fails");

        // Backend should be stopped (stop was called before start attempt)
        assert!(
            !state.current_state().await.is_running(),
            "backend should be stopped after failed restart"
        );
    }

    // === Test 11: Canary operates through trait interface
    // All tests above use CanaryMockBackend via Box<dyn InferenceBackend>,
    // proving the canary works through the trait. This test makes it explicit.
    #[tokio::test]
    async fn test_canary_operates_through_trait_interface() {
        let server = MockHttpServer::healthy().await;
        let port = server.port();

        let (_dir, state) =
            build_canary_state(Box::new(CanaryMockBackend::new(port)), test_config(port)).await;

        // run_canary_check operates through AppState's boxed InferenceBackend.
        let restarted = run_canary_check(&state, None).await;
        assert!(!restarted, "healthy backend via trait should not restart");

        server.shutdown().await;
    }
}

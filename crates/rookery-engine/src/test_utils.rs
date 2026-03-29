//! Shared test utilities for rookery-engine tests.
//!
//! Provides a `MockLlamaServer` that emulates llama-server HTTP endpoints
//! and is used across health, process, backend, and daemon route tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Configuration for mock server behavior.
#[derive(Clone)]
struct MockConfig {
    /// If set, the /health endpoint waits this long before responding.
    health_delay: Option<Duration>,
    /// If set, /health returns 500 after this many successful requests.
    health_fail_after: Option<u64>,
    /// If set, the first N /health requests return 500, then subsequent ones return 200.
    health_fail_first: Option<u64>,
    /// If true, POST /v1/chat/completions returns 500 instead of 200.
    completions_fail: bool,
}

/// Shared state for the mock server.
#[derive(Clone)]
struct MockState {
    config: MockConfig,
    health_request_count: Arc<AtomicU64>,
}

/// A lightweight mock llama-server for testing.
///
/// Binds to an OS-assigned port (port 0) and serves endpoints that mimic
/// llama-server's HTTP API. Supports configurable delays and failure modes
/// for testing health checks, timeouts, and watchdog behavior.
///
/// # Endpoints
///
/// - `GET /health` → `200 {"status":"ok"}` (configurable delay/failure)
/// - `GET /v1/models` → `200 {"data":[{"id":"mock-model","owned_by":"test"}]}`
/// - `GET /props` → `200 {"total_slots":1,"chat_template":"test"}`
/// - `GET /slots` → `200 [{"id":0,"state":0,"prompt":"","next_token":{}}]`
/// - `POST /v1/chat/completions` → `200` with minimal completion response + timings
pub struct MockLlamaServer {
    pub port: u16,
    shutdown_tx: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

/// Builder for configuring a `MockLlamaServer` before starting it.
pub struct MockLlamaServerBuilder {
    health_delay: Option<Duration>,
    health_fail_after: Option<u64>,
    health_fail_first: Option<u64>,
    completions_fail: bool,
}

impl MockLlamaServerBuilder {
    /// Set a delay before the /health endpoint responds.
    /// Useful for testing health check timeouts.
    pub fn health_delay(mut self, delay: Duration) -> Self {
        self.health_delay = Some(delay);
        self
    }

    /// Make the /health endpoint return 500 after `n` successful requests.
    /// Useful for testing watchdog/canary failure detection.
    pub fn health_fail_after(mut self, n: u64) -> Self {
        self.health_fail_after = Some(n);
        self
    }

    /// Make the first `n` /health requests return 500, then subsequent ones return 200.
    /// Useful for testing retry-then-succeed patterns.
    pub fn health_fail_first(mut self, n: u64) -> Self {
        self.health_fail_first = Some(n);
        self
    }

    /// Make POST /v1/chat/completions return 500 instead of 200.
    /// Useful for testing check_inference failure paths.
    pub fn completions_fail(mut self) -> Self {
        self.completions_fail = true;
        self
    }

    /// Start the mock server with the configured behavior.
    pub async fn start(self) -> MockLlamaServer {
        let config = MockConfig {
            health_delay: self.health_delay,
            health_fail_after: self.health_fail_after,
            health_fail_first: self.health_fail_first,
            completions_fail: self.completions_fail,
        };

        let state = MockState {
            config,
            health_request_count: Arc::new(AtomicU64::new(0)),
        };

        let app = Router::new()
            .route("/health", get(health_handler))
            .route("/v1/models", get(models_handler))
            .route("/props", get(props_handler))
            .route("/slots", get(slots_handler))
            .route("/v1/chat/completions", post(chat_completions_handler))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind mock server to port 0");
        let port = listener.local_addr().unwrap().port();

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("mock server failed");
        });

        MockLlamaServer {
            port,
            shutdown_tx: Some(shutdown_tx),
            handle: Some(handle),
        }
    }
}

impl MockLlamaServer {
    /// Start a mock server with default behavior (all endpoints return 200 immediately).
    pub async fn start() -> Self {
        Self::builder().start().await
    }

    /// Create a builder for configuring mock server behavior.
    pub fn builder() -> MockLlamaServerBuilder {
        MockLlamaServerBuilder {
            health_delay: None,
            health_fail_after: None,
            health_fail_first: None,
            completions_fail: false,
        }
    }

    /// Returns the port the mock server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Gracefully shut down the mock server.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for MockLlamaServer {
    fn drop(&mut self) {
        // Best-effort shutdown: send the signal so the server task can exit.
        // The JoinHandle will be dropped, which doesn't abort the task by default
        // in tokio, but since we sent the shutdown signal, it will exit soon.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

// ── Handlers ──────────────────────────────────────────────────────────

async fn health_handler(
    State(state): State<MockState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Apply configurable delay
    if let Some(delay) = state.config.health_delay {
        tokio::time::sleep(delay).await;
    }

    let count = state.health_request_count.fetch_add(1, Ordering::SeqCst);

    // Apply fail-first-N behavior: first N requests return 500, then 200
    if let Some(fail_first) = state.config.health_fail_first
        && count < fail_first
    {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Apply failure-after-N behavior
    if let Some(fail_after) = state.config.health_fail_after
        && count >= fail_after
    {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok(Json(serde_json::json!({"status": "ok"})))
}

async fn models_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "data": [{
            "id": "mock-model",
            "owned_by": "test"
        }]
    }))
}

async fn props_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "total_slots": 1,
        "chat_template": "test"
    }))
}

async fn slots_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!([{
        "id": 0,
        "state": 0,
        "prompt": "",
        "next_token": {}
    }]))
}

async fn chat_completions_handler(
    State(state): State<MockState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.config.completions_fail {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(Json(serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Hello!"
            },
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
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_server_starts_and_responds_health() {
        let server = MockLlamaServer::start().await;
        let port = server.port();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .expect("health request failed");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_mock_server_v1_models() {
        let server = MockLlamaServer::start().await;
        let port = server.port();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/v1/models"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"][0]["id"], "mock-model");
        assert_eq!(body["data"][0]["owned_by"], "test");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_mock_server_props() {
        let server = MockLlamaServer::start().await;
        let port = server.port();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/props"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["total_slots"], 1);
        assert_eq!(body["chat_template"], "test");

        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_mock_server_slots() {
        let server = MockLlamaServer::start().await;
        let port = server.port();

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/slots"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body.is_array());
        assert_eq!(body[0]["id"], 0);
        assert_eq!(body[0]["state"], 0);

        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_mock_server_chat_completions() {
        let server = MockLlamaServer::start().await;
        let port = server.port();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .json(&serde_json::json!({
                "model": "test",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 1
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        // Verify timings are present
        assert!(body["timings"]["predicted_per_second"].is_number());
        assert!(body["usage"]["total_tokens"].is_number());

        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_mock_server_health_delay() {
        let server = MockLlamaServer::builder()
            .health_delay(Duration::from_millis(200))
            .start()
            .await;
        let port = server.port();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let start = tokio::time::Instant::now();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .unwrap();
        let elapsed = start.elapsed();

        assert_eq!(resp.status(), 200);
        assert!(
            elapsed >= Duration::from_millis(150),
            "health delay should be at least ~200ms, was {elapsed:?}"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_mock_server_health_fail_after() {
        let server = MockLlamaServer::builder()
            .health_fail_after(2)
            .start()
            .await;
        let port = server.port();

        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/health");

        // First 2 requests succeed
        let resp1 = client.get(&url).send().await.unwrap();
        assert_eq!(resp1.status(), 200, "request 1 should succeed");

        let resp2 = client.get(&url).send().await.unwrap();
        assert_eq!(resp2.status(), 200, "request 2 should succeed");

        // Third request fails
        let resp3 = client.get(&url).send().await.unwrap();
        assert_eq!(
            resp3.status(),
            500,
            "request 3 should fail (health_fail_after=2)"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_mock_server_shutdown_is_clean() {
        let server = MockLlamaServer::start().await;
        let port = server.port();

        // Verify it's running
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await;
        assert!(resp.is_ok());

        // Shutdown
        server.shutdown().await;

        // Give the OS a moment to release the port
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify it's no longer responding
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await;
        assert!(resp.is_err(), "server should not respond after shutdown");
    }

    #[tokio::test]
    async fn test_mock_server_drop_cleans_up() {
        let port;
        {
            let server = MockLlamaServer::start().await;
            port = server.port();

            // Verify it's running
            let client = reqwest::Client::new();
            let resp = client
                .get(format!("http://127.0.0.1:{port}/health"))
                .send()
                .await;
            assert!(resp.is_ok());
            // server is dropped here
        }

        // Give the OS a moment
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify it's no longer responding after drop
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await;
        assert!(resp.is_err(), "server should not respond after drop");
    }
}

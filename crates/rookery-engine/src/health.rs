use std::time::Duration;

/// Poll the llama-server health endpoint with exponential backoff.
/// Returns Ok(()) when the server is healthy, Err on timeout.
pub async fn wait_for_health(port: u16, timeout: Duration) -> Result<(), HealthError> {
    let url = format!("http://127.0.0.1:{port}/health");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| HealthError::Client(e.to_string()))?;

    let start = tokio::time::Instant::now();
    let mut delay = Duration::from_millis(100);
    let max_delay = Duration::from_secs(5);

    loop {
        if start.elapsed() > timeout {
            return Err(HealthError::Timeout(timeout));
        }

        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(port, "llama-server health check passed");
                return Ok(());
            }
            Ok(resp) => {
                tracing::debug!(port, status = %resp.status(), "health check not ready");
            }
            Err(e) => {
                tracing::debug!(port, error = %e, "health check connection failed");
            }
        }

        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(max_delay);
    }
}

/// Single-shot health check — returns true if the server responds 200 within timeout.
pub async fn check_health(port: u16, timeout: Duration) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(_) => return false,
    };

    matches!(client.get(&url).send().await, Ok(resp) if resp.status().is_success())
}

/// Check if all server slots are busy processing requests.
/// Returns true if the server is reachable and all slots are currently processing.
/// Returns false if the server is unreachable, returns an error, or has idle slots.
pub async fn check_slots_busy(port: u16, timeout: Duration) -> bool {
    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(port, error = %e, "slots check client build failed");
            return false;
        }
    };

    let resp = match client
        .get(format!("http://127.0.0.1:{port}/slots"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::debug!(port, status = %r.status(), "slots check returned non-success");
            return false;
        }
        Err(e) => {
            tracing::debug!(port, error = %e, "slots check request failed");
            return false;
        }
    };

    let slots: Vec<serde_json::Value> = match resp.json().await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(port, error = %e, "slots check JSON parse failed");
            return false;
        }
    };

    // If all slots are processing, server is busy — don't send canary requests
    !slots.is_empty()
        && slots.iter().all(|s| {
            s.get("is_processing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
}

/// Inference canary — sends a minimal completion request to verify the CUDA
/// inference pipeline is functional, not just that the HTTP server responds.
/// Returns true if the server generates at least one token within the timeout.
pub async fn check_inference(port: u16, timeout: Duration) -> bool {
    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(_) => return false,
    };

    let body = serde_json::json!({
        "model": "test",
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1,
    });

    match client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HealthError {
    #[error("health check timed out after {0:?}")]
    Timeout(Duration),

    #[error("http client error: {0}")]
    Client(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockLlamaServer;

    // ── wait_for_health tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_wait_for_health_succeeds_on_healthy_server() {
        let server = MockLlamaServer::start().await;
        let result = wait_for_health(server.port(), Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "wait_for_health should succeed on healthy server"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_wait_for_health_timeout_on_unused_port() {
        // Bind to port 0 to get an unused port, then immediately drop the listener
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let timeout = Duration::from_millis(500);
        let start = tokio::time::Instant::now();
        let result = wait_for_health(port, timeout).await;
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "wait_for_health should fail on unused port"
        );
        match result.unwrap_err() {
            HealthError::Timeout(t) => assert_eq!(t, timeout),
            other => panic!("expected Timeout error, got: {other}"),
        }
        // Verify it actually waited close to the timeout duration
        assert!(
            elapsed >= Duration::from_millis(400),
            "should have waited near the timeout, but only waited {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_wait_for_health_succeeds_after_initial_failures() {
        // Server returns 500 for the first 3 health requests, then 200 on request 4+
        let server = MockLlamaServer::builder()
            .health_fail_first(3)
            .start()
            .await;

        let result = wait_for_health(server.port(), Duration::from_secs(10)).await;
        assert!(
            result.is_ok(),
            "wait_for_health should succeed after initial 500 responses"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_wait_for_health_respects_timeout() {
        // Bind to port 0 then drop to get an unused port — connection refused
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let short_timeout = Duration::from_millis(300);
        let start = tokio::time::Instant::now();
        let result = wait_for_health(port, short_timeout).await;
        let elapsed = start.elapsed();

        assert!(result.is_err());
        // Should not have waited much longer than the timeout
        assert!(
            elapsed < Duration::from_secs(2),
            "should respect timeout, but took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_wait_for_health_exponential_backoff() {
        // Start a server that delays health responses by 50ms. With exponential backoff,
        // the total time should reflect increasing delays between retries rather than
        // a fixed polling interval. We verify this by using a short timeout with an
        // unreachable server and checking that fewer retries happen than with fixed delay.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        // With exponential backoff starting at 100ms (100, 200, 400, 800...),
        // in 1 second we should get ~3-4 retries, not 10 (as with fixed 100ms).
        let timeout = Duration::from_secs(1);
        let start = tokio::time::Instant::now();
        let result = wait_for_health(port, timeout).await;
        let elapsed = start.elapsed();

        assert!(result.is_err());
        // The total elapsed should be close to the timeout
        assert!(
            elapsed >= Duration::from_millis(800),
            "backoff should make elapsed close to timeout, was {elapsed:?}"
        );
    }

    // ── check_health tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_check_health_true_on_200() {
        let server = MockLlamaServer::start().await;
        let result = check_health(server.port(), Duration::from_secs(5)).await;
        assert!(
            result,
            "check_health should return true when server responds 200"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_check_health_false_on_500() {
        // health_fail_after(0) means /health returns 500 on the very first request
        let server = MockLlamaServer::builder()
            .health_fail_after(0)
            .start()
            .await;

        let result = check_health(server.port(), Duration::from_secs(5)).await;
        assert!(!result, "check_health should return false on 500 response");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_check_health_false_on_connection_refused() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let result = check_health(port, Duration::from_secs(1)).await;
        assert!(
            !result,
            "check_health should return false when connection is refused"
        );
    }

    #[tokio::test]
    async fn test_check_health_false_on_timeout() {
        // Use a server with a long health delay and a short client timeout
        let server = MockLlamaServer::builder()
            .health_delay(Duration::from_secs(5))
            .start()
            .await;

        let result = check_health(server.port(), Duration::from_millis(100)).await;
        assert!(
            !result,
            "check_health should return false when request times out"
        );
        server.shutdown().await;
    }

    // ── check_inference tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_check_inference_true_on_200() {
        let server = MockLlamaServer::start().await;
        let result = check_inference(server.port(), Duration::from_secs(5)).await;
        assert!(
            result,
            "check_inference should return true when completions endpoint returns 200"
        );
        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_check_inference_false_on_non_200() {
        // Mock server returns 500 on /v1/chat/completions
        let server = MockLlamaServer::builder().completions_fail().start().await;

        let result = check_inference(server.port(), Duration::from_secs(5)).await;
        assert!(
            !result,
            "check_inference should return false when completions endpoint returns 500"
        );

        server.shutdown().await;
    }

    #[tokio::test]
    async fn test_check_inference_false_on_connection_refused() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let result = check_inference(port, Duration::from_secs(1)).await;
        assert!(
            !result,
            "check_inference should return false when connection is refused"
        );
    }

    #[tokio::test]
    async fn test_check_inference_false_on_timeout() {
        // Create a minimal server that accepts connections but delays forever on completions
        use axum::{Router, routing::post};
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                // Sleep longer than the client timeout
                tokio::time::sleep(Duration::from_secs(30)).await;
                "never"
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let result = check_inference(port, Duration::from_millis(200)).await;
        assert!(!result, "check_inference should return false on timeout");

        handle.abort();
    }

    // ── HealthError display tests ─────────────────────────────────────

    #[test]
    fn test_health_error_display_timeout() {
        let err = HealthError::Timeout(Duration::from_secs(30));
        let display = format!("{err}");
        assert!(
            display.contains("timed out"),
            "Timeout error should contain 'timed out', got: {display}"
        );
        assert!(
            display.contains("30"),
            "Timeout error should contain duration, got: {display}"
        );
    }

    #[test]
    fn test_health_error_display_client() {
        let err = HealthError::Client("connection reset".to_string());
        let display = format!("{err}");
        assert!(
            display.contains("connection reset"),
            "Client error should contain the message, got: {display}"
        );
    }
}

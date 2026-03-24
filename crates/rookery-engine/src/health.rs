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
    let client = match reqwest::Client::builder()
        .timeout(timeout)
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    matches!(client.get(&url).send().await, Ok(resp) if resp.status().is_success())
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

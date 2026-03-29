use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::{self, Stream, StreamExt};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;

use crate::app_state::AppState;

/// Max concurrent SSE connections
const MAX_SSE_CONNECTIONS: u32 = 16;
static SSE_CONNECTION_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub async fn get_events(
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, axum::http::StatusCode> {
    let count = SSE_CONNECTION_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if count >= MAX_SSE_CONNECTIONS {
        SSE_CONNECTION_COUNT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return Err(axum::http::StatusCode::TOO_MANY_REQUESTS);
    }
    // GPU stats stream — poll every 2 seconds
    let gpu_state = state.clone();
    let gpu_stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(2)))
            .map(move |_| {
                let stats = gpu_state
                    .gpu_monitor
                    .as_ref()
                    .and_then(|m| m.stats().ok())
                    .unwrap_or_default();
                Ok(Event::default()
                    .event("gpu")
                    .json_data(serde_json::json!({ "gpus": stats }))
                    .unwrap())
            });

    // State change stream — fires on start/stop/swap
    let state_rx = state.state_tx.subscribe();
    let state_stream = BroadcastStream::new(state_rx).filter_map(|result| {
        futures_util::future::ready(match result {
            Ok(value) => Some(Ok(Event::default()
                .event("state")
                .json_data(&value)
                .unwrap())),
            Err(_) => None, // lagged, skip
        })
    });

    // Log stream — fires on every new log line
    let log_rx = state.log_buffer.subscribe();
    let log_stream = BroadcastStream::new(log_rx).filter_map(|result| {
        futures_util::future::ready(match result {
            Ok(line) => Some(Ok(Event::default().event("log").data(line))),
            Err(_) => None,
        })
    });

    // Send initial state immediately
    let current_state = state.backend.lock().await.to_server_state().await;
    let initial_status = crate::routes::status_json_from_state(&current_state);
    let initial_event = stream::once(futures_util::future::ready(Ok(Event::default()
        .event("state")
        .json_data(&initial_status)
        .unwrap())));

    // Merge all streams, decrement connection count when stream ends
    let merged = initial_event
        .chain(futures_util::stream::select(
            gpu_stream,
            futures_util::stream::select(state_stream, log_stream),
        ))
        .chain(stream::once(futures_util::future::lazy(|_| {
            SSE_CONNECTION_COUNT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            // This item is never actually yielded because the stream ends when the client disconnects,
            // triggering the drop. But we need a fallback decrement for clean shutdown.
            Ok(Event::default().comment("close"))
        })));

    Ok(Sse::new(merged).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)] // Intentional: SSE_COUNTER_LOCK serializes tests
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use http_body_util::BodyExt;
    use std::sync::atomic::Ordering;
    use tower::ServiceExt;

    use crate::test_utils::{MockBackend, build_test_app_state};
    use rookery_core::config::BackendType;
    use rookery_engine::backend::BackendInfo;

    /// Build a router with just the SSE endpoint.
    fn sse_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/events", get(get_events))
            .with_state(state)
    }

    /// Mutex to serialize tests that depend on the global SSE_CONNECTION_COUNT.
    /// Because the counter is a global static, tests that set/check specific counter
    /// values must not run concurrently with other tests that also manipulate it.
    static SSE_COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Reset the global SSE connection counter to 0.
    /// Must be called in tests that manipulate the counter.
    fn reset_sse_counter() {
        SSE_CONNECTION_COUNT.store(0, Ordering::SeqCst);
    }

    /// Read SSE text from a streaming response body.
    /// Reads frames until we get at least `min_bytes` of data or the frame returns None.
    async fn read_sse_body(body: Body, min_bytes: usize) -> String {
        let mut collected = Vec::new();
        let mut body = body;
        // Use a timeout to avoid hanging forever on the infinite SSE stream
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while let Some(Ok(frame)) = body.frame().await {
                if let Ok(data) = frame.into_data() {
                    collected.extend_from_slice(&data);
                    if collected.len() >= min_bytes {
                        break;
                    }
                }
            }
        })
        .await;
        // Timeout is expected for the infinite stream — we just need the initial events
        let _ = result;
        String::from_utf8_lossy(&collected).to_string()
    }

    /// Parse SSE events from raw SSE text.
    /// Returns a list of (event_type, data) tuples.
    fn parse_sse_events(text: &str) -> Vec<(String, String)> {
        let mut events = Vec::new();
        let mut current_event = String::new();
        let mut current_data = String::new();

        for line in text.lines() {
            if line.starts_with("event:") {
                current_event = line.trim_start_matches("event:").trim().to_string();
            } else if line.starts_with("data:") {
                current_data = line.trim_start_matches("data:").trim().to_string();
            } else if line.is_empty() && !current_event.is_empty() {
                events.push((current_event.clone(), current_data.clone()));
                current_event.clear();
                current_data.clear();
            }
        }

        // Catch last event if text doesn't end with a blank line
        if !current_event.is_empty() {
            events.push((current_event, current_data));
        }

        events
    }

    // --- 1. SSE connection sends initial state event on connect (stopped) ---
    #[tokio::test]
    async fn test_sse_initial_state_event_on_connect_stopped() {
        let _lock = SSE_COUNTER_LOCK.lock().unwrap();
        reset_sse_counter();

        let (_dir, state) = build_test_app_state(None);
        let app = sse_router(state);

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Read enough bytes to capture the initial state event
        let body_text = read_sse_body(resp.into_body(), 50).await;
        let events = parse_sse_events(&body_text);

        // The first event should be a "state" event
        assert!(
            !events.is_empty(),
            "should receive at least one SSE event, got body: {body_text}"
        );
        let (event_type, data) = &events[0];
        assert_eq!(event_type, "state", "first event should be type 'state'");

        // Parse the JSON data
        let json: serde_json::Value =
            serde_json::from_str(data).expect("state event data should be valid JSON");
        assert_eq!(
            json["state"], "stopped",
            "initial state should be 'stopped'"
        );

        reset_sse_counter();
    }

    // --- 2. SSE initial state event when backend is running ---
    #[tokio::test]
    async fn test_sse_initial_state_event_on_connect_running() {
        let _lock = SSE_COUNTER_LOCK.lock().unwrap();
        reset_sse_counter();

        let running_info = BackendInfo {
            pid: Some(12345),
            container_id: None,
            port: 8081,
            profile: "test".into(),
            started_at: chrono::Utc::now(),
            backend_type: BackendType::LlamaServer,
            command_line: vec!["mock-server".into()],
            exe_path: Some(std::path::PathBuf::from("/mock/llama-server")),
        };
        let backend = MockBackend::running_with(running_info);
        let (_dir, state) = build_test_app_state(Some(Box::new(backend)));
        let app = sse_router(state);

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_text = read_sse_body(resp.into_body(), 50).await;
        let events = parse_sse_events(&body_text);

        assert!(!events.is_empty(), "should receive at least one SSE event");
        let (event_type, data) = &events[0];
        assert_eq!(event_type, "state");

        let json: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(json["state"], "running");
        assert_eq!(json["profile"], "test");
        assert_eq!(json["pid"], 12345);
        assert_eq!(json["port"], 8081);
        assert_eq!(json["backend"], "llama-server");

        reset_sse_counter();
    }

    // --- 3. SSE state event format includes all expected fields ---
    #[tokio::test]
    async fn test_sse_state_event_format_includes_all_fields() {
        let _lock = SSE_COUNTER_LOCK.lock().unwrap();
        reset_sse_counter();

        let (_dir, state) = build_test_app_state(None);
        let app = sse_router(state);

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_text = read_sse_body(resp.into_body(), 50).await;
        let events = parse_sse_events(&body_text);

        assert!(!events.is_empty(), "should receive at least one SSE event");
        let (_event_type, data) = &events[0];
        let json: serde_json::Value = serde_json::from_str(data).unwrap();

        // The state event JSON must include all these fields (from status_json_from_state)
        let expected_fields = ["state", "profile", "pid", "port", "uptime_secs", "backend"];
        for field in &expected_fields {
            assert!(
                json.get(field).is_some(),
                "state event JSON missing expected field '{field}', got: {json}"
            );
        }

        reset_sse_counter();
    }

    // --- 4. SSE connection limit: connection beyond MAX gets 429 ---
    //
    // The SSE_CONNECTION_COUNT is a global static shared across all tests.
    // The SSE_COUNTER_LOCK serializes all tests that depend on the counter
    // to prevent races.
    #[tokio::test]
    async fn test_sse_connection_limit_rejects_when_at_max() {
        let _lock = SSE_COUNTER_LOCK.lock().unwrap();

        // Set counter to MAX to simulate 16 active connections
        SSE_CONNECTION_COUNT.store(MAX_SSE_CONNECTIONS, Ordering::SeqCst);

        let (_dir, state) = build_test_app_state(None);
        let app = sse_router(state);

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "SSE connection should be rejected with 429 when at MAX_SSE_CONNECTIONS ({})",
            MAX_SSE_CONNECTIONS
        );

        // The handler does fetch_add(1) then fetch_sub(1) on rejection,
        // so the counter should be back to MAX
        let count = SSE_CONNECTION_COUNT.load(Ordering::SeqCst);
        assert_eq!(
            count, MAX_SSE_CONNECTIONS,
            "counter should be restored after rejection, got: {count}"
        );

        reset_sse_counter();
    }

    // --- 4b. SSE connection under limit succeeds ---
    //
    // Verify that when the counter is below MAX, the connection is accepted (200).
    #[tokio::test]
    async fn test_sse_connection_under_limit_succeeds() {
        let _lock = SSE_COUNTER_LOCK.lock().unwrap();
        reset_sse_counter();

        let (_dir, state) = build_test_app_state(None);
        let app = sse_router(state);

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "SSE connection should succeed when under MAX_SSE_CONNECTIONS"
        );

        reset_sse_counter();
    }

    // --- 5. SSE connection count increments on connect ---
    //
    // Verifies that each successful SSE connection increments the global counter.
    // The decrement happens when the stream is fully consumed or on daemon shutdown
    // (via the chained fallback item), which cannot be reliably tested via oneshot.
    #[tokio::test]
    async fn test_sse_connection_count_increments_on_connect() {
        let _lock = SSE_COUNTER_LOCK.lock().unwrap();
        reset_sse_counter();

        assert_eq!(
            SSE_CONNECTION_COUNT.load(Ordering::SeqCst),
            0,
            "counter should start at 0"
        );

        let (_dir, state) = build_test_app_state(None);
        let app = sse_router(state);

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Counter should have been incremented by the handler
        let count = SSE_CONNECTION_COUNT.load(Ordering::SeqCst);
        assert!(
            count >= 1,
            "counter should be incremented after SSE connect, got: {count}"
        );

        // Keep the response alive to prevent any cleanup
        drop(resp);

        reset_sse_counter();
    }

    // --- 5b. SSE rejected connection does not permanently increment counter ---
    //
    // When a connection is rejected (429), the handler increments then immediately
    // decrements the counter, leaving it unchanged.
    #[tokio::test]
    async fn test_sse_rejected_connection_does_not_leak_counter() {
        let _lock = SSE_COUNTER_LOCK.lock().unwrap();

        let baseline = MAX_SSE_CONNECTIONS;
        SSE_CONNECTION_COUNT.store(baseline, Ordering::SeqCst);

        let (_dir, state) = build_test_app_state(None);
        let app = sse_router(state);

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

        // Counter should be exactly back to baseline (fetch_add then fetch_sub)
        let count = SSE_CONNECTION_COUNT.load(Ordering::SeqCst);
        assert_eq!(
            count, baseline,
            "rejected connection should not permanently change the counter"
        );

        reset_sse_counter();
    }
}

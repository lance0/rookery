use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::{self, Stream, StreamExt};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;

use crate::app_state::AppState;
#[cfg(test)]
use crate::metrics::MAX_SSE_CONNECTIONS;
use crate::metrics::sse_connection_guard;

/// How often the stream proves it is still alive.
///
/// This has to be a *named* event rather than an SSE comment. Browsers never
/// surface comments to JavaScript, so a comment can stop an intermediary
/// timing the socket out — that is what the `KeepAlive` below is for — but it
/// cannot tell the dashboard anything. A named event solves both halves at
/// once: `EventSource.onmessage` only fires for the default `message` type, so
/// `ping` is invisible to it and can never be mistaken for data, while an
/// explicit `addEventListener("ping", …)` feeds the client's staleness clock.
///
/// Must stay comfortably under the dashboard's 3s freshness threshold.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

pub async fn get_events(
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, axum::http::StatusCode> {
    if !state.metrics.try_acquire_sse_connection() {
        return Err(axum::http::StatusCode::TOO_MANY_REQUESTS);
    }
    let guard = Arc::new(sse_connection_guard(state.metrics.clone()));
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
                    .unwrap_or_else(|_| Event::default().event("gpu").data("{}")))
            });

    // Heartbeat — fires on a fixed interval whether or not anything changed,
    // so the dashboard can tell "quiet" from "wedged". The payload is the
    // server clock, which is only there because an SSE event with an empty
    // data buffer is never dispatched by the browser.
    let heartbeat_stream = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
        HEARTBEAT_INTERVAL,
    ))
    .map(|_| {
        Ok(Event::default()
            .event("ping")
            .data(chrono::Utc::now().timestamp_millis().to_string()))
    });

    // State change stream — fires on start/stop/swap
    let state_rx = state.state_tx.subscribe();
    let state_stream = BroadcastStream::new(state_rx).filter_map(|result| {
        futures_util::future::ready(match result {
            Ok(value) => Some(Ok(Event::default()
                .event("state")
                .json_data(&value)
                .unwrap_or_else(|_| Event::default().event("state").data("{}")))),
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
    let current_state = state.current_state().await;
    let initial_status = crate::routes::status_json_from_state(&current_state);
    let initial_event = stream::once(futures_util::future::ready(Ok(Event::default()
        .event("state")
        .json_data(&initial_status)
        .unwrap_or_else(|_| Event::default().event("state").data("{}")))));

    // Keep a guard captured by the stream so the current-connection gauge
    // is decremented when the stream is dropped or completes.
    let stream_guard = guard.clone();
    let merged = initial_event
        .chain(futures_util::stream::select(
            heartbeat_stream,
            futures_util::stream::select(
                gpu_stream,
                futures_util::stream::select(state_stream, log_stream),
            ),
        ))
        .map(move |event| {
            let _guard = &stream_guard;
            event
        });

    // Comment heartbeat (`: ping`) for intermediaries only — invisible to the
    // client, and idle-triggered, so it costs nothing while events are flowing.
    Ok(Sse::new(merged).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::test_utils::{MockBackend, build_test_app_state, sync_state_from_backend};
    use rookery_core::config::BackendType;
    use rookery_engine::backend::BackendInfo;

    /// Build a router with just the SSE endpoint.
    fn sse_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/events", get(get_events))
            .with_state(state)
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
    }

    // --- 2. SSE initial state event when backend is running ---
    #[tokio::test]
    async fn test_sse_initial_state_event_on_connect_running() {
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
        sync_state_from_backend(&state).await;
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
    }

    // --- 3. SSE state event format includes all expected fields ---
    #[tokio::test]
    async fn test_sse_state_event_format_includes_all_fields() {
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
    }

    // --- 3b. Heartbeat is a NAMED event, so it can never reach onmessage ---
    //
    // The staleness watchdog on the client is fed by this event, but it must
    // never be read as data. `EventSource.onmessage` fires only for events
    // with no `event:` line (type `message`), so the invariant to hold is:
    // the heartbeat is present, and nothing on this stream is unnamed.
    #[tokio::test]
    async fn test_sse_heartbeat_is_a_named_ping_event() {
        let (_dir, state) = build_test_app_state(None);
        let app = sse_router(state);

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_text = read_sse_body(resp.into_body(), 400).await;
        let events = parse_sse_events(&body_text);

        assert!(
            events.iter().any(|(kind, _)| kind == "ping"),
            "expected a heartbeat 'ping' event, got: {body_text}"
        );

        for block in body_text.split("\n\n").filter(|b| b.contains("data:")) {
            assert!(
                block.lines().any(|line| line.starts_with("event:")),
                "unnamed event would surface via onmessage as data: {block:?}"
            );
        }
    }

    // --- 4. SSE connection limit: connection beyond MAX gets 429 ---
    #[tokio::test]
    async fn test_sse_connection_limit_rejects_when_at_max() {
        let (_dir, state) = build_test_app_state(None);
        state
            .metrics
            .set_sse_connections_current_for_test(MAX_SSE_CONNECTIONS);
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
    }

    // --- 4b. SSE connection under limit succeeds ---
    //
    // Verify that when the counter is below MAX, the connection is accepted (200).
    #[tokio::test]
    async fn test_sse_connection_under_limit_succeeds() {
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
    }

    // --- 5. SSE connection count increments on connect ---
    //
    // Verifies that a successful SSE connection increments the current count.
    #[tokio::test]
    async fn test_sse_connection_count_increments_on_connect() {
        let (_dir, state) = build_test_app_state(None);
        assert_eq!(state.metrics.sse_connections_current_value(), 0);
        let app = sse_router(state);

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body();
        drop(body);
    }

    // --- 5b. SSE rejected connection does not increment total ---
    #[tokio::test]
    async fn test_sse_rejected_connection_does_not_leak_counter() {
        let (_dir, state) = build_test_app_state(None);
        state
            .metrics
            .set_sse_connections_current_for_test(MAX_SSE_CONNECTIONS);
        let baseline = state.metrics.sse_connections_total_value();
        let app = sse_router(state.clone());

        let req = Request::builder()
            .uri("/api/events")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(state.metrics.sse_connections_total_value(), baseline);
    }
}

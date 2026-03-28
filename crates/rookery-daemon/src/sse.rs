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

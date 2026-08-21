//! Dashboard end of the event stream: liveness, staleness, and reconnect.
//!
//! `EventSource` only reports that the *socket* died. It says nothing about a
//! daemon still holding the connection open while its stat task is wedged:
//! `readyState` stays `OPEN`, no error fires, and the gauges keep showing
//! whatever arrived last as if it were current. For a dashboard whose whole
//! job is telling you the truth about a GPU, that is the worst failure mode.
//!
//! So freshness is tracked explicitly. Every inbound event stamps
//! `last_event_at`, and a 1s watchdog turns the age of that stamp — plus
//! `readyState` — into a [`ConnState`] the header chip renders. The daemon
//! emits a `ping` event on a fixed interval so the stamp advances even when
//! nothing changed.

use std::cell::RefCell;
use std::time::Duration;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api;
use crate::{GpuData, ModelInfoData, ServerStatus};

/// Data younger than this is live.
const FRESH_MS: f64 = 3_000.0;
/// Older than `FRESH_MS` but younger than this: numbers dim and read "stale".
/// Past it the stream is treated as dead and forcibly reconnected, however
/// healthy `readyState` claims to be.
const STALE_MS: f64 = 10_000.0;
/// Reconnect backoff: 1s, 2s, 4s, 8s, 16s, then capped. `1000 << 5` is 32s,
/// which [`backoff_ms`] clamps to [`BACKOFF_CAP_MS`].
const BACKOFF_MAX_SHIFT: u32 = 5;
const BACKOFF_CAP_MS: u32 = 30_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ConnState {
    /// An event arrived within `FRESH_MS`. Numbers are current.
    Live,
    /// Socket is open but silent for `FRESH_MS..STALE_MS`. Numbers are dimmed.
    Stale,
    /// Socket is not open, or has been silent past `STALE_MS`. A retry is
    /// either in the browser's hands or scheduled by us.
    #[default]
    Reconnecting,
    /// Stream is closed and we are deliberately not retrying — the API key
    /// was rejected or cleared, so retrying would just burn 401s.
    Disconnected,
}

impl ConnState {
    pub fn label(self) -> &'static str {
        match self {
            ConnState::Live => "live",
            ConnState::Stale => "stale",
            ConnState::Reconnecting => "Disconnected — reconnecting…",
            ConnState::Disconnected => "disconnected",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            ConnState::Live => "live",
            ConnState::Stale => "stale",
            ConnState::Reconnecting => "reconnecting",
            ConnState::Disconnected => "dead",
        }
    }

    /// Whether the numbers on screen can still be presented as authoritative.
    pub fn data_class(self) -> &'static str {
        match self {
            ConnState::Live => "",
            ConnState::Stale => " data-stale",
            ConnState::Reconnecting | ConnState::Disconnected => " data-dead",
        }
    }
}

/// The live stream plus the closures wired to it, owned together so they can
/// be torn down together.
///
/// Listeners used to be registered with `Closure::forget()`. That was harmless
/// while the connection was established once at startup; with an explicit
/// reconnect path it leaks the whole set every time — six closures per
/// reconnect, ~120/hour at the 30s backoff cap.
///
/// They cannot simply be dropped instead, because dropping a [`Closure`]
/// invalidates the JS function pointer it handed out — calling one afterwards
/// is a use-after-free that surfaces as a hard WASM trap in the console, not a
/// Rust error. So the teardown order is fixed, and lives in [`Drop`] rather
/// than at the call sites so it cannot be got wrong by a later caller.
struct Connection {
    /// Our own handle, independent of `SseHandles::event_source`: teardown must
    /// be able to detach the listeners even if something else already cleared
    /// that signal (`on_logout` and the `rookery-auth-required` handler both
    /// do).
    es: web_sys::EventSource,
    /// Named listeners, kept with the name they were registered under so
    /// `removeEventListener` can be handed the identical (type, callback) pair.
    named: NamedListeners,
    /// `onerror` and `onopen` are set as properties, not listeners, so they are
    /// detached with `set_onerror(None)` / `set_onopen(None)` and never need to
    /// be read back. They are fields purely so each closure lives exactly as
    /// long as the `EventSource` that can call it.
    #[allow(dead_code)]
    on_error: Closure<dyn FnMut(web_sys::Event)>,
    #[allow(dead_code)]
    on_open: Closure<dyn FnMut(web_sys::Event)>,
}

type NamedListeners = Vec<(&'static str, Closure<dyn FnMut(web_sys::MessageEvent)>)>;

impl Drop for Connection {
    fn drop(&mut self) {
        // Ordering is load-bearing:
        //
        //   1. `close()` — readyState goes CLOSED and the UA stops dispatching
        //      to this EventSource, so no *pending* event can still land.
        //   2. detach — after this the EventSource holds no reference to any of
        //      our function pointers, so no *future* dispatch can reach them
        //      either, whatever else still holds the object.
        //   3. the closures drop, on return from this fn, when the fields drop.
        //
        // Only after 1 and 2 is a closure unreachable from JS, so only then is
        // freeing it safe. Doing it the other way round is the use-after-free.
        self.es.close();
        self.es.set_onerror(None);
        self.es.set_onopen(None);
        for (name, callback) in &self.named {
            let _ = self
                .es
                .remove_event_listener_with_callback(name, callback.as_ref().unchecked_ref());
        }
    }
}

thread_local! {
    /// The one live connection. WASM is single-threaded and the dashboard opens
    /// exactly one stream, so this needs no more machinery than a slot.
    static LIVE: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

/// Drop the previous connection — see [`Connection::drop`] for the ordering.
///
/// Safe to call from anywhere *except* synchronously inside one of the
/// closures it frees. Nothing does: the only listener that triggers a
/// reconnect is `on_error`, and it goes through [`schedule_reconnect`], which
/// always crosses a timer await before reaching [`connect_sse`].
fn close_current(handles: SseHandles) {
    // Bound out of the `with` so the `RefCell` borrow is released before the
    // connection drops.
    let previous = LIVE.with(|slot| slot.borrow_mut().take());
    drop(previous);
    handles.set_event_source.set(None);
}

/// Everything the stream needs to write into. All fields are `Copy` signals so
/// the error handler can schedule a reconnect from inside itself.
#[derive(Clone, Copy)]
pub struct SseHandles {
    pub event_source: ReadSignal<Option<web_sys::EventSource>>,
    pub set_event_source: WriteSignal<Option<web_sys::EventSource>>,
    pub set_gpu: WriteSignal<GpuData>,
    pub set_status: WriteSignal<ServerStatus>,
    pub set_model_info: WriteSignal<ModelInfoData>,
    pub set_logs: WriteSignal<Vec<String>>,
    pub conn: RwSignal<ConnState>,
    pub last_event_at: RwSignal<f64>,
    pub retry_attempt: RwSignal<u32>,
    pub retry_pending: RwSignal<bool>,
}

impl SseHandles {
    fn touch(&self) {
        self.last_event_at.set(js_sys::Date::now());
    }
}

/// Exponential backoff with a 30s ceiling and ±25% jitter.
///
/// The jitter matters more than the curve here: the daemon returns 429 past
/// `MAX_SSE_CONNECTIONS`, so the tabs that get rejected are by definition all
/// retrying at once. Without jitter they would stay in lockstep forever.
fn backoff_ms(attempt: u32) -> u32 {
    let base = (1000u32 << attempt.min(BACKOFF_MAX_SHIFT)).min(BACKOFF_CAP_MS);
    let jitter = 1.0 + (js_sys::Math::random() - 0.5) * 0.5;
    (f64::from(base) * jitter) as u32
}

/// Queue one reconnect. Guarded so a burst of `error` events (or the watchdog
/// firing while a retry is already in flight) cannot stack timers — each extra
/// timer would open another connection against `MAX_SSE_CONNECTIONS`.
fn schedule_reconnect(handles: SseHandles) {
    if handles.retry_pending.get_untracked() {
        return;
    }
    handles.retry_pending.set(true);
    let attempt = handles.retry_attempt.get_untracked();
    handles.retry_attempt.set(attempt.saturating_add(1));
    let delay = backoff_ms(attempt);

    wasm_bindgen_futures::spawn_local(async move {
        gloo_timers::future::sleep(Duration::from_millis(u64::from(delay))).await;
        handles.retry_pending.set(false);

        // `EventSource` hides the HTTP status, so a permanently-closed stream
        // could be a 429 (worth retrying) or a 401 (not). Probe a cheap
        // authenticated endpoint: `api` already dispatches
        // `rookery-auth-required` on 401, which tears the stream down and
        // shows the key prompt.
        if let Err(error) = api::fetch_profiles().await
            && api::is_unauthorized(&error)
        {
            handles.conn.set(ConnState::Disconnected);
            return;
        }

        connect_sse(handles);
    });
}

/// Force a fresh connection now, dropping any existing one first so the
/// daemon's `rookery_sse_connections_current` gauge is released.
fn reconnect_now(handles: SseHandles) {
    if handles.retry_pending.get_untracked() {
        return;
    }
    close_current(handles);
    schedule_reconnect(handles);
}

pub fn connect_sse(handles: SseHandles) {
    close_current(handles);

    let Ok(es) = web_sys::EventSource::new(&api::events_url()) else {
        handles.conn.set(ConnState::Disconnected);
        return;
    };

    handles.touch();
    handles.conn.set(ConnState::Live);

    // Heartbeat. A named event never reaches `EventSource.onmessage`, so this
    // resets staleness without ever being mistaken for data.
    let on_ping = Closure::<dyn FnMut(_)>::new(move |_: web_sys::MessageEvent| {
        handles.touch();
    });
    let _ = es.add_event_listener_with_callback("ping", on_ping.as_ref().unchecked_ref());

    let on_gpu = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
        handles.touch();
        if let Some(data) = e.data().as_string()
            && let Ok(stats) = serde_json::from_str::<GpuData>(&data)
        {
            handles.set_gpu.set(stats);
        }
    });
    let _ = es.add_event_listener_with_callback("gpu", on_gpu.as_ref().unchecked_ref());

    let on_state = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
        handles.touch();
        if let Some(data) = e.data().as_string()
            && let Ok(status) = serde_json::from_str::<ServerStatus>(&data)
        {
            let is_running = status.state == "running";
            handles.set_status.set(status);
            if is_running {
                let set_model_info_retry = handles.set_model_info;
                wasm_bindgen_futures::spawn_local(async move {
                    gloo_timers::future::sleep(Duration::from_millis(500)).await;
                    if let Ok(model_info) = api::fetch_model_info().await {
                        set_model_info_retry.set(model_info);
                    }
                });
            } else {
                handles.set_model_info.set(ModelInfoData::default());
            }
        }
    });
    let _ = es.add_event_listener_with_callback("state", on_state.as_ref().unchecked_ref());

    let on_log = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
        handles.touch();
        if let Some(data) = e.data().as_string() {
            handles.set_logs.update(|lines| {
                lines.push(data);
                if lines.len() > 500 {
                    lines.drain(..lines.len() - 500);
                }
            });
        }
    });
    let _ = es.add_event_listener_with_callback("log", on_log.as_ref().unchecked_ref());

    // `error` fires both for a transport blip the browser will retry itself
    // (`readyState == CONNECTING`) and for a permanently closed stream. Per
    // spec a non-2xx response — the daemon's 429 past `MAX_SSE_CONNECTIONS`,
    // or a 401 on a bad key — closes the stream for good, and nothing
    // reopens it. Only that case is ours to retry.
    let on_error = Closure::<dyn FnMut(_)>::new(move |_: web_sys::Event| {
        let closed = handles
            .event_source
            .get_untracked()
            .is_none_or(|es| es.ready_state() == web_sys::EventSource::CLOSED);
        handles.conn.set(ConnState::Reconnecting);
        if closed {
            schedule_reconnect(handles);
        }
    });
    es.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    let on_open = Closure::<dyn FnMut(_)>::new(move |_: web_sys::Event| {
        handles.touch();
        handles.retry_attempt.set(0);
        handles.conn.set(ConnState::Live);
    });
    es.set_onopen(Some(on_open.as_ref().unchecked_ref()));

    // Take ownership of the whole set. This replaces nothing — `close_current`
    // above already emptied the slot — so no closure is freed here.
    LIVE.with(|slot| {
        *slot.borrow_mut() = Some(Connection {
            es: es.clone(),
            named: vec![
                ("ping", on_ping),
                ("gpu", on_gpu),
                ("state", on_state),
                ("log", on_log),
            ],
            on_error,
            on_open,
        });
    });

    handles.set_event_source.set(Some(es));
}

/// Derive [`ConnState`] once a second from `readyState` plus the age of the
/// last event, and force a reconnect when an open-but-silent stream crosses
/// `STALE_MS`. That silent case is the whole point: it is invisible to
/// `EventSource`.
pub fn spawn_watchdog(handles: SseHandles) {
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            gloo_timers::future::sleep(Duration::from_secs(1)).await;

            let Some(ready_state) = handles
                .event_source
                .get_untracked()
                .map(|es| es.ready_state())
            else {
                // No stream at all: either a retry is pending or the key was
                // cleared. Leave whatever state put us here alone.
                continue;
            };

            let age = js_sys::Date::now() - handles.last_event_at.get_untracked();
            let next = match ready_state {
                web_sys::EventSource::OPEN if age < FRESH_MS => ConnState::Live,
                web_sys::EventSource::OPEN if age < STALE_MS => ConnState::Stale,
                _ => ConnState::Reconnecting,
            };
            if handles.conn.get_untracked() != next {
                handles.conn.set(next);
            }

            if ready_state == web_sys::EventSource::OPEN && age >= STALE_MS {
                reconnect_now(handles);
            }
        }
    });
}

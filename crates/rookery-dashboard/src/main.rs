mod api;
mod components;

use components::*;
use components::toast::{Toast, ToastKind, show_toast};
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerStatus {
    pub state: String,
    pub profile: Option<String>,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub uptime_secs: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuData {
    pub gpus: Vec<GpuStats>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuStats {
    pub index: u32,
    pub name: String,
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
    pub temperature_c: u32,
    pub utilization_pct: u32,
    pub power_watts: f32,
    pub power_limit_watts: f32,
    #[serde(default)]
    pub processes: Vec<GpuProcess>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuProcess {
    pub pid: u32,
    pub name: String,
    pub vram_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    pub model: String,
    pub port: u16,
    pub ctx_size: u32,
    pub reasoning_budget: i32,
    pub default: bool,
    pub estimated_vram_mb: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub pid: u32,
    pub status: serde_json::Value,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub uptime_secs: Option<i64>,
    #[serde(default)]
    pub total_restarts: Option<u32>,
    #[serde(default)]
    pub error_count: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentsData {
    pub agents: Vec<AgentInfo>,
    pub configured: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelInfoData {
    pub available: bool,
    pub model_id: Option<String>,
    pub owned_by: Option<String>,
    pub props: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Settings,
    Chat,
    Bench,
    Logs,
    Models,
}

fn get_window() -> Option<web_sys::Window> {
    web_sys::window()
}

fn get_storage() -> Option<web_sys::Storage> {
    get_window()?.local_storage().ok()?
}

fn init_theme() -> bool {
    // Returns true if light mode
    if let Some(storage) = get_storage() {
        if let Ok(Some(theme)) = storage.get_item("rookery-theme") {
            let is_light = theme == "light";
            if is_light {
                if let Some(doc) = get_window().and_then(|w| w.document()) {
                    if let Some(el) = doc.document_element() {
                        let _ = el.class_list().add_1("light");
                    }
                }
            }
            return is_light;
        }
    }
    false
}

fn toggle_theme(is_light: bool) -> bool {
    let new_light = !is_light;
    if let Some(doc) = get_window().and_then(|w| w.document()) {
        if let Some(el) = doc.document_element() {
            if new_light {
                let _ = el.class_list().add_1("light");
            } else {
                let _ = el.class_list().remove_1("light");
            }
        }
    }
    if let Some(storage) = get_storage() {
        let _ = storage.set_item("rookery-theme", if new_light { "light" } else { "dark" });
    }
    new_light
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let (status, set_status) = signal(ServerStatus::default());
    let (gpu, set_gpu) = signal(GpuData::default());
    let (logs, set_logs) = signal(Vec::<String>::new());
    let (profiles, set_profiles) = signal(Vec::<ProfileInfo>::new());
    let (agents, set_agents) = signal(AgentsData::default());
    let (connected, set_connected) = signal(false);
    let (model_info, set_model_info) = signal(ModelInfoData::default());
    let (tab, set_tab) = signal(Tab::Overview);
    let (toasts, set_toasts) = signal(Vec::<Toast>::new());
    let (is_light, set_is_light) = signal(init_theme());

    // Load initial data
    let set_profiles_init = set_profiles.clone();
    let set_agents_init = set_agents.clone();
    let set_logs_init = set_logs.clone();
    let set_model_info_init = set_model_info.clone();

    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(p) = api::fetch_profiles().await {
            set_profiles_init.set(p);
        }
        if let Ok(a) = api::fetch_agents().await {
            set_agents_init.set(a);
        }
        if let Ok(l) = api::fetch_logs(100).await {
            set_logs_init.set(l);
        }
        if let Ok(m) = api::fetch_model_info().await {
            set_model_info_init.set(m);
        }
    });

    // SSE connection
    let es = web_sys::EventSource::new("/api/events").ok();

    if let Some(ref es) = es {
        set_connected.set(true);

        // GPU events
        let set_gpu_sse = set_gpu.clone();
        let on_gpu = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if let Ok(stats) = serde_json::from_str::<GpuData>(&data) {
                    set_gpu_sse.set(stats);
                }
            }
        });
        es.add_event_listener_with_callback("gpu", on_gpu.as_ref().unchecked_ref())
            .unwrap();
        on_gpu.forget();

        // State events — also refresh model info on state change
        let set_status_sse = set_status.clone();
        let set_model_info_sse = set_model_info.clone();
        let on_state = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if let Ok(s) = serde_json::from_str::<ServerStatus>(&data) {
                    let is_running = s.state == "running";
                    set_status_sse.set(s);
                    if is_running {
                        let set_mi = set_model_info_sse.clone();
                        wasm_bindgen_futures::spawn_local(async move {
                            gloo_timers::future::sleep(std::time::Duration::from_millis(500)).await;
                            if let Ok(m) = api::fetch_model_info().await {
                                set_mi.set(m);
                            }
                        });
                    } else {
                        set_model_info_sse.set(ModelInfoData::default());
                    }
                }
            }
        });
        es.add_event_listener_with_callback("state", on_state.as_ref().unchecked_ref())
            .unwrap();
        on_state.forget();

        // Log events
        let set_logs_sse = set_logs.clone();
        let on_log = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
            if let Some(data) = e.data().as_string() {
                set_logs_sse.update(|lines| {
                    lines.push(data);
                    if lines.len() > 500 {
                        lines.drain(..lines.len() - 500);
                    }
                });
            }
        });
        es.add_event_listener_with_callback("log", on_log.as_ref().unchecked_ref())
            .unwrap();
        on_log.forget();

        // Connection error
        let set_conn_err = set_connected.clone();
        let on_error = Closure::<dyn FnMut(_)>::new(move |_: web_sys::Event| {
            set_conn_err.set(false);
        });
        es.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();

        // Connection open (handles reconnection after error)
        let set_conn_open = set_connected.clone();
        let on_open = Closure::<dyn FnMut(_)>::new(move |_: web_sys::Event| {
            set_conn_open.set(true);
        });
        es.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        on_open.forget();
    }

    // Periodic agent refresh
    let set_agents_interval = set_agents.clone();
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            gloo_timers::future::sleep(std::time::Duration::from_secs(10)).await;
            if let Ok(a) = api::fetch_agents().await {
                set_agents_interval.set(a);
            }
        }
    });

    // Periodic server stats polling (single loop at App level to avoid accumulation)
    let (server_stats, set_server_stats) = signal(Option::<serde_json::Value>::None);
    {
        let status_for_stats = status.clone();
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                if status_for_stats.get().state == "running" {
                    if let Ok(data) = api::fetch_server_stats().await {
                        if data["available"].as_bool().unwrap_or(false) {
                            set_server_stats.set(Some(data));
                        } else {
                            set_server_stats.set(None);
                        }
                    }
                } else {
                    set_server_stats.set(None);
                }
                gloo_timers::future::sleep(std::time::Duration::from_millis(3000)).await;
            }
        });
    }

    // Keyboard shortcuts
    {
        let set_tab = set_tab.clone();
        let set_toasts_kb = set_toasts.clone();
        let set_profiles_kb = set_profiles.clone();
        let set_agents_kb = set_agents.clone();

        let on_keydown = Closure::<dyn FnMut(_)>::new(move |e: web_sys::KeyboardEvent| {
            // Skip if an input/textarea is focused
            if let Some(doc) = get_window().and_then(|w| w.document()) {
                if let Some(active) = doc.active_element() {
                    let tag = active.tag_name().to_uppercase();
                    if tag == "INPUT" || tag == "TEXTAREA" || tag == "SELECT" {
                        return;
                    }
                }
            }

            let key = e.key();
            match key.as_str() {
                "1" => set_tab.set(Tab::Overview),
                "2" => set_tab.set(Tab::Settings),
                "3" => set_tab.set(Tab::Chat),
                "4" => set_tab.set(Tab::Bench),
                "5" => set_tab.set(Tab::Logs),
                "6" => set_tab.set(Tab::Models),
                "t" => {
                    set_is_light.update(|light| *light = toggle_theme(*light));
                }
                "s" => {
                    let st = set_toasts_kb.clone();
                    let sp = set_profiles_kb.clone();
                    let sa = set_agents_kb.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::start_server(None).await {
                            Ok(resp) => {
                                let msg = resp["message"].as_str().unwrap_or("started").to_string();
                                let success = resp["success"].as_bool().unwrap_or(false);
                                show_toast(st, msg, if success { ToastKind::Success } else { ToastKind::Error });
                            }
                            Err(e) => show_toast(st, format!("start failed: {e}"), ToastKind::Error),
                        }
                        if let Ok(p) = api::fetch_profiles().await { sp.set(p); }
                        if let Ok(a) = api::fetch_agents().await { sa.set(a); }
                    });
                }
                "x" => {
                    let st = set_toasts_kb.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match api::stop_server().await {
                            Ok(resp) => {
                                let msg = resp["message"].as_str().unwrap_or("stopped").to_string();
                                show_toast(st, msg, ToastKind::Success);
                            }
                            Err(e) => show_toast(st, format!("stop failed: {e}"), ToastKind::Error),
                        }
                    });
                }
                _ => {}
            }
        });

        if let Some(doc) = get_window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("keydown", on_keydown.as_ref().unchecked_ref());
            on_keydown.forget();
        }
    }

    let tab_btn = move |t: Tab, label: &'static str, key: &'static str| {
        let class = move || if tab.get() == t { "tab active" } else { "tab" };
        view! {
            <button class=class on:click=move |_| set_tab.set(t)>
                {label}
                <span class="tab-key">{key}</span>
            </button>
        }
    };

    let on_theme_toggle = move |_| {
        set_is_light.update(|light| *light = toggle_theme(*light));
    };

    view! {
        <div class="app">
            <div class="header">
                <h1>"rookery"</h1>
                <span class="subtitle">"local inference command center"</span>
                <span style="margin-left:auto;display:flex;align-items:center;gap:8px">
                    <button class="theme-toggle" on:click=on_theme_toggle>
                        {move || if is_light.get() { "dark" } else { "light" }}
                    </button>
                    <span class={move || if connected.get() { "conn-dot connected" } else { "conn-dot disconnected" }}></span>
                    <span style="font-size:0.75em;color:var(--muted)">
                        {move || if connected.get() { "connected" } else { "disconnected" }}
                    </span>
                </span>
            </div>

            <div class="tab-bar">
                {tab_btn(Tab::Overview, "Overview", "1")}
                {tab_btn(Tab::Settings, "Settings", "2")}
                {tab_btn(Tab::Chat, "Chat", "3")}
                {tab_btn(Tab::Bench, "Bench", "4")}
                {tab_btn(Tab::Logs, "Logs", "5")}
                {tab_btn(Tab::Models, "Models", "6")}
            </div>

            <div class="tab-content">
                {move || match tab.get() {
                    Tab::Overview => view! {
                        <div>
                            <div class="grid">
                                <StatusCard status=status set_profiles=set_profiles set_agents=set_agents set_toasts=set_toasts />
                                <GpuPanel gpu=gpu />
                            </div>
                            <div class="grid">
                                <ModelInfo model_info=model_info />
                                <ServerStats stats=server_stats />
                            </div>
                            <div class="grid">
                                <AgentPanel agents=agents set_agents=set_agents set_toasts=set_toasts />
                            </div>
                        </div>
                    }.into_any(),
                    Tab::Settings => view! {
                        <div>
                            <div class="grid">
                                <ProfileSwitcher profiles=profiles status=status set_profiles=set_profiles set_agents=set_agents set_toasts=set_toasts />
                                <AgentPanel agents=agents set_agents=set_agents set_toasts=set_toasts />
                            </div>
                            <div class="section">
                                <SettingsPanel status=status set_toasts=set_toasts />
                            </div>
                        </div>
                    }.into_any(),
                    Tab::Chat => view! {
                        <div class="section">
                            <ChatPanel set_toasts=set_toasts />
                        </div>
                    }.into_any(),
                    Tab::Bench => view! {
                        <div class="section">
                            <BenchPanel status=status />
                        </div>
                    }.into_any(),
                    Tab::Logs => view! {
                        <div class="section">
                            <LogViewer logs=logs />
                        </div>
                    }.into_any(),
                    Tab::Models => view! {
                        <div class="section">
                            <ModelsPanel set_toasts=set_toasts />
                        </div>
                    }.into_any(),
                }}
            </div>

            <ToastContainer toasts=toasts />
        </div>
    }
}

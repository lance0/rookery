mod api;
mod components;

use components::*;
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentsData {
    pub agents: Vec<AgentInfo>,
    pub configured: Vec<String>,
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

    // Load initial data
    let set_profiles_init = set_profiles.clone();
    let set_agents_init = set_agents.clone();
    let set_logs_init = set_logs.clone();

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

        // State events
        let set_status_sse = set_status.clone();
        let on_state = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
            if let Some(data) = e.data().as_string() {
                if let Ok(s) = serde_json::from_str::<ServerStatus>(&data) {
                    set_status_sse.set(s);
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
        let set_conn = set_connected.clone();
        let on_error = Closure::<dyn FnMut(_)>::new(move |_: web_sys::Event| {
            set_conn.set(false);
        });
        es.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();
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

    view! {
        <div class="app">
            <div class="header">
                <h1>"rookery"</h1>
                <span class="subtitle">"local inference command center"</span>
                <span style="margin-left:auto">
                    <span class={move || if connected.get() { "conn-dot connected" } else { "conn-dot disconnected" }}></span>
                    <span style="font-size:0.75em;color:var(--text-muted)">
                        {move || if connected.get() { "connected" } else { "disconnected" }}
                    </span>
                </span>
            </div>

            <div class="grid">
                <StatusCard status=status set_profiles=set_profiles set_agents=set_agents />
                <GpuPanel gpu=gpu />
            </div>

            <div class="grid">
                <ProfileSwitcher profiles=profiles status=status set_profiles=set_profiles set_agents=set_agents />
                <AgentPanel agents=agents set_agents=set_agents />
            </div>

            <div class="section">
                <BenchPanel status=status />
            </div>

            <div class="section">
                <LogViewer logs=logs />
            </div>
        </div>
    }
}

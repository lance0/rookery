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
    #[serde(default)]
    pub backend: Option<String>,
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
    #[serde(default)]
    pub ctx_size: Option<u32>,
    #[serde(default)]
    pub reasoning_budget: Option<i32>,
    pub default: bool,
    pub estimated_vram_mb: Option<u32>,
    #[serde(default)]
    pub backend: Option<String>,
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
    #[serde(default)]
    pub lifetime_errors: Option<u32>,
    #[serde(default)]
    pub last_restart_reason: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
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
    Agents,
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

fn load_dashboard_data(
    set_profiles: WriteSignal<Vec<ProfileInfo>>,
    set_agents: WriteSignal<AgentsData>,
    set_logs: WriteSignal<Vec<String>>,
    set_model_info: WriteSignal<ModelInfoData>,
    set_loading: WriteSignal<bool>,
    set_load_error: WriteSignal<Option<String>>,
    set_auth_required: WriteSignal<bool>,
) {
    set_loading.set(true);
    set_load_error.set(None);

    wasm_bindgen_futures::spawn_local(async move {
        let mut any_ok = false;
        let mut unauthorized = false;
        let mut first_error = None::<String>;

        match api::fetch_profiles().await {
            Ok(profiles) => {
                set_profiles.set(profiles);
                any_ok = true;
            }
            Err(error) => {
                unauthorized |= api::is_unauthorized(&error);
                first_error.get_or_insert(error);
            }
        }

        match api::fetch_agents().await {
            Ok(agents) => {
                set_agents.set(agents);
                any_ok = true;
            }
            Err(error) => {
                unauthorized |= api::is_unauthorized(&error);
                first_error.get_or_insert(error);
            }
        }

        match api::fetch_logs(100).await {
            Ok(logs) => {
                set_logs.set(logs);
                any_ok = true;
            }
            Err(error) => {
                unauthorized |= api::is_unauthorized(&error);
                first_error.get_or_insert(error);
            }
        }

        match api::fetch_model_info().await {
            Ok(model_info) => {
                set_model_info.set(model_info);
                any_ok = true;
            }
            Err(error) => {
                unauthorized |= api::is_unauthorized(&error);
                first_error.get_or_insert(error);
            }
        }

        if unauthorized {
            set_auth_required.set(true);
            set_load_error.set(None);
            set_loading.set(false);
            return;
        }

        if !any_ok {
            set_load_error.set(Some(
                first_error.unwrap_or_else(|| "failed to connect to rookeryd".into()),
            ));
        }

        set_loading.set(false);
    });
}

fn connect_sse(
    event_source: ReadSignal<Option<web_sys::EventSource>>,
    set_event_source: WriteSignal<Option<web_sys::EventSource>>,
    set_connected: WriteSignal<bool>,
    set_gpu: WriteSignal<GpuData>,
    set_status: WriteSignal<ServerStatus>,
    set_model_info: WriteSignal<ModelInfoData>,
    set_logs: WriteSignal<Vec<String>>,
) {
    if let Some(existing) = event_source.get_untracked() {
        existing.close();
    }
    set_event_source.set(None);

    let Ok(es) = web_sys::EventSource::new(&api::events_url()) else {
        set_connected.set(false);
        return;
    };

    set_connected.set(true);

    let set_gpu_sse = set_gpu;
    let on_gpu = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
        if let Some(data) = e.data().as_string()
            && let Ok(stats) = serde_json::from_str::<GpuData>(&data)
        {
            set_gpu_sse.set(stats);
        }
    });
    let _ = es.add_event_listener_with_callback("gpu", on_gpu.as_ref().unchecked_ref());
    on_gpu.forget();

    let set_status_sse = set_status;
    let set_model_info_sse = set_model_info;
    let on_state = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
        if let Some(data) = e.data().as_string()
            && let Ok(status) = serde_json::from_str::<ServerStatus>(&data)
        {
            let is_running = status.state == "running";
            set_status_sse.set(status);
            if is_running {
                let set_model_info_retry = set_model_info_sse;
                wasm_bindgen_futures::spawn_local(async move {
                    gloo_timers::future::sleep(std::time::Duration::from_millis(500)).await;
                    if let Ok(model_info) = api::fetch_model_info().await {
                        set_model_info_retry.set(model_info);
                    }
                });
            } else {
                set_model_info_sse.set(ModelInfoData::default());
            }
        }
    });
    let _ = es.add_event_listener_with_callback("state", on_state.as_ref().unchecked_ref());
    on_state.forget();

    let set_logs_sse = set_logs;
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
    let _ = es.add_event_listener_with_callback("log", on_log.as_ref().unchecked_ref());
    on_log.forget();

    let set_conn_err = set_connected;
    let on_error = Closure::<dyn FnMut(_)>::new(move |_: web_sys::Event| {
        set_conn_err.set(false);
    });
    es.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();

    let set_conn_open = set_connected;
    let on_open = Closure::<dyn FnMut(_)>::new(move |_: web_sys::Event| {
        set_conn_open.set(true);
    });
    es.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    on_open.forget();

    set_event_source.set(Some(es));
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

    let (loading, set_loading) = signal(true);
    let (load_error, set_load_error) = signal(Option::<String>::None);
    let (auth_required, set_auth_required) = signal(false);
    let (auth_input, set_auth_input) = signal(api::get_api_key().unwrap_or_default());
    let (auth_error, set_auth_error) = signal(Option::<String>::None);
    let (has_api_key, set_has_api_key) = signal(api::get_api_key().is_some());
    let (event_source, set_event_source) = signal(Option::<web_sys::EventSource>::None);
    let (server_stats, set_server_stats) = signal(Option::<serde_json::Value>::None);
    let (releases, set_releases) = signal(Option::<serde_json::Value>::None);

    load_dashboard_data(
        set_profiles,
        set_agents,
        set_logs,
        set_model_info,
        set_loading,
        set_load_error,
        set_auth_required,
    );
    connect_sse(
        event_source,
        set_event_source,
        set_connected,
        set_gpu,
        set_status,
        set_model_info,
        set_logs,
    );

    {
        let set_auth_required_evt = set_auth_required;
        let set_connected_evt = set_connected;
        let event_source_evt = event_source;
        let set_event_source_evt = set_event_source;
        let set_auth_error_evt = set_auth_error;
        let set_loading_evt = set_loading;

        let on_auth_required = Closure::<dyn FnMut(_)>::new(move |_: web_sys::Event| {
            if let Some(existing) = event_source_evt.get_untracked() {
                existing.close();
            }
            set_event_source_evt.set(None);
            set_connected_evt.set(false);
            set_auth_error_evt.set(Some("Enter the API key to continue.".into()));
            set_loading_evt.set(false);
            set_auth_required_evt.set(true);
        });

        if let Some(window) = get_window() {
            let _ = window.add_event_listener_with_callback(
                "rookery-auth-required",
                on_auth_required.as_ref().unchecked_ref(),
            );
            on_auth_required.forget();
        }
    }

    // Periodic agent refresh
    let set_agents_interval = set_agents.clone();
    let auth_required_agents = auth_required;
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            gloo_timers::future::sleep(std::time::Duration::from_secs(10)).await;
            if auth_required_agents.get() {
                continue;
            }
            if let Ok(a) = api::fetch_agents().await {
                set_agents_interval.set(a);
            }
        }
    });

    // Periodic server stats polling (single loop at App level to avoid accumulation)
    {
        let status_for_stats = status.clone();
        let auth_required_stats = auth_required;
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                if auth_required_stats.get() {
                    set_server_stats.set(None);
                } else if status_for_stats.get().state == "running" {
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

    // Periodic upstream release polling (every 5 minutes, daemon caches GitHub calls)
    {
        let auth_required_releases = auth_required;
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                if !auth_required_releases.get() {
                    if let Ok(data) = api::fetch_releases().await {
                        set_releases.set(Some(data));
                    }
                }
                gloo_timers::future::sleep(std::time::Duration::from_secs(300)).await;
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
                "3" => set_tab.set(Tab::Agents),
                "4" => set_tab.set(Tab::Chat),
                "5" => set_tab.set(Tab::Bench),
                "6" => set_tab.set(Tab::Logs),
                "7" => set_tab.set(Tab::Models),
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

    let on_logout = move |_| {
        let _ = api::clear_api_key();
        if let Some(existing) = event_source.get_untracked() {
            existing.close();
        }
        set_event_source.set(None);
        set_connected.set(false);
        set_has_api_key.set(false);
        set_auth_input.set(String::new());
        set_auth_error.set(Some("API key cleared.".into()));
        set_auth_required.set(true);
    };

    let on_auth_submit = move |_| {
        let key = auth_input.get_untracked();
        let set_auth_error_submit = set_auth_error;
        let set_auth_required_submit = set_auth_required;
        let set_has_api_key_submit = set_has_api_key;
        let set_profiles_submit = set_profiles;
        let set_agents_submit = set_agents;
        let set_logs_submit = set_logs;
        let set_model_info_submit = set_model_info;
        let set_loading_submit = set_loading;
        let set_load_error_submit = set_load_error;
        let event_source_submit = event_source;
        let set_event_source_submit = set_event_source;
        let set_connected_submit = set_connected;
        let set_gpu_submit = set_gpu;
        let set_status_submit = set_status;

        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = api::set_api_key(&key) {
                set_auth_error_submit.set(Some(error));
                return;
            }

            match api::fetch_profiles().await {
                Ok(profiles) => {
                    set_profiles_submit.set(profiles);
                    set_has_api_key_submit.set(true);
                    set_auth_required_submit.set(false);
                    set_auth_error_submit.set(None);
                    load_dashboard_data(
                        set_profiles_submit,
                        set_agents_submit,
                        set_logs_submit,
                        set_model_info_submit,
                        set_loading_submit,
                        set_load_error_submit,
                        set_auth_required_submit,
                    );
                    connect_sse(
                        event_source_submit,
                        set_event_source_submit,
                        set_connected_submit,
                        set_gpu_submit,
                        set_status_submit,
                        set_model_info_submit,
                        set_logs_submit,
                    );
                }
                Err(error) if api::is_unauthorized(&error) => {
                    set_auth_error_submit.set(Some("Invalid API key.".into()));
                    set_auth_required_submit.set(true);
                }
                Err(error) => {
                    set_auth_error_submit.set(Some(format!("failed to connect: {error}")));
                }
            }
        });
    };

    view! {
        <div class="app">
            <div class="header">
                <h1>"rookery"</h1>
                <span class="subtitle">"local inference command center"</span>
                <span style="margin-left:auto;display:flex;align-items:center;gap:8px">
                    <a class="theme-toggle" href="/metrics" target="_blank" style="text-decoration:none">"metrics"</a>
                    {move || {
                        if has_api_key.get() {
                            view! {
                                <button class="theme-toggle" on:click=on_logout>
                                    "logout"
                                </button>
                            }.into_any()
                        } else {
                            view! { <span></span> }.into_any()
                        }
                    }}
                    <button class="theme-toggle" on:click=on_theme_toggle>
                        {move || if is_light.get() { "dark" } else { "light" }}
                    </button>
                    <span class={move || if connected.get() { "conn-dot connected" } else { "conn-dot disconnected" }}></span>
                    <span style="font-size:0.75em;color:var(--muted)">
                        {move || if connected.get() { "connected" } else { "disconnected" }}
                    </span>
                </span>
            </div>

            {move || {
                if auth_required.get() {
                    return view! {
                        <div class="section">
                            <div class="card" style="max-width:520px;margin:40px auto">
                                <h2>"API Key Required"</h2>
                                <p style="color:var(--muted);margin-bottom:16px">
                                    "This rookery daemon is protected. Enter the configured API key to load the dashboard."
                                </p>
                                <input
                                    class="input"
                                    type="password"
                                    placeholder="rky-..."
                                    prop:value=move || auth_input.get()
                                    on:input=move |ev| set_auth_input.set(event_target_value(&ev))
                                />
                                <div style="display:flex;gap:12px;align-items:center;margin-top:16px">
                                    <button class="btn" on:click=on_auth_submit>"Unlock"</button>
                                    {move || {
                                        auth_error
                                            .get()
                                            .map(|error| view! { <span class="agent-errors">{error}</span> })
                                    }}
                                </div>
                            </div>
                        </div>
                    }
                    .into_any();
                }

                if let Some(err) = load_error.get() {
                    return view! {
                        <div class="load-error">
                            <span>{err}</span>
                            <button class="btn" on:click=move |_| {
                                web_sys::window().and_then(|w| w.location().reload().ok());
                            }>"Retry"</button>
                        </div>
                    }.into_any();
                }
                view! { <span></span> }.into_any()
            }}

            {move || {
                if auth_required.get() {
                    return view! { <span></span> }.into_any();
                }

                view! {
                    <>
                        <div class="tab-bar">
                            {tab_btn(Tab::Overview, "Overview", "1")}
                            {tab_btn(Tab::Settings, "Settings", "2")}
                            {tab_btn(Tab::Agents, "Agents", "3")}
                            {tab_btn(Tab::Chat, "Chat", "4")}
                            {tab_btn(Tab::Bench, "Bench", "5")}
                            {tab_btn(Tab::Logs, "Logs", "6")}
                            {tab_btn(Tab::Models, "Models", "7")}
                        </div>

                        <div class="tab-content">
                            {move || {
                                if loading.get() {
                                    return view! { <div class="loading">"loading..."</div> }.into_any();
                                }
                                match tab.get() {
                                    Tab::Overview => view! {
                                        <div>
                                            <div class="grid">
                                                <StatusCard status=status set_profiles=set_profiles set_agents=set_agents set_toasts=set_toasts />
                                                <GpuPanel gpu=gpu />
                                            </div>
                                            <div class="grid">
                                                <ModelInfo model_info=model_info />
                                                <ServerStats stats=server_stats status=status />
                                            </div>
                                            <div class="grid">
                                                <AgentSummary agents=agents set_tab=set_tab />
                                                <UpdateBanner releases=releases />
                                            </div>
                                        </div>
                                    }.into_any(),
                                    Tab::Settings => view! {
                                        <div>
                                            <div class="section">
                                                <ProfileSwitcher profiles=profiles status=status set_profiles=set_profiles set_agents=set_agents set_toasts=set_toasts />
                                            </div>
                                            <div class="section">
                                                <SettingsPanel status=status set_toasts=set_toasts />
                                            </div>
                                        </div>
                                    }.into_any(),
                                    Tab::Agents => view! {
                                        <div class="section">
                                            <AgentsTab agents=agents set_agents=set_agents logs=logs set_toasts=set_toasts />
                                        </div>
                                    }.into_any(),
                                    Tab::Chat => view! {
                                        <div class="section">
                                            <ChatPanel set_toasts=set_toasts />
                                        </div>
                                    }.into_any(),
                                    Tab::Bench => view! {
                                        <div class="section">
                                            <BenchPanel status=status set_toasts=set_toasts />
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
                                }.into_any()
                            }}
                        </div>
                    </>
                }.into_any()
            }}

            <ToastContainer toasts=toasts />
        </div>
    }
}

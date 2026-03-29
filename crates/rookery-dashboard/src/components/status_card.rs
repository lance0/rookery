use leptos::prelude::*;
use crate::{ServerStatus, ProfileInfo, AgentsData, api};
use crate::components::toast::{Toast, ToastKind, show_toast};

fn spawn_refresh_lists(
    set_profiles: WriteSignal<Vec<ProfileInfo>>,
    set_agents: WriteSignal<AgentsData>,
) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(p) = api::fetch_profiles().await {
            set_profiles.set(p);
        }
        if let Ok(a) = api::fetch_agents().await {
            set_agents.set(a);
        }
    });
}

#[component]
pub fn StatusCard(
    status: ReadSignal<ServerStatus>,
    set_profiles: WriteSignal<Vec<ProfileInfo>>,
    set_agents: WriteSignal<AgentsData>,
    set_toasts: WriteSignal<Vec<Toast>>,
) -> impl IntoView {
    let state_class = move || {
        let s = status.get();
        let base = s.state.split(':').next().unwrap_or("stopped").trim().to_string();
        format!("badge {base}")
    };

    let state_text = move || status.get().state.clone();
    let profile_text = move || status.get().profile.clone().unwrap_or_else(|| "—".into());
    let can_start = move || matches!(status.get().state.as_str(), "stopped" | "failed");
    let can_stop = move || status.get().state != "stopped";
    let can_sleep = move || status.get().state == "running";
    let can_wake = move || status.get().state == "sleeping";

    let backend_badge = move || {
        let s = status.get();
        if s.state != "running" {
            return None;
        }
        s.backend.as_ref().map(|b| {
            let label = match b.as_str() {
                "vllm" => "vLLM",
                "llama-server" => "llama.cpp",
                other => other,
            };
            label.to_string()
        })
    };
    let pid_port = move || {
        let s = status.get();
        match (s.pid, s.port) {
            (Some(pid), Some(port)) => format!("{pid} / :{port}"),
            _ => "—".into(),
        }
    };
    let uptime = move || {
        status.get().uptime_secs.map(|secs| {
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            format!("{h}h {m}m {s}s")
        }).unwrap_or_else(|| "—".into())
    };

    let on_start = move |_| {
        let set_profiles = set_profiles.clone();
        let set_agents = set_agents.clone();
        let set_toasts = set_toasts.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match api::start_server(None).await {
                Ok(resp) => {
                    let msg = resp["message"].as_str().unwrap_or("started").to_string();
                    let success = resp["success"].as_bool().unwrap_or(false);
                    show_toast(set_toasts, msg, if success { ToastKind::Success } else { ToastKind::Error });
                }
                Err(e) => show_toast(set_toasts, format!("start failed: {e}"), ToastKind::Error),
            }
        });
        spawn_refresh_lists(set_profiles, set_agents);
    };

    let on_stop = move |_| {
        let set_profiles = set_profiles.clone();
        let set_agents = set_agents.clone();
        let set_toasts = set_toasts.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match api::stop_server().await {
                Ok(resp) => {
                    let msg = resp["message"].as_str().unwrap_or("stopped").to_string();
                    show_toast(set_toasts, msg, ToastKind::Success);
                }
                Err(e) => show_toast(set_toasts, format!("stop failed: {e}"), ToastKind::Error),
            }
        });
        spawn_refresh_lists(set_profiles, set_agents);
    };

    let on_sleep = move |_| {
        let set_profiles = set_profiles.clone();
        let set_agents = set_agents.clone();
        let set_toasts = set_toasts.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match api::sleep_server().await {
                Ok(resp) => {
                    let msg = resp["message"].as_str().unwrap_or("server sleeping").to_string();
                    let success = resp["success"].as_bool().unwrap_or(false);
                    show_toast(
                        set_toasts,
                        msg,
                        if success { ToastKind::Success } else { ToastKind::Error },
                    );
                }
                Err(e) => show_toast(set_toasts, format!("sleep failed: {e}"), ToastKind::Error),
            }
        });
        spawn_refresh_lists(set_profiles, set_agents);
    };

    let on_wake = move |_| {
        let set_profiles = set_profiles.clone();
        let set_agents = set_agents.clone();
        let set_toasts = set_toasts.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match api::wake_server().await {
                Ok(resp) => {
                    let msg = resp["message"].as_str().unwrap_or("server woke").to_string();
                    let success = resp["success"].as_bool().unwrap_or(false);
                    show_toast(
                        set_toasts,
                        msg,
                        if success { ToastKind::Success } else { ToastKind::Error },
                    );
                }
                Err(e) => show_toast(set_toasts, format!("wake failed: {e}"), ToastKind::Error),
            }
        });
        spawn_refresh_lists(set_profiles, set_agents);
    };

    view! {
        <div class="card">
            <h2>"Server"</h2>
            <div class="stat">
                <div class="stat-label">"State"</div>
                <div><span class=state_class>{state_text}</span></div>
            </div>
            <div class="stat">
                <div class="stat-label">"Profile"</div>
                <div class="stat-value">
                    {profile_text}
                    {move || backend_badge().map(|label| view! {
                        <span class="badge backend">{label}</span>
                    })}
                </div>
            </div>
            <div class="stat">
                <div class="stat-label">"PID / Port"</div>
                <div class="stat-value mono">{pid_port}</div>
            </div>
            <div class="stat">
                <div class="stat-label">"Uptime"</div>
                <div class="stat-value mono">{uptime}</div>
            </div>
            <div class="btn-row">
                <button class="btn" on:click=on_start disabled=move || !can_start()>"Start"</button>
                <button class="btn" on:click=on_wake disabled=move || !can_wake()>"Wake"</button>
                <button class="btn" on:click=on_sleep disabled=move || !can_sleep()>"Sleep"</button>
                <button class="btn danger" on:click=on_stop disabled=move || !can_stop()>"Stop"</button>
            </div>
        </div>
    }
}

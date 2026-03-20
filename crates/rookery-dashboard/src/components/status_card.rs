use leptos::prelude::*;
use crate::{ServerStatus, ProfileInfo, AgentsData, api};

#[component]
pub fn StatusCard(
    status: ReadSignal<ServerStatus>,
    set_profiles: WriteSignal<Vec<ProfileInfo>>,
    set_agents: WriteSignal<AgentsData>,
) -> impl IntoView {
    let state_class = move || {
        let s = status.get();
        let base = s.state.split(':').next().unwrap_or("stopped").trim().to_string();
        format!("badge {base}")
    };

    let state_text = move || status.get().state.clone();
    let profile_text = move || status.get().profile.clone().unwrap_or_else(|| "—".into());
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
        wasm_bindgen_futures::spawn_local(async move {
            let _ = api::start_server(None).await;
            if let Ok(p) = api::fetch_profiles().await { set_profiles.set(p); }
            if let Ok(a) = api::fetch_agents().await { set_agents.set(a); }
        });
    };

    let on_stop = move |_| {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = api::stop_server().await;
        });
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
                <div class="stat-value">{profile_text}</div>
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
                <button class="btn" on:click=on_start>"Start"</button>
                <button class="btn danger" on:click=on_stop>"Stop"</button>
            </div>
        </div>
    }
}

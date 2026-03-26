use leptos::prelude::*;
use crate::{AgentsData, AgentInfo, api};
use crate::components::toast::{Toast, ToastKind, show_toast};

fn format_uptime(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

#[component]
pub fn AgentPanel(
    agents: ReadSignal<AgentsData>,
    set_agents: WriteSignal<AgentsData>,
    set_toasts: WriteSignal<Vec<Toast>>,
) -> impl IntoView {
    view! {
        <div class="card">
            <h2>"Agents"</h2>
            {move || {
                let data = agents.get();
                if data.configured.is_empty() {
                    return view! { <div class="empty">"no agents configured"</div> }.into_any();
                }

                let running_map: std::collections::HashMap<String, AgentInfo> = data.agents.iter()
                    .filter(|a| a.status == serde_json::json!("running"))
                    .map(|a| (a.name.clone(), a.clone()))
                    .collect();

                view! {
                    <div>
                        {data.configured.into_iter().map(|name| {
                            let agent = running_map.get(&name);
                            let is_running = agent.is_some();
                            let version = agent.and_then(|a| a.version.clone());
                            let uptime = agent.and_then(|a| a.uptime_secs);
                            let restarts = agent.and_then(|a| a.total_restarts).unwrap_or(0);
                            let errors = agent.and_then(|a| a.error_count).unwrap_or(0);
                            let dot_class = if is_running { "agent-dot running" } else { "agent-dot stopped" };
                            let btn_text = if is_running { "Stop" } else { "Start" };

                            let click_name = name.clone();
                            let running = is_running;
                            let set_agents = set_agents.clone();
                            let set_toasts = set_toasts.clone();
                            let on_click = move |_| {
                                let n = click_name.clone();
                                let sa = set_agents.clone();
                                let st = set_toasts.clone();
                                wasm_bindgen_futures::spawn_local(async move {
                                    let result = if running {
                                        api::stop_agent(&n).await
                                    } else {
                                        api::start_agent(&n).await
                                    };
                                    match result {
                                        Ok(resp) => {
                                            let msg = resp["message"].as_str().unwrap_or("done").to_string();
                                            let success = resp["success"].as_bool().unwrap_or(false);
                                            show_toast(st, msg, if success { ToastKind::Success } else { ToastKind::Error });
                                        }
                                        Err(e) => show_toast(st, format!("failed: {e}"), ToastKind::Error),
                                    }
                                    gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;
                                    if let Ok(a) = api::fetch_agents().await { sa.set(a); }
                                });
                            };

                            view! {
                                <div class="agent-row">
                                    <div class=dot_class></div>
                                    <span class="agent-name">{name}</span>
                                    {version.map(|v| view! { <span class="agent-version">"v"{v}</span> })}
                                    {uptime.map(|s| view! { <span class="agent-uptime">{format_uptime(s)}</span> })}
                                    {(restarts > 0).then(|| view! {
                                        <span class="agent-restarts">{format!("{restarts} restart{}", if restarts == 1 { "" } else { "s" })}</span>
                                    })}
                                    {(errors > 0).then(|| view! {
                                        <span class="agent-errors">{format!("{errors} err{}", if errors == 1 { "" } else { "s" })}</span>
                                    })}
                                    <button class="btn" on:click=on_click>{btn_text}</button>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                }.into_any()
            }}
        </div>
    }
}

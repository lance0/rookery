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
pub fn AgentsTab(
    agents: ReadSignal<AgentsData>,
    set_agents: WriteSignal<AgentsData>,
    logs: ReadSignal<Vec<String>>,
    set_toasts: WriteSignal<Vec<Toast>>,
) -> impl IntoView {
    let (updating_agent, set_updating_agent) = signal(Option::<String>::None);
    let (health_details, set_health_details) = signal(std::collections::HashMap::<String, serde_json::Value>::new());

    // Fetch health details for all running agents whenever agents signal changes
    Effect::new(move |_| {
        let data = agents.get();
        let running_names: Vec<String> = data.agents.iter()
            .filter(|a| a.status == serde_json::json!("running"))
            .map(|a| a.name.clone())
            .collect();

        wasm_bindgen_futures::spawn_local(async move {
            let mut details = std::collections::HashMap::new();
            for name in running_names {
                if let Ok(health) = api::fetch_agent_health(&name).await {
                    details.insert(name, health);
                }
            }
            set_health_details.set(details);
        });
    });

    let agent_log_ref = NodeRef::<leptos::html::Div>::new();

    // Auto-scroll for agent logs
    Effect::new(move |_| {
        let _lines = logs.get();
        if let Some(el) = agent_log_ref.get() {
            let el: &web_sys::HtmlElement = &el;
            let at_bottom = el.scroll_top() + el.client_height() >= el.scroll_height() - 50;
            if at_bottom {
                el.set_scroll_top(el.scroll_height());
            }
        }
    });

    view! {
        <div>
            {move || {
                let data = agents.get();
                let details = health_details.get();

                if data.configured.is_empty() {
                    return view! { <div class="card"><div class="empty">"no agents configured"</div></div> }.into_any();
                }

                let running_map: std::collections::HashMap<String, AgentInfo> = data.agents.iter()
                    .filter(|a| a.status == serde_json::json!("running"))
                    .map(|a| (a.name.clone(), a.clone()))
                    .collect();

                view! {
                    <div class="agent-cards">
                        {data.configured.into_iter().map(|name| {
                            let agent = running_map.get(&name);
                            let detail = details.get(&name);
                            let is_running = agent.is_some();
                            let version = agent.and_then(|a| a.version.clone());
                            let pid = agent.map(|a| a.pid);
                            let uptime = agent.and_then(|a| a.uptime_secs);
                            let restarts = agent.and_then(|a| a.total_restarts).unwrap_or(0);
                            let errors = agent.and_then(|a| a.error_count).unwrap_or(0);
                            let lifetime = agent.and_then(|a| a.lifetime_errors).unwrap_or(0);
                            let last_reason = agent.and_then(|a| a.last_restart_reason.clone());
                            let started_at = agent.and_then(|a| a.started_at.clone());

                            // Watchdog detail from health API
                            let watchdog_state = detail
                                .and_then(|d| d["watchdog"]["state"].as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let consecutive_crashes = detail
                                .and_then(|d| d["watchdog"]["consecutive_crashes"].as_u64())
                                .unwrap_or(0);
                            let backoff_secs = detail
                                .and_then(|d| d["watchdog"]["backoff_secs"].as_u64())
                                .unwrap_or(0);
                            let dep_ports: Vec<(u16, bool)> = detail
                                .and_then(|d| d["dependency_ports"].as_array())
                                .map(|arr| arr.iter().filter_map(|p| {
                                    Some((p["port"].as_u64()? as u16, p["up"].as_bool()?))
                                }).collect())
                                .unwrap_or_default();
                            let last_restart_at = detail
                                .and_then(|d| d["last_restart_at"].as_str())
                                .map(|s| s.to_string());

                            let dot_class = if is_running { "agent-dot running" } else { "agent-dot stopped" };
                            let status_text = if is_running { "running" } else { "stopped" };
                            let btn_text = if is_running { "Stop" } else { "Start" };
                            let display_name = name.clone();
                            let updating_name = name.clone();
                            let updating_name_text = name.clone();

                            let click_name = name.clone();
                            let running = is_running;
                            let set_agents_click = set_agents.clone();
                            let set_toasts_click = set_toasts.clone();
                            let on_click = move |_| {
                                let n = click_name.clone();
                                let sa = set_agents_click.clone();
                                let st = set_toasts_click.clone();
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

                            let update_name = name.clone();
                            let set_agents_update = set_agents.clone();
                            let set_toasts_update = set_toasts.clone();
                            let on_update = move |_| {
                                let n = update_name.clone();
                                let sa = set_agents_update.clone();
                                let st = set_toasts_update.clone();
                                set_updating_agent.set(Some(n.clone()));
                                wasm_bindgen_futures::spawn_local(async move {
                                    match api::update_agent(&n).await {
                                        Ok(resp) => {
                                            let msg = resp["message"].as_str().unwrap_or("updated").to_string();
                                            let success = resp["success"].as_bool().unwrap_or(false);
                                            show_toast(st, msg, if success { ToastKind::Success } else { ToastKind::Error });
                                        }
                                        Err(e) => show_toast(st, format!("update failed: {e}"), ToastKind::Error),
                                    }
                                    if let Ok(a) = api::fetch_agents().await { sa.set(a); }
                                    set_updating_agent.set(None);
                                });
                            };

                            let watchdog_class = match watchdog_state.as_str() {
                                "healthy" => "watchdog-badge healthy",
                                "backing_off" => "watchdog-badge backing-off",
                                _ => "watchdog-badge idle",
                            };

                            view! {
                                <div class="agent-card card">
                                    <div class="agent-card-header">
                                        <span class=dot_class></span>
                                        <span class="agent-card-name">{display_name}</span>
                                        <span class="agent-card-status">{status_text}</span>
                                        {version.map(|v| view! { <span class="agent-version">"v"{v}</span> })}
                                    </div>
                                    <div class="agent-card-details">
                                        {pid.map(|p| view! {
                                            <div class="agent-detail">
                                                <span class="agent-detail-label">"PID"</span>
                                                <span class="agent-detail-value">{p.to_string()}</span>
                                            </div>
                                        })}
                                        {uptime.map(|s| view! {
                                            <div class="agent-detail">
                                                <span class="agent-detail-label">"Uptime"</span>
                                                <span class="agent-detail-value">{format_uptime(s)}</span>
                                            </div>
                                        })}
                                        {started_at.map(|t| view! {
                                            <div class="agent-detail">
                                                <span class="agent-detail-label">"Started"</span>
                                                <span class="agent-detail-value">{t}</span>
                                            </div>
                                        })}
                                        <div class="agent-detail">
                                            <span class="agent-detail-label">"Restarts"</span>
                                            <span class="agent-detail-value">
                                                {restarts.to_string()}
                                                {last_reason.map(|r| format!(" (last: {r})"))}
                                            </span>
                                        </div>
                                        {last_restart_at.map(|t| view! {
                                            <div class="agent-detail">
                                                <span class="agent-detail-label">"Last restart"</span>
                                                <span class="agent-detail-value">{t}</span>
                                            </div>
                                        })}
                                        <div class="agent-detail">
                                            <span class="agent-detail-label">"Errors"</span>
                                            <span class={if errors > 0 { "agent-detail-value agent-errors" } else { "agent-detail-value" }}>
                                                {if lifetime > errors {
                                                    format!("{errors} ({lifetime} lifetime)")
                                                } else {
                                                    errors.to_string()
                                                }}
                                            </span>
                                        </div>
                                    </div>
                                    // Watchdog section
                                    {is_running.then(|| view! {
                                        <div class="agent-card-watchdog">
                                            <div class="agent-detail">
                                                <span class="agent-detail-label">"Watchdog"</span>
                                                <span class=watchdog_class>{watchdog_state.clone()}</span>
                                            </div>
                                            {(consecutive_crashes > 0).then(|| view! {
                                                <div class="agent-detail">
                                                    <span class="agent-detail-label">"Crashes"</span>
                                                    <span class="agent-detail-value agent-errors">
                                                        {format!("{consecutive_crashes} (backoff: {backoff_secs}s)")}
                                                    </span>
                                                </div>
                                            })}
                                            {(!dep_ports.is_empty()).then(|| {
                                                let ports_view: Vec<_> = dep_ports.iter().map(|(port, up)| {
                                                    let class = if *up { "dep-port up" } else { "dep-port down" };
                                                    let label = if *up { "up" } else { "down" };
                                                    view! { <span class=class>{format!(":{port} {label}")}</span> }
                                                }).collect();
                                                view! {
                                                    <div class="agent-detail">
                                                        <span class="agent-detail-label">"Deps"</span>
                                                        <span class="agent-detail-value">{ports_view}</span>
                                                    </div>
                                                }
                                            })}
                                        </div>
                                    })}
                                    <div class="agent-card-actions">
                                        <button
                                            class="btn"
                                            on:click=on_update
                                            disabled=move || updating_agent.get().as_deref() == Some(updating_name.as_str())
                                        >
                                            {move || {
                                                if updating_agent.get().as_deref() == Some(updating_name_text.as_str()) {
                                                    "Updating..."
                                                } else {
                                                    "Update"
                                                }
                                            }}
                                        </button>
                                        <button class="btn" on:click=on_click>{btn_text}</button>
                                    </div>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                }.into_any()
            }}

            <div class="card" style="margin-top:10px">
                <h2>"Agent Logs"</h2>
                <div class="log-viewer" node_ref=agent_log_ref>
                    {move || {
                        logs.get().into_iter()
                            .filter(|line| line.contains("[agent:"))
                            .map(|line| {
                                view! { <div class="log-line">{line}</div> }
                            })
                            .collect_view()
                    }}
                </div>
            </div>
        </div>
    }
}

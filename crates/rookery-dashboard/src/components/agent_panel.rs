use leptos::prelude::*;
use crate::{AgentsData, api};
use crate::components::toast::{Toast, ToastKind, show_toast};

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

                let running_map: std::collections::HashMap<String, Option<String>> = data.agents.iter()
                    .filter(|a| a.status == serde_json::json!("running"))
                    .map(|a| (a.name.clone(), a.version.clone()))
                    .collect();

                view! {
                    <div>
                        {data.configured.into_iter().map(|name| {
                            let is_running = running_map.contains_key(&name);
                            let version = running_map.get(&name).and_then(|v| v.clone());
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

use leptos::prelude::*;
use crate::{AgentsData, api};

#[component]
pub fn AgentPanel(
    agents: ReadSignal<AgentsData>,
    set_agents: WriteSignal<AgentsData>,
) -> impl IntoView {
    view! {
        <div class="card">
            <h2>"Agents"</h2>
            {move || {
                let data = agents.get();
                if data.configured.is_empty() {
                    return view! { <div class="empty">"no agents configured"</div> }.into_any();
                }

                let running_names: std::collections::HashSet<String> = data.agents.iter()
                    .filter(|a| a.status == serde_json::json!("running"))
                    .map(|a| a.name.clone())
                    .collect();

                view! {
                    <div>
                        {data.configured.into_iter().map(|name| {
                            let is_running = running_names.contains(&name);
                            let dot_class = if is_running { "agent-dot running" } else { "agent-dot stopped" };
                            let btn_text = if is_running { "Stop" } else { "Start" };

                            let click_name = name.clone();
                            let running = is_running;
                            let set_agents = set_agents.clone();
                            let on_click = move |_| {
                                let n = click_name.clone();
                                let sa = set_agents.clone();
                                wasm_bindgen_futures::spawn_local(async move {
                                    if running {
                                        let _ = api::stop_agent(&n).await;
                                    } else {
                                        let _ = api::start_agent(&n).await;
                                    }
                                    gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;
                                    if let Ok(a) = api::fetch_agents().await { sa.set(a); }
                                });
                            };

                            view! {
                                <div class="agent-row">
                                    <div class=dot_class></div>
                                    <span class="agent-name">{name}</span>
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

use crate::{AgentsData, Tab};
use leptos::prelude::*;

#[component]
pub fn AgentSummary(agents: ReadSignal<AgentsData>, set_tab: WriteSignal<Tab>) -> impl IntoView {
    view! {
        // The card stays click-to-open for the mouse; the button is the keyboard and
        // screen-reader path (a bare on:click div is reachable by neither).
        <div class="card agent-summary" on:click=move |_| set_tab.set(Tab::Agents) style="cursor:pointer">
            <h2>
                "Agents"
                <button
                    class="agent-summary-link"
                    aria-label="Agent details"
                    on:click=move |_| set_tab.set(Tab::Agents)
                >
                    "details \u{2192}"
                </button>
            </h2>
            {move || {
                let data = agents.get();
                if data.configured.is_empty() {
                    return view! { <div class="empty">"no agents configured"</div> }.into_any();
                }

                let running_names: std::collections::HashSet<String> = data.agents.iter()
                    .filter(|a| a.status == serde_json::json!("running"))
                    .map(|a| a.name.clone())
                    .collect();

                let unhealthy: usize = data.agents.iter()
                    .filter(|a| a.error_count.unwrap_or(0) > 0)
                    .count();

                view! {
                    <div class="agent-pills">
                        {data.configured.into_iter().map(|name| {
                            let is_running = running_names.contains(&name);
                            let dot_class = if is_running { "dot running" } else { "dot stopped" };
                            let status_text = if is_running { "running" } else { "stopped" };
                            view! {
                                <span class="agent-pill">
                                    <span class=dot_class></span>
                                    {name}
                                    " \u{00b7} "
                                    <span class="agent-pill-status">{status_text}</span>
                                </span>
                            }
                        }).collect_view()}
                        {(unhealthy > 0).then(|| view! {
                            <span class="agent-pill unhealthy">
                                {format!("{unhealthy} unhealthy")}
                            </span>
                        })}
                    </div>
                }.into_any()
            }}
        </div>
    }
}

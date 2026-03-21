use leptos::prelude::*;

#[component]
pub fn ServerStats(stats: ReadSignal<Option<serde_json::Value>>) -> impl IntoView {
    let content = move || {
        let s = match stats.get() {
            Some(s) => s,
            None => return view! {
                <div class="card">
                    <h2>"Server Stats"</h2>
                    <div class="empty">"server not running"</div>
                </div>
            }.into_any(),
        };

        let slots = s.get("slots").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let slot = slots.first().cloned();

        let n_ctx = slot.as_ref()
            .and_then(|s| s["n_ctx"].as_u64())
            .unwrap_or(0);

        // Processing status
        let is_processing = slot.as_ref()
            .and_then(|s| s["is_processing"].as_bool())
            .unwrap_or(false);

        // Task count (monotonic, proxy for total requests)
        let id_task = slot.as_ref()
            .and_then(|s| s["id_task"].as_u64())
            .unwrap_or(0);

        // Last generation stats
        let n_decoded = slot.as_ref()
            .and_then(|s| s["next_token"].as_array())
            .and_then(|a| a.first())
            .and_then(|t| t["n_decoded"].as_u64())
            .unwrap_or(0);

        let status_text = if is_processing { "processing" } else { "idle" };
        let status_class = if is_processing { "badge running" } else { "badge stopped" };

        let ctx_display = if n_ctx >= 1024 {
            format!("{}K", n_ctx / 1024)
        } else {
            format!("{n_ctx}")
        };

        view! {
            <div class="card">
                <h2>"Server Stats"</h2>
                <div class="stat">
                    <div class="stat-label">"Status"</div>
                    <div><span class=status_class>{status_text}</span></div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Requests Served"</div>
                    <div class="stat-value mono">{format!("{id_task}")}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Last Gen Tokens"</div>
                    <div class="stat-value mono">{format!("{n_decoded}")}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Context Window"</div>
                    <div class="stat-value mono">{ctx_display}</div>
                </div>
            </div>
        }.into_any()
    };

    view! { <div>{content}</div> }
}

use crate::ServerStatus;
use leptos::prelude::*;

/// What the Server Stats card should show. Pulled out of the view so the
/// "do we actually have data?" decision is testable without a DOM.
#[derive(Debug, PartialEq)]
pub(crate) enum StatsCard {
    NotRunning,
    /// Running, but no slot data — every field renders N/A.
    Unavailable(&'static str),
    Slot(serde_json::Value),
}

/// Why we have no slot data. vLLM has no `/slots` endpoint at all; llama-server
/// can still miss it (`--no-slots`, 404, or the port gone mid-swap).
fn no_slots_reason(is_vllm: bool) -> &'static str {
    if is_vllm {
        "N/A — vLLM does not expose /slots"
    } else {
        "N/A — stats unavailable"
    }
}

/// The daemon sends `{"available": true, "slots": null}` whenever the `/slots`
/// proxy fails. That used to be treated as "no data" for vLLM only; every other
/// backend fell through to `unwrap_or(0)` and rendered
/// "idle / 0 / 0 / 0" styled exactly like a real measurement. Missing data is
/// missing on every backend.
pub(crate) fn stats_card(
    is_running: bool,
    is_vllm: bool,
    stats: Option<&serde_json::Value>,
) -> StatsCard {
    let Some(s) = stats else {
        // No payload at all — only "not running" if the server really is.
        return if is_running {
            StatsCard::Unavailable(no_slots_reason(is_vllm))
        } else {
            StatsCard::NotRunning
        };
    };

    match s
        .get("slots")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
    {
        Some(slot) => StatsCard::Slot(slot.clone()),
        None => StatsCard::Unavailable(no_slots_reason(is_vllm)),
    }
}

#[component]
pub fn ServerStats(
    stats: ReadSignal<Option<serde_json::Value>>,
    status: ReadSignal<ServerStatus>,
) -> impl IntoView {
    // One card for "running, but we have no slot data" — every field N/A rather
    // than a zero styled identically to a measurement.
    let unavailable = |reason: &'static str| {
        view! {
            <div class="card">
                <h2>"Server Stats"</h2>
                <div class="stat">
                    <div class="stat-label">"Status"</div>
                    <div class="stat-value">{reason}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Requests Served"</div>
                    <div class="stat-value mono">"N/A"</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Last Gen Tokens"</div>
                    <div class="stat-value mono">"N/A"</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Context Window"</div>
                    <div class="stat-value mono">"N/A"</div>
                </div>
            </div>
        }
        .into_any()
    };

    let content = move || {
        let current_status = status.get();
        let is_running = current_status.state == "running";
        let is_vllm = current_status.backend.as_deref() == Some("vllm");

        let slot = match stats_card(is_running, is_vllm, stats.get().as_ref()) {
            StatsCard::Slot(slot) => slot,
            StatsCard::Unavailable(reason) => return unavailable(reason),
            StatsCard::NotRunning => {
                return view! {
                    <div class="card">
                        <h2>"Server Stats"</h2>
                        <div class="empty">"server not running"</div>
                    </div>
                }
                .into_any();
            }
        };

        // Every unwrap_or(0) below is safe now: we only get here with real slot
        // data, so a 0 is a measured 0.
        let n_ctx = slot["n_ctx"].as_u64().unwrap_or(0);

        // Processing status
        let is_processing = slot["is_processing"].as_bool().unwrap_or(false);

        // Task count (monotonic, proxy for total requests)
        let id_task = slot["id_task"].as_u64().unwrap_or(0);

        // Last generation stats
        let n_decoded = slot["next_token"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|t| t["n_decoded"].as_u64())
            .unwrap_or(0);

        let status_text = if is_processing { "processing" } else { "idle" };
        let status_class = if is_processing {
            "badge running"
        } else {
            "badge stopped"
        };

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
        }
        .into_any()
    };

    view! { <div>{content}</div> }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// LAN-1145 regression. The daily driver is llama-server, and its `/slots`
    /// proxy fails often enough to matter (`--no-slots`, 404, port gone
    /// mid-swap). Before the fix this branch was gated on `&& is_vllm`, so
    /// llama-server fell through and rendered "idle / 0 / 0 / 0" as if measured.
    #[test]
    fn llama_server_null_slots_is_unavailable_not_zeros() {
        let stats = json!({"available": true, "slots": null});
        assert_eq!(
            stats_card(true, false, Some(&stats)),
            StatsCard::Unavailable("N/A — stats unavailable"),
            "missing slot data must not render as real zeros on llama-server"
        );
    }

    #[test]
    fn vllm_null_slots_keeps_its_specific_reason() {
        let stats = json!({"available": true, "slots": null});
        assert_eq!(
            stats_card(true, true, Some(&stats)),
            StatsCard::Unavailable("N/A — vLLM does not expose /slots")
        );
    }

    #[test]
    fn empty_slots_array_is_also_unavailable() {
        let stats = json!({"available": true, "slots": []});
        assert_eq!(
            stats_card(true, false, Some(&stats)),
            StatsCard::Unavailable("N/A — stats unavailable")
        );
    }

    #[test]
    fn real_slot_data_still_renders() {
        let stats = json!({"available": true, "slots": [{"id": 0, "n_ctx": 131072}]});
        assert_eq!(
            stats_card(true, false, Some(&stats)),
            StatsCard::Slot(json!({"id": 0, "n_ctx": 131072}))
        );
    }

    #[test]
    fn no_payload_while_running_is_unavailable_on_any_backend() {
        assert_eq!(
            stats_card(true, false, None),
            StatsCard::Unavailable("N/A — stats unavailable"),
            "'server not running' is a lie when status says it is running"
        );
    }

    #[test]
    fn no_payload_while_stopped_is_not_running() {
        assert_eq!(stats_card(false, false, None), StatsCard::NotRunning);
    }
}

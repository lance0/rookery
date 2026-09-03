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

/// Why we have no slot data.
///
/// `/slots` is a llama-server endpoint. Container backends do not implement it
/// at all, so "unavailable" there is the normal steady state, not a fault —
/// saying so plainly stops it reading like something is broken. llama-server
/// itself can still miss it (`--no-slots`, 404, or the port gone mid-swap).
fn no_slots_reason(backend: Option<&str>) -> &'static str {
    match backend {
        Some("vllm") => "N/A — vLLM does not expose /slots",
        Some("sglang") => "N/A — SGLang does not expose /slots",
        _ => "N/A — stats unavailable",
    }
}

/// The daemon sends `{"available": true, "slots": null}` whenever the `/slots`
/// proxy fails. That used to be treated as "no data" for vLLM only; every other
/// backend fell through to `unwrap_or(0)` and rendered
/// "idle / 0 / 0 / 0" styled exactly like a real measurement. Missing data is
/// missing on every backend.
pub(crate) fn stats_card(
    is_running: bool,
    backend: Option<&str>,
    stats: Option<&serde_json::Value>,
) -> StatsCard {
    let Some(s) = stats else {
        // No payload at all — only "not running" if the server really is.
        return if is_running {
            StatsCard::Unavailable(no_slots_reason(backend))
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
        None => StatsCard::Unavailable(no_slots_reason(backend)),
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
        let backend = current_status.backend.as_deref();

        // SGLang exposes no /slots, but its Prometheus scrape carries strictly
        // more than llama-server's slot payload — including mamba_usage, the
        // GDN state pool, which is the resource that actually runs out first on
        // a 32GB card. Render that instead of an "unavailable" placeholder.
        if backend == Some("sglang")
            && let Some(m) = stats.get().as_ref().and_then(|s| s.get("sglang").cloned())
            && m.as_object().is_some_and(|o| !o.is_empty())
        {
            return sglang_card(&m);
        }

        let slot = match stats_card(is_running, backend, stats.get().as_ref()) {
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

/// Render SGLang's telemetry. Separate from the llama-server slot card because
/// the shape is different, not merely the field names.
fn sglang_card(m: &serde_json::Value) -> leptos::prelude::AnyView {
    use leptos::prelude::*;

    let num = |k: &str| m.get(k).and_then(|v| v.as_f64());
    let pct = |v: Option<f64>| match v {
        Some(x) if x.is_finite() => format!("{:.1}%", x * 100.0),
        _ => "—".to_string(),
    };
    let int = |v: Option<f64>| match v {
        Some(x) if x.is_finite() => {
            // thousands separators; a bare 137735 is hard to read at a glance
            let n = x as i64;
            let sgn = if n < 0 { "-" } else { "" };
            let d: Vec<char> = n.abs().to_string().chars().collect();
            let mut out = String::new();
            for (i, c) in d.iter().enumerate() {
                if i > 0 && (d.len() - i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(*c);
            }
            format!("{sgn}{out}")
        }
        _ => "—".to_string(),
    };
    let f2 = |v: Option<f64>| match v {
        Some(x) if x.is_finite() => format!("{x:.2}"),
        _ => "—".to_string(),
    };

    let kv_used = int(num("kv_used"));
    let kv_total = int(num("kv_total"));
    let kv_usage = pct(num("kv_usage"));
    // The binding constraint on this card, so it gets its own tile.
    let mamba = pct(num("mamba_usage"));
    let accept_len = f2(num("accept_length"));
    let accept_rate = pct(num("accept_rate"));
    let cache_hit = pct(num("cache_hit_rate"));
    let running = int(num("running_reqs"));
    let queued = int(num("queued_reqs"));
    let kv_gb = f2(num("kv_cache_gb"));

    let busy = num("running_reqs").unwrap_or(0.0) > 0.0;
    let status_text = if busy { "processing" } else { "idle" };
    let status_class = if busy {
        "badge running"
    } else {
        "badge stopped"
    };

    view! {
        <div class="card">
            <h2>"Server Stats"</h2>
            <div class="stat-grid">
                <div class="stat">
                    <div class="stat-label">"Status"</div>
                    <div><span class=status_class>{status_text}</span></div>
                </div>
                <div class="stat">
                    <div class="stat-label">"KV Used"</div>
                    <div class="stat-value mono">{format!("{kv_used} / {kv_total}")}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"KV Usage"</div>
                    <div class="stat-value mono">{kv_usage}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"GDN State Pool"</div>
                    <div class="stat-value mono">{mamba}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Accept Length"</div>
                    <div class="stat-value mono">{accept_len}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Accept Rate"</div>
                    <div class="stat-value mono">{accept_rate}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Prefix Cache Hit"</div>
                    <div class="stat-value mono">{cache_hit}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"Requests"</div>
                    <div class="stat-value mono">{format!("{running} run / {queued} queued")}</div>
                </div>
                <div class="stat">
                    <div class="stat-label">"KV Cache"</div>
                    <div class="stat-value mono">{format!("{kv_gb} GB")}</div>
                </div>
            </div>
        </div>
    }
    .into_any()
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
            stats_card(true, Some("llama-server"), Some(&stats)),
            StatsCard::Unavailable("N/A — stats unavailable"),
            "missing slot data must not render as real zeros on llama-server"
        );
    }

    #[test]
    fn vllm_null_slots_keeps_its_specific_reason() {
        let stats = json!({"available": true, "slots": null});
        assert_eq!(
            stats_card(true, Some("vllm"), Some(&stats)),
            StatsCard::Unavailable("N/A — vLLM does not expose /slots")
        );
    }

    /// SGLang is container-backed and has no `/slots`, same as vLLM. Falling
    /// through to llama-server's generic "stats unavailable" would imply
    /// something failed, when in fact the endpoint simply does not exist.
    #[test]
    fn sglang_null_slots_gets_its_own_reason() {
        let stats = json!({"available": true, "slots": null});
        assert_eq!(
            stats_card(true, Some("sglang"), Some(&stats)),
            StatsCard::Unavailable("N/A — SGLang does not expose /slots")
        );
    }

    #[test]
    fn unknown_backend_falls_back_to_generic_reason() {
        let stats = json!({"available": true, "slots": null});
        assert_eq!(
            stats_card(true, None, Some(&stats)),
            StatsCard::Unavailable("N/A — stats unavailable")
        );
    }

    #[test]
    fn empty_slots_array_is_also_unavailable() {
        let stats = json!({"available": true, "slots": []});
        assert_eq!(
            stats_card(true, Some("llama-server"), Some(&stats)),
            StatsCard::Unavailable("N/A — stats unavailable")
        );
    }

    #[test]
    fn real_slot_data_still_renders() {
        let stats = json!({"available": true, "slots": [{"id": 0, "n_ctx": 131072}]});
        assert_eq!(
            stats_card(true, Some("llama-server"), Some(&stats)),
            StatsCard::Slot(json!({"id": 0, "n_ctx": 131072}))
        );
    }

    #[test]
    fn no_payload_while_running_is_unavailable_on_any_backend() {
        assert_eq!(
            stats_card(true, Some("llama-server"), None),
            StatsCard::Unavailable("N/A — stats unavailable"),
            "'server not running' is a lie when status says it is running"
        );
    }

    #[test]
    fn no_payload_while_stopped_is_not_running() {
        assert_eq!(
            stats_card(false, Some("llama-server"), None),
            StatsCard::NotRunning
        );
    }
}

use crate::components::toast::{Toast, ToastKind, show_toast};
use crate::{ServerStatus, api};
use leptos::prelude::*;

#[derive(Debug, Clone, Default)]
struct BenchResults {
    tests: Vec<BenchTest>,
    errors: Vec<BenchError>,
    loading: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
struct BenchTest {
    name: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    pp_tok_s: f64,
    gen_tok_s: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
struct BenchError {
    name: String,
    error: String,
}

/// LAN-1094. A bench where every request failed came back as `{"tests": []}`
/// and rendered as "no results yet" — the same words as the never-run state,
/// so the button flipping back was the only thing that happened. Pulled out
/// of the view so the "did this fail or did it never run?" decision is
/// testable without a DOM (same shape as LAN-1145's `stats_card`).
#[derive(Debug, PartialEq)]
enum BenchCard {
    NeverRan,
    /// Nothing measured, and here is why.
    Failed(Vec<BenchError>),
    /// Real measurements. A non-empty `.1` means some prompts still failed.
    Results(Vec<BenchTest>, Vec<BenchError>),
}

fn bench_card(tests: Vec<BenchTest>, errors: Vec<BenchError>) -> BenchCard {
    match (tests.is_empty(), errors.is_empty()) {
        (true, true) => BenchCard::NeverRan,
        (true, false) => BenchCard::Failed(errors),
        _ => BenchCard::Results(tests, errors),
    }
}

/// The toast is transient; this is the record that stays on the card.
fn failure_list(errors: Vec<BenchError>) -> impl IntoView {
    view! {
        <ul style="margin:4px 0 0 16px">
            {errors.into_iter().map(|e| view! {
                <li>{format!("{}: {}", e.name, e.error)}</li>
            }).collect_view()}
        </ul>
    }
}

#[component]
pub fn BenchPanel(
    status: ReadSignal<ServerStatus>,
    set_toasts: WriteSignal<Vec<Toast>>,
) -> impl IntoView {
    let (results, set_results) = signal(BenchResults::default());

    let is_running = move || status.get().state == "running";

    let on_bench = move |_| {
        set_results.update(|r| r.loading = true);
        let st = set_toasts;
        wasm_bindgen_futures::spawn_local(async move {
            match api::run_bench().await {
                Ok(data) => {
                    let tests: Vec<BenchTest> =
                        serde_json::from_value(data["tests"].clone()).unwrap_or_default();
                    let errors: Vec<BenchError> =
                        serde_json::from_value(data["errors"].clone()).unwrap_or_default();
                    if !errors.is_empty() {
                        show_toast(
                            st,
                            format!("{} bench test(s) failed", errors.len()),
                            ToastKind::Error,
                        );
                    }
                    set_results.set(BenchResults {
                        tests,
                        errors,
                        loading: false,
                    });
                }
                Err(e) => {
                    // The request itself never landed. That is still a failed
                    // bench, not an unrun one — say so in the panel, because a
                    // toast disappears and "no results yet" is what remains.
                    set_results.set(BenchResults {
                        tests: Vec::new(),
                        errors: vec![BenchError {
                            name: "request".into(),
                            error: e.clone(),
                        }],
                        loading: false,
                    });
                    show_toast(st, format!("bench failed: {e}"), ToastKind::Error);
                }
            }
        });
    };

    view! {
        <div class="card">
            <h2>"Benchmark"</h2>
            <div class="btn-row" style="margin-bottom:12px">
                <button
                    class="btn"
                    on:click=on_bench
                    disabled=move || !is_running() || results.get().loading
                >
                    {move || if results.get().loading { "running..." } else { "Run Bench" }}
                </button>
            </div>

            {move || {
                let r = results.get();
                match bench_card(r.tests, r.errors) {
                    BenchCard::NeverRan => {
                        view! { <div class="empty">"no results yet"</div> }.into_any()
                    }
                    BenchCard::Failed(errors) => view! {
                        <div class="empty" style="color:var(--red)">
                            <div>"bench failed — no tests completed"</div>
                            {failure_list(errors)}
                        </div>
                    }.into_any(),
                    BenchCard::Results(tests, errors) => view! {
                        {(!errors.is_empty()).then(|| view! {
                            <div class="empty" style="color:var(--red)">
                                <div>{format!("{} of {} tests failed", errors.len(),
                                    errors.len() + tests.len())}</div>
                                {failure_list(errors)}
                            </div>
                        })}
                        <table class="bench-table">
                            <thead>
                                <tr>
                                    <th>"Test"</th>
                                    <th>"PP Tok"</th>
                                    <th>"Gen Tok"</th>
                                    <th>"PP tok/s"</th>
                                    <th>"Gen tok/s"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {tests.into_iter().map(|t| view! {
                                    <tr>
                                        <td>{t.name}</td>
                                        <td class="value">{t.prompt_tokens}</td>
                                        <td class="value">{t.completion_tokens}</td>
                                        <td class="value">{format!("{:.0}", t.pp_tok_s)}</td>
                                        <td class="value">{format!("{:.0}", t.gen_tok_s)}</td>
                                    </tr>
                                }).collect_view()}
                            </tbody>
                        </table>
                    }.into_any()
                }
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(name: &str) -> BenchError {
        BenchError {
            name: name.into(),
            error: "operation timed out".into(),
        }
    }

    fn ok(name: &str) -> BenchTest {
        BenchTest {
            name: name.into(),
            prompt_tokens: 5,
            completion_tokens: 1,
            pp_tok_s: 500.0,
            gen_tok_s: 200.0,
        }
    }

    /// LAN-1094 regression. Before the fix the panel branched on
    /// `tests.is_empty()` alone, so three failed requests rendered the same
    /// "no results yet" as a bench nobody ever started.
    #[test]
    fn total_failure_is_not_the_never_ran_state() {
        assert_eq!(
            bench_card(vec![], vec![err("short"), err("medium"), err("long")]),
            BenchCard::Failed(vec![err("short"), err("medium"), err("long")]),
            "a failed bench must not render as an unrun one"
        );
    }

    #[test]
    fn nothing_run_is_still_nothing_run() {
        assert_eq!(bench_card(vec![], vec![]), BenchCard::NeverRan);
    }

    /// One real measurement is an incomplete result, not a failure — it
    /// renders, with the failures noted next to it.
    #[test]
    fn partial_failure_keeps_the_table_and_the_reasons() {
        assert_eq!(
            bench_card(vec![ok("short")], vec![err("long")]),
            BenchCard::Results(vec![ok("short")], vec![err("long")])
        );
    }

    #[test]
    fn a_clean_run_carries_no_errors() {
        assert_eq!(
            bench_card(vec![ok("short")], vec![]),
            BenchCard::Results(vec![ok("short")], vec![])
        );
    }
}

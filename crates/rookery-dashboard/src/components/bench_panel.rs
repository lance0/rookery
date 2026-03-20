use leptos::prelude::*;
use crate::{ServerStatus, api};

#[derive(Debug, Clone, Default)]
struct BenchResults {
    tests: Vec<BenchTest>,
    loading: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct BenchTest {
    name: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    pp_tok_s: f64,
    gen_tok_s: f64,
}

#[component]
pub fn BenchPanel(status: ReadSignal<ServerStatus>) -> impl IntoView {
    let (results, set_results) = signal(BenchResults::default());

    let is_running = move || status.get().state == "running";

    let on_bench = move |_| {
        set_results.update(|r| r.loading = true);
        wasm_bindgen_futures::spawn_local(async move {
            match api::run_bench().await {
                Ok(data) => {
                    let tests: Vec<BenchTest> =
                        serde_json::from_value(data["tests"].clone()).unwrap_or_default();
                    set_results.set(BenchResults { tests, loading: false });
                }
                Err(_) => {
                    set_results.update(|r| r.loading = false);
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
                if r.tests.is_empty() {
                    view! { <div class="empty">"no results yet"</div> }.into_any()
                } else {
                    view! {
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
                                {r.tests.into_iter().map(|t| view! {
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

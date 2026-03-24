use leptos::prelude::*;
use crate::api;
use crate::components::toast::{Toast, ToastKind, show_toast};

#[component]
pub fn ModelsPanel(set_toasts: WriteSignal<Vec<Toast>>) -> impl IntoView {
    let (hardware, set_hardware) = signal(Option::<serde_json::Value>::None);
    let (search_query, set_search_query) = signal(String::new());
    let (search_results, set_search_results) = signal(Vec::<serde_json::Value>::new());
    let (selected_repo, set_selected_repo) = signal(Option::<String>::None);
    let (quants, set_quants) = signal(Vec::<serde_json::Value>::new());
    let (cached, set_cached) = signal(Vec::<serde_json::Value>::new());
    let (searching, set_searching) = signal(false);
    let (loading_quants, set_loading_quants) = signal(false);

    // Load hardware and cached models on mount
    let set_hardware_init = set_hardware.clone();
    let set_cached_init = set_cached.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(hw) = api::fetch_hardware().await {
            set_hardware_init.set(Some(hw));
        }
        if let Ok(c) = api::fetch_cached_models().await {
            if let Some(models) = c["models"].as_array() {
                set_cached_init.set(models.clone());
            }
        }
    });

    let do_search = move || {
        let q = search_query.get().trim().to_string();
        if q.is_empty() || searching.get() {
            return;
        }
        set_searching.set(true);
        set_search_results.set(Vec::new());
        set_selected_repo.set(None);
        set_quants.set(Vec::new());
        let set_toasts = set_toasts.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match api::search_models(&q).await {
                Ok(resp) => {
                    if let Some(results) = resp["results"].as_array() {
                        set_search_results.set(results.clone());
                    }
                }
                Err(e) => show_toast(set_toasts, format!("search failed: {e}"), ToastKind::Error),
            }
            set_searching.set(false);
        });
    };

    let on_search_keydown = move |e: web_sys::KeyboardEvent| {
        if e.key() == "Enter" {
            do_search();
        }
    };

    let on_search_click = move |_| do_search();

    let select_repo = move |repo: String| {
        set_selected_repo.set(Some(repo.clone()));
        set_loading_quants.set(true);
        let set_toasts = set_toasts.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match api::fetch_quants(&repo).await {
                Ok(resp) => {
                    if let Some(q) = resp["quants"].as_array() {
                        set_quants.set(q.clone());
                    }
                }
                Err(e) => show_toast(set_toasts, format!("failed to load quants: {e}"), ToastKind::Error),
            }
            set_loading_quants.set(false);
        });
    };

    let pull_quant = move |repo: String, quant: String| {
        let set_toasts = set_toasts.clone();
        let set_cached = set_cached.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match api::pull_model(&repo, Some(&quant)).await {
                Ok(resp) => {
                    if resp["started"].as_bool().unwrap_or(false) {
                        show_toast(set_toasts, format!("downloading {quant}..."), ToastKind::Success);
                    } else {
                        let msg = resp["message"].as_str().unwrap_or("pull failed");
                        show_toast(set_toasts, msg.to_string(), ToastKind::Error);
                    }
                }
                Err(e) => show_toast(set_toasts, format!("pull failed: {e}"), ToastKind::Error),
            }
            // Refresh cached list
            if let Ok(c) = api::fetch_cached_models().await {
                if let Some(models) = c["models"].as_array() {
                    set_cached.set(models.clone());
                }
            }
        });
    };

    view! {
        <div>
            // Hardware summary
            {move || {
                let hw = hardware.get();
                match hw {
                    Some(hw) => {
                        let gpu_name = hw.get("gpu").and_then(|g| g["name"].as_str()).unwrap_or("no GPU").to_string();
                        let vram_total = hw.get("gpu").and_then(|g| g["vram_total_mb"].as_u64()).unwrap_or(0);
                        let vram_free = hw.get("gpu").and_then(|g| g["vram_free_mb"].as_u64()).unwrap_or(0);
                        let bw = hw.get("gpu").and_then(|g| g["memory_bandwidth_gbps"].as_f64()).unwrap_or(0.0);
                        let cpu_name = hw.get("cpu").and_then(|c| c["name"].as_str()).unwrap_or("unknown").to_string();
                        let ram_total = hw.get("cpu").and_then(|c| c["ram_total_mb"].as_u64()).unwrap_or(0);

                        view! {
                            <div class="card" style="margin-bottom:10px">
                                <h2>"Hardware"</h2>
                                <div class="stat">
                                    <span class="stat-label">"GPU "</span>
                                    <span class="stat-value">{gpu_name}</span>
                                </div>
                                <div class="stat">
                                    <span class="stat-label">"VRAM "</span>
                                    <span class="stat-value mono">{format!("{vram_free} / {vram_total} MB free")}</span>
                                    <span class="stat-label">{format!(" ({bw:.0} GB/s)")}</span>
                                </div>
                                <div class="stat">
                                    <span class="stat-label">"CPU "</span>
                                    <span class="stat-value">{cpu_name}</span>
                                    <span class="stat-label">{format!(" ({:.0}GB RAM)", ram_total as f64 / 1024.0)}</span>
                                </div>
                            </div>
                        }.into_any()
                    }
                    None => view! { <div class="card" style="margin-bottom:10px"><div class="empty">"loading hardware..."</div></div> }.into_any()
                }
            }}

            // Search
            <div class="card" style="margin-bottom:10px">
                <h2>"Search Models"</h2>
                <div style="display:flex;gap:6px;margin-bottom:8px">
                    <input
                        class="setting-input"
                        style="flex:1;width:auto"
                        placeholder="search HuggingFace GGUF repos..."
                        prop:value=move || search_query.get()
                        on:input=move |ev| set_search_query.set(event_target_value(&ev))
                        on:keydown=on_search_keydown
                    />
                    <button class="btn" on:click=on_search_click disabled=move || searching.get()>
                        {move || if searching.get() { "..." } else { "Search" }}
                    </button>
                </div>

                // Search results
                {move || {
                    let results = search_results.get();
                    if results.is_empty() {
                        return view! { <div></div> }.into_any();
                    }
                    view! {
                        <div>
                            {results.into_iter().map(|r| {
                                let id = r["id"].as_str().unwrap_or("?").to_string();
                                let downloads = r["downloads"].as_u64().unwrap_or(0);
                                let likes = r["likes"].as_u64().unwrap_or(0);
                                let id_click = id.clone();
                                let is_selected = move || selected_repo.get().as_deref() == Some(&id);
                                let class = move || if is_selected() { "profile-card active" } else { "profile-card" };
                                view! {
                                    <button class=class on:click={
                                        let id = id_click.clone();
                                        move |_| select_repo(id.clone())
                                    } style="width:100%;margin-bottom:4px">
                                        <div class="profile-name">{id_click.clone()}</div>
                                        <div class="profile-meta">{format!("{} downloads, {} likes", format_count(downloads), format_count(likes))}</div>
                                    </button>
                                }
                            }).collect_view()}
                        </div>
                    }.into_any()
                }}
            </div>

            // Quants table
            {move || {
                let repo = selected_repo.get();
                let qs = quants.get();
                match repo {
                    Some(repo) if !qs.is_empty() => {
                        view! {
                            <div class="card" style="margin-bottom:10px">
                                <h2>{format!("Quants — {repo}")}</h2>
                                <table class="bench-table">
                                    <thead>
                                        <tr>
                                            <th>"Quant"</th>
                                            <th>"Size"</th>
                                            <th>"Fit"</th>
                                            <th>"Est tok/s"</th>
                                            <th></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {qs.into_iter().map(|q| {
                                            let label = q["label"].as_str().unwrap_or("?").to_string();
                                            let size_gb = q["total_bytes"].as_u64().unwrap_or(0) as f64 / 1_073_741_824.0;
                                            let downloaded = q["is_downloaded"].as_bool().unwrap_or(false);
                                            let fit_mode = q.get("perf_estimate")
                                                .and_then(|e| e["fit_mode"].as_str())
                                                .unwrap_or("unknown")
                                                .to_string();
                                            let est_toks = q.get("perf_estimate")
                                                .and_then(|e| e["estimated_gen_toks"].as_f64())
                                                .unwrap_or(0.0);

                                            let fit_class = match fit_mode.as_str() {
                                                "full_gpu" => "badge running",
                                                "partial_offload" => "badge starting",
                                                "cpu_only" => "badge stopped",
                                                _ => "badge failed",
                                            };
                                            let fit_label = fit_mode.replace('_', " ");

                                            let repo_for_pull = repo.clone();
                                            let label_for_pull = label.clone();

                                            view! {
                                                <tr>
                                                    <td>
                                                        {label.clone()}
                                                        {if downloaded { " ✓" } else { "" }}
                                                    </td>
                                                    <td class="value">{format!("{size_gb:.1}GB")}</td>
                                                    <td><span class=fit_class>{fit_label}</span></td>
                                                    <td class="value">{format!("~{est_toks:.0}")}</td>
                                                    <td>
                                                        <button
                                                            class="btn"
                                                            disabled=move || downloaded
                                                            on:click={
                                                                let r = repo_for_pull.clone();
                                                                let l = label_for_pull.clone();
                                                                move |_| pull_quant(r.clone(), l.clone())
                                                            }
                                                        >
                                                            {if downloaded { "cached" } else { "pull" }}
                                                        </button>
                                                    </td>
                                                </tr>
                                            }
                                        }).collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        }.into_any()
                    }
                    Some(_) if loading_quants.get() => {
                        view! { <div class="card"><div class="empty">"loading quants..."</div></div> }.into_any()
                    }
                    _ => view! { <div></div> }.into_any()
                }
            }}

            // Cached models
            {move || {
                let models = cached.get();
                if models.is_empty() {
                    return view! {
                        <div class="card">
                            <h2>"Cached Models"</h2>
                            <div class="empty">"no models in ~/.cache/llama.cpp/"</div>
                        </div>
                    }.into_any();
                }
                view! {
                    <div class="card">
                        <h2>"Cached Models"</h2>
                        {models.into_iter().map(|m| {
                            let repo = m["repo"].as_str().unwrap_or("?").to_string();
                            let quant = m["quant_label"].as_str().unwrap_or("?").to_string();
                            let size_gb = m["size_bytes"].as_u64().unwrap_or(0) as f64 / 1_073_741_824.0;
                            view! {
                                <div class="process-row">
                                    <span class="process-name">{format!("{repo} / {quant}")}</span>
                                    <span class="process-vram">{format!("{size_gb:.1}GB")}</span>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                }.into_any()
            }}
        </div>
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

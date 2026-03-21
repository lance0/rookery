use leptos::prelude::*;
use crate::ModelInfoData;

#[component]
pub fn ModelInfo(model_info: ReadSignal<ModelInfoData>) -> impl IntoView {
    let model_id = move || {
        model_info.get().model_id.unwrap_or_else(|| "—".into())
    };
    let owned_by = move || {
        model_info.get().owned_by.unwrap_or_else(|| "—".into())
    };

    let ctx_size = move || {
        let info = model_info.get();
        info.props.as_ref()
            .and_then(|p| p.get("default_generation_settings"))
            .and_then(|s| s.get("n_ctx"))
            .and_then(|v| v.as_u64())
            .map(|n| format!("{}", n))
            .unwrap_or_else(|| "—".into())
    };

    let chat_template = move || {
        let info = model_info.get();
        let has_template = info.props.as_ref()
            .and_then(|p| p.get("chat_template"))
            .is_some();
        if has_template { "loaded" } else { "—" }
    };

    let total_size = move || {
        let info = model_info.get();
        info.props.as_ref()
            .and_then(|p| p.get("total_size"))
            .and_then(|v| v.as_u64())
            .map(|bytes| {
                let gb = bytes as f64 / 1_073_741_824.0;
                format!("{gb:.1} GB")
            })
            .unwrap_or_else(|| "—".into())
    };

    view! {
        <div class="card">
            <h2>"Model"</h2>
            {move || {
                if !model_info.get().available {
                    view! {
                        <div class="empty">"no model loaded"</div>
                    }.into_any()
                } else {
                    view! {
                        <div>
                            <div class="stat">
                                <div class="stat-label">"Model ID"</div>
                                <div class="stat-value" style="word-break:break-all">{model_id}</div>
                            </div>
                            <div class="stat">
                                <div class="stat-label">"Owner"</div>
                                <div class="stat-value">{owned_by}</div>
                            </div>
                            <div class="stat">
                                <div class="stat-label">"Context"</div>
                                <div class="stat-value mono">{ctx_size}</div>
                            </div>
                            <div class="stat">
                                <div class="stat-label">"Size"</div>
                                <div class="stat-value mono">{total_size}</div>
                            </div>
                            <div class="stat">
                                <div class="stat-label">"Chat Template"</div>
                                <div class="stat-value">{chat_template}</div>
                            </div>
                        </div>
                    }.into_any()
                }
            }}
        </div>
    }
}

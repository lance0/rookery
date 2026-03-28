use leptos::prelude::*;
use crate::{ServerStatus, api};
use crate::components::toast::{Toast, ToastKind, show_toast};

#[derive(Debug, Clone, Default)]
pub struct ProfileSettings {
    pub temp: String,
    pub top_p: String,
    pub top_k: String,
    pub min_p: String,
    pub ctx_size: String,
    pub threads: String,
    pub threads_batch: String,
    pub batch_size: String,
    pub reasoning_budget: String,
}

#[component]
pub fn SettingsPanel(
    status: ReadSignal<ServerStatus>,
    set_toasts: WriteSignal<Vec<Toast>>,
) -> impl IntoView {
    let (settings, set_settings) = signal(ProfileSettings::default());
    let (loaded_profile, set_loaded_profile) = signal(String::new());
    let (saving, set_saving) = signal(false);

    // Load config when profile changes
    let load_profile = move || {
        let profile = status.get().profile.clone();
        if let Some(profile_name) = profile {
            if profile_name != loaded_profile.get() {
                let pn = profile_name.clone();
                set_loaded_profile.set(profile_name);
                wasm_bindgen_futures::spawn_local(async move {
                    if let Ok(config) = api::fetch_config().await {
                        if let Some(profiles) = config.get("profiles") {
                            if let Some(p) = profiles.get(&pn) {
                                set_settings.set(ProfileSettings {
                                    temp: p["temp"].as_f64().map(|v| format!("{v}")).unwrap_or_default(),
                                    top_p: p["top_p"].as_f64().map(|v| format!("{v}")).unwrap_or_default(),
                                    top_k: p["top_k"].as_u64().map(|v| format!("{v}")).unwrap_or_default(),
                                    min_p: p["min_p"].as_f64().map(|v| format!("{v}")).unwrap_or_default(),
                                    ctx_size: p["ctx_size"].as_u64().map(|v| format!("{v}")).unwrap_or_default(),
                                    threads: p["threads"].as_u64().map(|v| format!("{v}")).unwrap_or_default(),
                                    threads_batch: p["threads_batch"].as_u64().map(|v| format!("{v}")).unwrap_or_default(),
                                    batch_size: p["batch_size"].as_u64().map(|v| format!("{v}")).unwrap_or_default(),
                                    reasoning_budget: p["reasoning_budget"].as_i64().map(|v| format!("{v}")).unwrap_or_default(),
                                });
                            }
                        }
                    }
                });
            }
        }
    };

    let on_save = move |_| {
        let profile_name = loaded_profile.get();
        if profile_name.is_empty() { return; }
        set_saving.set(true);
        let s = settings.get();
        let pn = profile_name.clone();
        let set_toasts = set_toasts.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut update = serde_json::Map::new();
            let mut errors: Vec<String> = Vec::new();

            // Sampling params: validate ranges
            match s.temp.parse::<f64>() {
                Ok(v) if (0.0..=2.0).contains(&v) => { update.insert("temp".into(), serde_json::json!(v)); }
                Ok(v) => errors.push(format!("temp {v} out of range (0.0-2.0)")),
                Err(_) if !s.temp.is_empty() => errors.push("temp: invalid number".into()),
                _ => {}
            }
            match s.top_p.parse::<f64>() {
                Ok(v) if (0.0..=1.0).contains(&v) => { update.insert("top_p".into(), serde_json::json!(v)); }
                Ok(v) => errors.push(format!("top_p {v} out of range (0.0-1.0)")),
                Err(_) if !s.top_p.is_empty() => errors.push("top_p: invalid number".into()),
                _ => {}
            }
            match s.top_k.parse::<u64>() {
                Ok(v) if v <= 1000 => { update.insert("top_k".into(), serde_json::json!(v)); }
                Ok(v) => errors.push(format!("top_k {v} out of range (0-1000)")),
                Err(_) if !s.top_k.is_empty() => errors.push("top_k: invalid number".into()),
                _ => {}
            }
            match s.min_p.parse::<f64>() {
                Ok(v) if (0.0..=1.0).contains(&v) => { update.insert("min_p".into(), serde_json::json!(v)); }
                Ok(v) => errors.push(format!("min_p {v} out of range (0.0-1.0)")),
                Err(_) if !s.min_p.is_empty() => errors.push("min_p: invalid number".into()),
                _ => {}
            }

            // Resource params
            if let Ok(v) = s.ctx_size.parse::<u64>() { update.insert("ctx_size".into(), serde_json::json!(v)); }
            else if !s.ctx_size.is_empty() { errors.push("ctx_size: invalid number".into()); }
            if let Ok(v) = s.threads.parse::<u64>() { update.insert("threads".into(), serde_json::json!(v)); }
            else if !s.threads.is_empty() { errors.push("threads: invalid number".into()); }
            if let Ok(v) = s.threads_batch.parse::<u64>() { update.insert("threads_batch".into(), serde_json::json!(v)); }
            else if !s.threads_batch.is_empty() { errors.push("threads_batch: invalid number".into()); }
            if let Ok(v) = s.batch_size.parse::<u64>() { update.insert("batch_size".into(), serde_json::json!(v)); }
            else if !s.batch_size.is_empty() { errors.push("batch_size: invalid number".into()); }
            if let Ok(v) = s.reasoning_budget.parse::<i64>() { update.insert("reasoning_budget".into(), serde_json::json!(v)); }
            else if !s.reasoning_budget.is_empty() { errors.push("reasoning_budget: invalid number".into()); }

            if !errors.is_empty() {
                show_toast(set_toasts.clone(), errors.join(", "), ToastKind::Error);
                set_saving.set(false);
                return;
            }

            let body = serde_json::Value::Object(update);
            match api::update_profile(&pn, &body).await {
                Ok(resp) => {
                    let msg = resp["message"].as_str().unwrap_or("saved").to_string();
                    show_toast(set_toasts, msg, ToastKind::Success);
                }
                Err(e) => {
                    show_toast(set_toasts, format!("save failed: {e}"), ToastKind::Error);
                }
            }
            set_saving.set(false);
        });
    };

    view! {
        <div class="card">
            <h2>"Settings"</h2>
            {move || {
                load_profile();
                let profile_name = loaded_profile.get();
                if profile_name.is_empty() {
                    return view! { <div class="empty">"no profile selected"</div> }.into_any();
                }
                let s = settings.get();
                view! {
                    <div>
                        <div class="settings-header">
                            <span class="stat-label">"Profile: "</span>
                            <span class="stat-value">{profile_name}</span>
                        </div>
                        <div class="settings-grid">
                            <div class="setting-group">
                                <div class="setting-group-title">"Sampling"</div>
                                <SettingInput label="temp" value=s.temp.clone() on_change=move |v| set_settings.update(|s| s.temp = v) />
                                <SettingInput label="top_p" value=s.top_p.clone() on_change=move |v| set_settings.update(|s| s.top_p = v) />
                                <SettingInput label="top_k" value=s.top_k.clone() on_change=move |v| set_settings.update(|s| s.top_k = v) />
                                <SettingInput label="min_p" value=s.min_p.clone() on_change=move |v| set_settings.update(|s| s.min_p = v) />
                                <SettingInput label="reasoning_budget" value=s.reasoning_budget.clone() on_change=move |v| set_settings.update(|s| s.reasoning_budget = v) />
                            </div>
                            <div class="setting-group">
                                <div class="setting-group-title">"Resources"</div>
                                <SettingInput label="ctx_size" value=s.ctx_size.clone() on_change=move |v| set_settings.update(|s| s.ctx_size = v) />
                                <SettingInput label="threads" value=s.threads.clone() on_change=move |v| set_settings.update(|s| s.threads = v) />
                                <SettingInput label="threads_batch" value=s.threads_batch.clone() on_change=move |v| set_settings.update(|s| s.threads_batch = v) />
                                <SettingInput label="batch_size" value=s.batch_size.clone() on_change=move |v| set_settings.update(|s| s.batch_size = v) />
                            </div>
                        </div>
                        <div class="btn-row">
                            <button class="btn" on:click=on_save disabled=move || saving.get()>
                                {move || if saving.get() { "saving..." } else { "Save" }}
                            </button>
                        </div>
                        <div class="settings-note">"changes apply on next start/swap"</div>
                    </div>
                }.into_any()
            }}
        </div>
    }
}

#[component]
fn SettingInput(
    label: &'static str,
    value: String,
    on_change: impl Fn(String) + 'static,
) -> impl IntoView {
    view! {
        <div class="setting-row">
            <label class="setting-label">{label}</label>
            <input
                class="setting-input"
                type="text"
                value=value
                on:input=move |ev| {
                    on_change(event_target_value(&ev));
                }
            />
        </div>
    }
}

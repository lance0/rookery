use crate::components::toast::{Toast, ToastKind, show_toast};
use crate::{ServerStatus, api};
use leptos::prelude::*;

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

/// Pulls a profile's *effective* llama-server settings out of `/api/config`.
///
/// The `[llama_server]` sub-table wins whenever it exists: `llama_server_config()`
/// ignores the legacy flat fields entirely once it is present, and that is also
/// where `PUT /api/config/profile/{name}` writes since 0.1.7. Reading the flat
/// fields shows numbers the server does not use — including immediately after a
/// save that appeared to succeed.
fn parse_settings(profile: &serde_json::Value) -> ProfileSettings {
    let p = profile
        .get("llama_server")
        .filter(|v| !v.is_null())
        .unwrap_or(profile);

    // These are f32 server-side; serde widens them to f64, so temp 0.7 arrives
    // as 0.699999988079071. Narrowing back is lossless and keeps the form legible.
    let float = |key: &str| {
        p[key]
            .as_f64()
            .map(|v| (v as f32).to_string())
            .unwrap_or_default()
    };
    let uint = |key: &str| p[key].as_u64().map(|v| v.to_string()).unwrap_or_default();

    ProfileSettings {
        temp: float("temp"),
        top_p: float("top_p"),
        top_k: uint("top_k"),
        min_p: float("min_p"),
        ctx_size: uint("ctx_size"),
        threads: uint("threads"),
        threads_batch: uint("threads_batch"),
        batch_size: uint("batch_size"),
        reasoning_budget: p["reasoning_budget"]
            .as_i64()
            .map(|v| v.to_string())
            .unwrap_or_default(),
    }
}

async fn fetch_settings(profile: &str) -> Result<ProfileSettings, String> {
    let config = api::fetch_config().await?;
    config
        .get("profiles")
        .and_then(|profiles| profiles.get(profile))
        .map(parse_settings)
        .ok_or_else(|| format!("'{profile}' is not in the daemon config"))
}

#[component]
pub fn SettingsPanel(
    status: ReadSignal<ServerStatus>,
    set_toasts: WriteSignal<Vec<Toast>>,
) -> impl IntoView {
    let (settings, set_settings) = signal(ProfileSettings::default());
    let (loaded_profile, set_loaded_profile) = signal(String::new());
    let (saving, set_saving) = signal(false);

    // Reload when the running profile changes. This is an effect, not a call from
    // the render closure: writing signals from the render closure rebuilt the whole
    // form on every keystroke and on every status tick, and made the load order
    // below impossible to reason about.
    Effect::new(move |prev: Option<Option<String>>| {
        let profile = status.get().profile;
        if prev.as_ref() == Some(&profile) {
            return profile;
        }

        // Drop the previous profile's values *synchronously*. `loaded_profile` and
        // `settings` are then only ever set together, in the success arm below, so
        // the header can never name one profile while the inputs still hold
        // another's numbers — that window is what let a save PUT them onto the
        // wrong profile. It also means the Save button is unrendered until real
        // values have arrived.
        set_settings.set(ProfileSettings::default());
        set_loaded_profile.set(String::new());

        if let Some(name) = profile.clone() {
            wasm_bindgen_futures::spawn_local(async move {
                match fetch_settings(&name).await {
                    Ok(s) => {
                        set_settings.set(s);
                        set_loaded_profile.set(name);
                    }
                    Err(e) => show_toast(
                        set_toasts,
                        format!("could not load {name} settings: {e}"),
                        ToastKind::Error,
                    ),
                }
            });
        }

        profile
    });

    let on_save = move |_| {
        let profile_name = loaded_profile.get();
        if profile_name.is_empty() {
            return;
        }
        set_saving.set(true);
        let s = settings.get();
        let pn = profile_name.clone();
        let set_toasts = set_toasts;
        wasm_bindgen_futures::spawn_local(async move {
            let mut update = serde_json::Map::new();
            let mut errors: Vec<String> = Vec::new();

            // An empty field is an error, not "leave this one alone". Omitting the
            // key makes the server skip it (`if let Some(v)`), so clearing a field
            // used to toast "saved" and then show the old value again on reopen.
            // Every field is populated from config on load, so none is legitimately
            // blank.

            // Sampling params: validate ranges
            match s.temp.parse::<f64>() {
                Ok(v) if (0.0..=2.0).contains(&v) => {
                    update.insert("temp".into(), serde_json::json!(v));
                }
                Ok(v) => errors.push(format!("temp {v} out of range (0.0-2.0)")),
                Err(_) => errors.push("temp: invalid number".into()),
            }
            match s.top_p.parse::<f64>() {
                Ok(v) if (0.0..=1.0).contains(&v) => {
                    update.insert("top_p".into(), serde_json::json!(v));
                }
                Ok(v) => errors.push(format!("top_p {v} out of range (0.0-1.0)")),
                Err(_) => errors.push("top_p: invalid number".into()),
            }
            match s.top_k.parse::<u64>() {
                Ok(v) if v <= 1000 => {
                    update.insert("top_k".into(), serde_json::json!(v));
                }
                Ok(v) => errors.push(format!("top_k {v} out of range (0-1000)")),
                Err(_) => errors.push("top_k: invalid number".into()),
            }
            match s.min_p.parse::<f64>() {
                Ok(v) if (0.0..=1.0).contains(&v) => {
                    update.insert("min_p".into(), serde_json::json!(v));
                }
                Ok(v) => errors.push(format!("min_p {v} out of range (0.0-1.0)")),
                Err(_) => errors.push("min_p: invalid number".into()),
            }

            // Resource params
            if let Ok(v) = s.ctx_size.parse::<u64>() {
                update.insert("ctx_size".into(), serde_json::json!(v));
            } else {
                errors.push("ctx_size: invalid number".into());
            }
            if let Ok(v) = s.threads.parse::<u64>() {
                update.insert("threads".into(), serde_json::json!(v));
            } else {
                errors.push("threads: invalid number".into());
            }
            if let Ok(v) = s.threads_batch.parse::<u64>() {
                update.insert("threads_batch".into(), serde_json::json!(v));
            } else {
                errors.push("threads_batch: invalid number".into());
            }
            if let Ok(v) = s.batch_size.parse::<u64>() {
                update.insert("batch_size".into(), serde_json::json!(v));
            } else {
                errors.push("batch_size: invalid number".into());
            }
            if let Ok(v) = s.reasoning_budget.parse::<i64>() {
                update.insert("reasoning_budget".into(), serde_json::json!(v));
            } else {
                errors.push("reasoning_budget: invalid number".into());
            }

            if !errors.is_empty() {
                show_toast(set_toasts, errors.join(", "), ToastKind::Error);
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
                // prop, not attribute: once the user has typed, the DOM's dirty
                // value flag makes an attribute write a no-op, so a freshly
                // fetched profile's value would never reach the field.
                prop:value=value
                on:input=move |ev| {
                    on_change(event_target_value(&ev));
                }
            />
        </div>
    }
}

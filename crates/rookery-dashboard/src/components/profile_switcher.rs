use leptos::prelude::*;
use crate::{ProfileInfo, ServerStatus, AgentsData, api};
use crate::components::toast::{Toast, ToastKind, show_toast};

#[component]
pub fn ProfileSwitcher(
    profiles: ReadSignal<Vec<ProfileInfo>>,
    status: ReadSignal<ServerStatus>,
    set_profiles: WriteSignal<Vec<ProfileInfo>>,
    set_agents: WriteSignal<AgentsData>,
    set_toasts: WriteSignal<Vec<Toast>>,
) -> impl IntoView {
    let active_profile = move || status.get().profile.clone();
    let is_running = move || status.get().state == "running";

    view! {
        <div class="card">
            <h2>"Profiles"</h2>
            <div class="profile-grid">
                {move || {
                    let active = active_profile();
                    profiles.get().into_iter().map(|p| {
                        let name = p.name.clone();
                        let is_active = active.as_ref() == Some(&name);
                        let card_class = if is_active { "profile-card active" } else { "profile-card" };
                        let ctx_str = p.ctx_size.map(|c| {
                            if c >= 1024 {
                                format!("{}K", c / 1024)
                            } else {
                                c.to_string()
                            }
                        });
                        let thinking = if p.reasoning_budget.unwrap_or(0) != 0 { " thinking" } else { "" };
                        let backend_label = p.backend.as_deref().unwrap_or("llama-server");
                        let meta = if let Some(ctx) = ctx_str {
                            format!("{}, {ctx}{thinking}, {backend_label}", p.model)
                        } else {
                            format!("{}, {backend_label}", p.model)
                        };
                        let default_marker = if p.default { " *" } else { "" };

                        let click_name = name.clone();
                        let running = is_running();
                        let set_profiles = set_profiles.clone();
                        let set_agents = set_agents.clone();
                        let set_toasts = set_toasts.clone();
                        let on_click = move |_| {
                            let n = click_name.clone();
                            let sp = set_profiles.clone();
                            let sa = set_agents.clone();
                            let st = set_toasts.clone();
                            wasm_bindgen_futures::spawn_local(async move {
                                let result = if running {
                                    api::swap_profile(&n).await
                                } else {
                                    api::start_server(Some(&n)).await
                                };
                                match result {
                                    Ok(resp) => {
                                        let msg = resp["message"].as_str().unwrap_or("done").to_string();
                                        let success = resp["success"].as_bool().unwrap_or(false);
                                        show_toast(st, msg, if success { ToastKind::Success } else { ToastKind::Error });
                                    }
                                    Err(e) => show_toast(st, format!("failed: {e}"), ToastKind::Error),
                                }
                                if let Ok(p) = api::fetch_profiles().await { sp.set(p); }
                                if let Ok(a) = api::fetch_agents().await { sa.set(a); }
                            });
                        };

                        view! {
                            <button class=card_class on:click=on_click>
                                <div class="profile-name">{name}{default_marker}</div>
                                <div class="profile-meta">{meta}</div>
                            </button>
                        }
                    }).collect_view()
                }}
            </div>
        </div>
    }
}

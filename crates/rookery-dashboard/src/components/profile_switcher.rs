use leptos::prelude::*;
use crate::{ProfileInfo, ServerStatus, AgentsData, api};

#[component]
pub fn ProfileSwitcher(
    profiles: ReadSignal<Vec<ProfileInfo>>,
    status: ReadSignal<ServerStatus>,
    set_profiles: WriteSignal<Vec<ProfileInfo>>,
    set_agents: WriteSignal<AgentsData>,
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
                        let ctx = if p.ctx_size >= 1024 {
                            format!("{}K", p.ctx_size / 1024)
                        } else {
                            p.ctx_size.to_string()
                        };
                        let thinking = if p.reasoning_budget != 0 { " thinking" } else { "" };
                        let meta = format!("{}, {ctx}{thinking}", p.model);
                        let default_marker = if p.default { " *" } else { "" };

                        let click_name = name.clone();
                        let running = is_running();
                        let set_profiles = set_profiles.clone();
                        let set_agents = set_agents.clone();
                        let on_click = move |_| {
                            let n = click_name.clone();
                            let sp = set_profiles.clone();
                            let sa = set_agents.clone();
                            wasm_bindgen_futures::spawn_local(async move {
                                if running {
                                    let _ = api::swap_profile(&n).await;
                                } else {
                                    let _ = api::start_server(Some(&n)).await;
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

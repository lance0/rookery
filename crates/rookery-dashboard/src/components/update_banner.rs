use leptos::prelude::*;

#[component]
pub fn UpdateBanner(releases: ReadSignal<Option<serde_json::Value>>) -> impl IntoView {
    view! {
        <div class="card update-banner">
            <h2>"Upstream Releases"</h2>
            {move || {
                let data = releases.get();
                let Some(data) = data else {
                    return view! { <div class="empty">"checking..."</div> }.into_any();
                };

                let repos = data["repos"].as_array().cloned().unwrap_or_default();
                if repos.is_empty() {
                    return view! { <div class="empty">"no release data yet"</div> }.into_any();
                }

                view! {
                    <div class="release-list">
                        {repos.iter().map(|repo| {
                            let name = repo["repo"].as_str().unwrap_or("unknown").to_string();
                            let short_name = name.rsplit('/').next().unwrap_or(&name).to_string();
                            let current = repo["current_version"]["raw"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string();
                            let latest_tag = repo["latest"]["tag_name"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string();
                            let release_url = repo["latest"]["html_url"]
                                .as_str()
                                .unwrap_or("#")
                                .to_string();
                            let update_available = repo["update_available"].as_bool().unwrap_or(false);
                            let ahead = repo["ahead_of_release"].as_bool().unwrap_or(false);

                            let (badge_class, badge_text) = if update_available {
                                ("badge update-available", "update available")
                            } else if ahead {
                                ("badge ahead", "ahead of release")
                            } else {
                                ("badge running", "up to date")
                            };

                            view! {
                                <div class="release-item">
                                    <div class="release-header">
                                        <span class="release-name">{short_name}</span>
                                        <span class=badge_class>{badge_text}</span>
                                    </div>
                                    <div class="release-versions">
                                        <span class="stat-label">"current "</span>
                                        <span class="stat-value">{current}</span>
                                        <span class="release-arrow">" → "</span>
                                        <span class="stat-label">"latest "</span>
                                        <a href=release_url target="_blank" class="stat-value release-link">{latest_tag}</a>
                                    </div>
                                </div>
                            }
                        }).collect_view()}
                        {
                            let checked = repos.first()
                                .and_then(|r| r["checked_at"].as_str())
                                .map(|s| s.to_string());
                            checked.map(|ts| {
                                view! {
                                    <div class="release-checked">
                                        {format_checked_ago(&ts)}
                                    </div>
                                }
                            })
                        }
                    </div>
                }.into_any()
            }}
        </div>
    }
}

fn format_checked_ago(ts: &str) -> String {
    let Ok(dt) = ts.parse::<chrono::DateTime<chrono::Utc>>() else {
        return format!("checked at {ts}");
    };
    let ago = chrono::Utc::now().signed_duration_since(dt);
    let mins = ago.num_minutes();
    if mins < 1 {
        "checked just now".to_string()
    } else if mins < 60 {
        format!("checked {mins}m ago")
    } else {
        let hours = ago.num_hours();
        format!("checked {hours}h ago")
    }
}

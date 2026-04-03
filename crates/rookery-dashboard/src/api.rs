use gloo_net::http::{Request, RequestBuilder, Response};

use crate::{AgentsData, ModelInfoData, ProfileInfo};

const API_KEY_STORAGE_KEY: &str = "rookery-api-key";

fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

fn storage() -> Option<web_sys::Storage> {
    window()?.local_storage().ok()?
}

fn auth_request(builder: RequestBuilder) -> RequestBuilder {
    if let Some(api_key) = get_api_key() {
        builder.header("Authorization", &format!("Bearer {api_key}"))
    } else {
        builder
    }
}

async fn send(builder: RequestBuilder) -> Result<Response, String> {
    let response = builder.send().await.map_err(|e| e.to_string())?;
    handle_response(response)
}

async fn send_request(request: Request) -> Result<Response, String> {
    let response = request.send().await.map_err(|e| e.to_string())?;
    handle_response(response)
}

fn handle_response(response: Response) -> Result<Response, String> {
    if response.status() == 401 {
        notify_auth_required();
        return Err("HTTP 401".into());
    }

    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }

    Ok(response)
}

pub fn get_api_key() -> Option<String> {
    storage()?
        .get_item(API_KEY_STORAGE_KEY)
        .ok()?
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

pub fn set_api_key(value: &str) -> Result<(), String> {
    let key = value.trim();
    if key.is_empty() {
        return Err("API key cannot be empty".into());
    }

    storage()
        .ok_or_else(|| "localStorage unavailable".to_string())?
        .set_item(API_KEY_STORAGE_KEY, key)
        .map_err(|e| format!("{e:?}"))
}

pub fn clear_api_key() -> Result<(), String> {
    if let Some(storage) = storage() {
        storage
            .remove_item(API_KEY_STORAGE_KEY)
            .map_err(|e| format!("{e:?}"))?;
    }
    Ok(())
}

pub fn notify_auth_required() {
    if let Some(window) = window()
        && let Ok(event) = web_sys::Event::new("rookery-auth-required")
    {
        let _ = window.dispatch_event(&event);
    }
}

pub fn is_unauthorized(error: &str) -> bool {
    error.trim() == "HTTP 401"
}

pub fn events_url() -> String {
    match get_api_key() {
        Some(api_key) => {
            let encoded: String = js_sys::encode_uri_component(&api_key).into();
            format!("/api/events?token={encoded}")
        }
        None => "/api/events".into(),
    }
}

pub async fn fetch_profiles() -> Result<Vec<ProfileInfo>, String> {
    let resp = send(auth_request(Request::get("/api/profiles"))).await?;
    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let profiles: Vec<ProfileInfo> =
        serde_json::from_value(data["profiles"].clone()).unwrap_or_default();
    Ok(profiles)
}

pub async fn fetch_agents() -> Result<AgentsData, String> {
    let resp = send(auth_request(Request::get("/api/agents"))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_logs(n: usize) -> Result<Vec<String>, String> {
    let resp = send(auth_request(Request::get(&format!("/api/logs?n={n}")))).await?;
    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let lines: Vec<String> = serde_json::from_value(data["lines"].clone()).unwrap_or_default();
    Ok(lines)
}

pub async fn start_server(profile: Option<&str>) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "profile": profile });
    let request = auth_request(Request::post("/api/start"));
    let resp = send_request(request.json(&body).map_err(|e| e.to_string())?).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn stop_server() -> Result<serde_json::Value, String> {
    let request = auth_request(Request::post("/api/stop"));
    let resp = send_request(
        request
            .json(&serde_json::json!({}))
            .map_err(|e| e.to_string())?,
    )
    .await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn sleep_server() -> Result<serde_json::Value, String> {
    let request = auth_request(Request::post("/api/sleep"));
    let resp = send_request(
        request
            .json(&serde_json::json!({}))
            .map_err(|e| e.to_string())?,
    )
    .await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn wake_server() -> Result<serde_json::Value, String> {
    let request = auth_request(Request::post("/api/wake"));
    let resp = send_request(
        request
            .json(&serde_json::json!({}))
            .map_err(|e| e.to_string())?,
    )
    .await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn swap_profile(profile: &str) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "profile": profile });
    let request = auth_request(Request::post("/api/swap"));
    let resp = send_request(request.json(&body).map_err(|e| e.to_string())?).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_agent_health(name: &str) -> Result<serde_json::Value, String> {
    let resp = send(auth_request(Request::get(&format!("/api/agents/{name}/health")))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn start_agent(name: &str) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "name": name });
    let request = auth_request(Request::post("/api/agents/start"));
    let resp = send_request(request.json(&body).map_err(|e| e.to_string())?).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn stop_agent(name: &str) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "name": name });
    let request = auth_request(Request::post("/api/agents/stop"));
    let resp = send_request(request.json(&body).map_err(|e| e.to_string())?).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn update_agent(name: &str) -> Result<serde_json::Value, String> {
    let request = auth_request(Request::post(&format!("/api/agents/{name}/update")));
    let resp = send_request(
        request
            .json(&serde_json::json!({}))
            .map_err(|e| e.to_string())?,
    )
    .await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn run_bench() -> Result<serde_json::Value, String> {
    let resp = send(auth_request(Request::get("/api/bench"))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_model_info() -> Result<ModelInfoData, String> {
    let resp = send(auth_request(Request::get("/api/model-info"))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_server_stats() -> Result<serde_json::Value, String> {
    let resp = send(auth_request(Request::get("/api/server-stats"))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_config() -> Result<serde_json::Value, String> {
    let resp = send(auth_request(Request::get("/api/config"))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_hardware() -> Result<serde_json::Value, String> {
    let resp = send(auth_request(Request::get("/api/hardware"))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn search_models(query: &str) -> Result<serde_json::Value, String> {
    let resp = send(auth_request(Request::get(&format!("/api/models/search?q={query}")))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_quants(repo: &str) -> Result<serde_json::Value, String> {
    let resp = send(auth_request(Request::get(&format!("/api/models/quants?repo={repo}")))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_cached_models() -> Result<serde_json::Value, String> {
    let resp = send(auth_request(Request::get("/api/models/cached"))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_releases() -> Result<serde_json::Value, String> {
    let resp = send(auth_request(Request::get("/api/releases"))).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn pull_model(repo: &str, quant: Option<&str>) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "repo": repo, "quant": quant });
    let request = auth_request(Request::post("/api/models/pull"));
    let resp = send_request(request.json(&body).map_err(|e| e.to_string())?).await?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn update_profile(
    name: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request = auth_request(Request::put(&format!("/api/config/profile/{name}")));
    let resp = send_request(request.json(data).map_err(|e| e.to_string())?).await?;
    resp.json().await.map_err(|e| e.to_string())
}

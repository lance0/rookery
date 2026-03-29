use gloo_net::http::Request;
use serde_json;

use crate::{AgentsData, ModelInfoData, ProfileInfo};

pub async fn fetch_profiles() -> Result<Vec<ProfileInfo>, String> {
    let resp = Request::get("/api/profiles")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let profiles: Vec<ProfileInfo> =
        serde_json::from_value(data["profiles"].clone()).unwrap_or_default();
    Ok(profiles)
}

pub async fn fetch_agents() -> Result<AgentsData, String> {
    let resp = Request::get("/api/agents")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_logs(n: usize) -> Result<Vec<String>, String> {
    let resp = Request::get(&format!("/api/logs?n={n}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let lines: Vec<String> = serde_json::from_value(data["lines"].clone()).unwrap_or_default();
    Ok(lines)
}

pub async fn start_server(profile: Option<&str>) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "profile": profile });
    let resp = Request::post("/api/start")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn stop_server() -> Result<serde_json::Value, String> {
    let resp = Request::post("/api/stop")
        .json(&serde_json::json!({}))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn sleep_server() -> Result<serde_json::Value, String> {
    let resp = Request::post("/api/sleep")
        .json(&serde_json::json!({}))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn wake_server() -> Result<serde_json::Value, String> {
    let resp = Request::post("/api/wake")
        .json(&serde_json::json!({}))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn swap_profile(profile: &str) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "profile": profile });
    let resp = Request::post("/api/swap")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn start_agent(name: &str) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "name": name });
    let resp = Request::post("/api/agents/start")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn stop_agent(name: &str) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "name": name });
    let resp = Request::post("/api/agents/stop")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn update_agent(name: &str) -> Result<serde_json::Value, String> {
    let resp = Request::post(&format!("/api/agents/{name}/update"))
        .json(&serde_json::json!({}))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn run_bench() -> Result<serde_json::Value, String> {
    let resp = Request::get("/api/bench")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_model_info() -> Result<ModelInfoData, String> {
    let resp = Request::get("/api/model-info")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_server_stats() -> Result<serde_json::Value, String> {
    let resp = Request::get("/api/server-stats")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_config() -> Result<serde_json::Value, String> {
    let resp = Request::get("/api/config")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

// --- Model discovery ---

pub async fn fetch_hardware() -> Result<serde_json::Value, String> {
    let resp = Request::get("/api/hardware")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn search_models(query: &str) -> Result<serde_json::Value, String> {
    let resp = Request::get(&format!("/api/models/search?q={query}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_quants(repo: &str) -> Result<serde_json::Value, String> {
    let resp = Request::get(&format!("/api/models/quants?repo={repo}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn fetch_cached_models() -> Result<serde_json::Value, String> {
    let resp = Request::get("/api/models/cached")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn pull_model(repo: &str, quant: Option<&str>) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "repo": repo, "quant": quant });
    let resp = Request::post("/api/models/pull")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

pub async fn update_profile(name: &str, data: &serde_json::Value) -> Result<serde_json::Value, String> {
    let resp = Request::put(&format!("/api/config/profile/{name}"))
        .json(data)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::debug;

// --- Types ---

/// A GitHub release (subset of fields from the API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: String,
    pub published_at: Option<DateTime<Utc>>,
    pub html_url: String,
    #[serde(default)]
    pub body: String,
}

/// Parsed version info from a llama-server or vLLM binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub raw: String,
    pub build_number: Option<u32>,
    pub commit_hash: Option<String>,
}

/// Cached release state for one tracked repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoReleaseState {
    pub repo: String,
    pub latest: Option<GitHubRelease>,
    pub current_version: Option<VersionInfo>,
    pub update_available: bool,
    pub ahead_of_release: bool,
    pub checked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

/// Full release cache, persisted to disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseCache {
    pub repos: Vec<RepoReleaseState>,
}

impl ReleaseCache {
    /// Load cache from disk, or return default if missing/corrupt.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Atomic save: write to .tmp then rename.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create cache dir: {e}"))?;
        }
        let tmp = path.with_extension("json.tmp");
        let data =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize cache: {e}"))?;
        std::fs::write(&tmp, &data).map_err(|e| format!("write cache tmp: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename cache: {e}"))?;
        Ok(())
    }

    /// Get the state for a specific repo, or None.
    pub fn get(&self, repo: &str) -> Option<&RepoReleaseState> {
        self.repos.iter().find(|r| r.repo == repo)
    }

    /// Get mutable state for a repo, creating if missing.
    pub fn get_or_insert(&mut self, repo: &str) -> &mut RepoReleaseState {
        if !self.repos.iter().any(|r| r.repo == repo) {
            self.repos.push(RepoReleaseState {
                repo: repo.to_string(),
                latest: None,
                current_version: None,
                update_available: false,
                ahead_of_release: false,
                checked_at: None,
                etag: None,
            });
        }
        self.repos.iter_mut().find(|r| r.repo == repo).unwrap()
    }

    /// Returns true if any tracked repo has an update available.
    pub fn has_updates(&self) -> bool {
        self.repos.iter().any(|r| r.update_available)
    }
}

// --- GitHub Client ---

pub struct GitHubClient {
    client: reqwest::Client,
}

impl GitHubClient {
    pub fn new(token: Option<&str>) -> Self {
        let mut builder = reqwest::Client::builder()
            .user_agent(format!("rookery/{}", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30));

        if let Some(tok) = token {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {tok}")) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
            builder = builder.default_headers(headers);
        }

        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());

        Self { client }
    }

    /// Fetch latest release for a repo. Returns None on 304 (cache hit).
    /// On success, returns the release and the new ETag.
    pub async fn latest_release(
        &self,
        repo: &str,
        etag: Option<&str>,
    ) -> Result<Option<(GitHubRelease, Option<String>)>, String> {
        let url = format!("https://api.github.com/repos/{repo}/releases/latest");

        let mut req = self.client.get(&url);
        if let Some(etag) = etag {
            req = req.header("If-None-Match", etag);
        }

        let resp = req.send().await.map_err(|e| format!("github api: {e}"))?;

        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            debug!("github {repo}: 304 not modified (cache hit)");
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("github {repo}: {status} — {body}"));
        }

        let new_etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let release: GitHubRelease = resp
            .json()
            .await
            .map_err(|e| format!("github {repo} parse: {e}"))?;

        Ok(Some((release, new_etag)))
    }
}

// --- Version Parsing ---

/// Parse llama-server version output or build_info string.
/// Accepts: "version: 8650 (43a4ee4a2)" or "b8650-43a4ee4a2" or "b8650"
pub fn parse_llama_build_info(raw: &str) -> VersionInfo {
    let raw = raw.trim().to_string();

    // Try "version: NNNN (HASH)"
    if let Some(rest) = raw.strip_prefix("version: ") {
        let parts: Vec<&str> = rest.splitn(2, ' ').collect();
        let build_number = parts.first().and_then(|s| s.parse::<u32>().ok());
        let commit_hash = parts
            .get(1)
            .map(|s| s.trim_matches(|c| c == '(' || c == ')').to_string());
        return VersionInfo {
            raw,
            build_number,
            commit_hash,
        };
    }

    // Try "bNNNN-HASH" or "bNNNN"
    let trimmed = raw.trim_start_matches('b');
    let parts: Vec<&str> = trimmed.splitn(2, '-').collect();
    let build_number = parts.first().and_then(|s| s.parse::<u32>().ok());
    let commit_hash = parts.get(1).map(|s| s.to_string());

    VersionInfo {
        raw,
        build_number,
        commit_hash,
    }
}

/// Extract build number from a llama.cpp release tag like "b8650".
pub fn parse_tag_build_number(tag: &str) -> Option<u32> {
    tag.trim_start_matches('b').parse::<u32>().ok()
}

/// Compare current version against latest release tag.
/// Returns (update_available, ahead_of_release).
pub fn compare_llama_versions(current: &VersionInfo, latest_tag: &str) -> (bool, bool) {
    let current_num = match current.build_number {
        Some(n) => n,
        None => return (false, false),
    };
    let latest_num = match parse_tag_build_number(latest_tag) {
        Some(n) => n,
        None => return (false, false),
    };

    if current_num < latest_num {
        (true, false)
    } else if current_num > latest_num {
        (false, true)
    } else {
        (false, false)
    }
}

/// Detect llama-server version by running `--version` and parsing stdout.
pub async fn detect_llama_version(binary_path: &Path) -> Result<VersionInfo, String> {
    let output = tokio::process::Command::new(binary_path)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("spawn llama-server --version: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Find the "version: NNNN (HASH)" line
    for line in stdout.lines() {
        if line.starts_with("version:") {
            return Ok(parse_llama_build_info(line));
        }
    }

    Err(format!(
        "could not parse version from llama-server output: {stdout}"
    ))
}

/// Detect llama-server version from the running server's /props endpoint.
pub async fn detect_llama_version_from_props(port: u16) -> Result<VersionInfo, String> {
    let url = format!("http://127.0.0.1:{port}/props");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build client: {e}"))?;

    let resp: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("props request: {e}"))?
        .json()
        .await
        .map_err(|e| format!("props parse: {e}"))?;

    let build_info = resp["build_info"]
        .as_str()
        .ok_or_else(|| "no build_info in /props".to_string())?;

    Ok(parse_llama_build_info(build_info))
}

/// Default cache file path.
pub fn default_cache_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("rookery")
        .join("releases.json")
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_line() {
        let v = parse_llama_build_info("version: 8650 (43a4ee4a2)");
        assert_eq!(v.build_number, Some(8650));
        assert_eq!(v.commit_hash.as_deref(), Some("43a4ee4a2"));
    }

    #[test]
    fn test_parse_build_info_tag_style() {
        let v = parse_llama_build_info("b8650-43a4ee4a2");
        assert_eq!(v.build_number, Some(8650));
        assert_eq!(v.commit_hash.as_deref(), Some("43a4ee4a2"));
    }

    #[test]
    fn test_parse_build_info_tag_only() {
        let v = parse_llama_build_info("b8646");
        assert_eq!(v.build_number, Some(8646));
        assert_eq!(v.commit_hash, None);
    }

    #[test]
    fn test_parse_tag_build_number() {
        assert_eq!(parse_tag_build_number("b8646"), Some(8646));
        assert_eq!(parse_tag_build_number("b8650"), Some(8650));
        assert_eq!(parse_tag_build_number("v0.16.0"), None);
    }

    #[test]
    fn test_compare_update_available() {
        let current = VersionInfo {
            raw: "b8640".into(),
            build_number: Some(8640),
            commit_hash: None,
        };
        assert_eq!(compare_llama_versions(&current, "b8650"), (true, false));
    }

    #[test]
    fn test_compare_ahead_of_release() {
        let current = VersionInfo {
            raw: "b8650".into(),
            build_number: Some(8650),
            commit_hash: None,
        };
        assert_eq!(compare_llama_versions(&current, "b8646"), (false, true));
    }

    #[test]
    fn test_compare_up_to_date() {
        let current = VersionInfo {
            raw: "b8650".into(),
            build_number: Some(8650),
            commit_hash: None,
        };
        assert_eq!(compare_llama_versions(&current, "b8650"), (false, false));
    }

    #[test]
    fn test_cache_roundtrip() {
        let mut cache = ReleaseCache::default();
        let state = cache.get_or_insert("ggml-org/llama.cpp");
        state.update_available = true;
        state.checked_at = Some(Utc::now());

        let tmp = std::env::temp_dir().join("rookery-test-releases.json");
        cache.save(&tmp).unwrap();
        let loaded = ReleaseCache::load(&tmp);
        assert_eq!(loaded.repos.len(), 1);
        assert!(loaded.repos[0].update_available);
        assert!(loaded.has_updates());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_cache_load_missing() {
        let cache = ReleaseCache::load(Path::new("/nonexistent/path.json"));
        assert!(cache.repos.is_empty());
    }
}

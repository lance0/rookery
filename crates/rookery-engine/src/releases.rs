use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::debug;

// --- Types ---

fn default_true() -> bool {
    true
}

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
    /// ggml-org ships `bNNNNN` build tags as prereleases alongside stable semver
    /// tags, so this is not a "skip it" flag — it is how the newest build is
    /// labelled. Kept so callers can show which kind a tag is.
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
}

/// Parsed version info from a llama-server or vLLM binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub raw: String,
    pub build_number: Option<u32>,
    pub commit_hash: Option<String>,
    /// Semantic version, when the binary reports one. llama.cpp started emitting
    /// this with v0.2.0 (2026-08-21); older builds only had a build number.
    #[serde(default)]
    pub semver: Option<(u32, u32, u32)>,
    /// Whether the reported version carried a prerelease/dev suffix (`0.3.0-dev`).
    ///
    /// Load-bearing for comparison, not cosmetic: llama.cpp stamps `X.Y.Z-dev` on
    /// master both BEFORE X.Y.Z ships and AFTER, so an equal-looking `-dev` build
    /// can sit on either side of the tag. Without this flag the suffix is dropped
    /// and a Sep-3 build 154 commits past v0.3.0 compares exactly equal to it.
    #[serde(default)]
    pub is_dev: bool,
}

/// Cached release state for one tracked repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoReleaseState {
    pub repo: String,
    pub latest: Option<GitHubRelease>,
    pub current_version: Option<VersionInfo>,
    pub update_available: bool,
    pub ahead_of_release: bool,
    /// False when the current version and the latest tag could not be compared
    /// like-for-like. `update_available: false` then means "unknown", NOT
    /// "up to date" -- render it as such. Defaults true so a cache written by an
    /// older build keeps its previous meaning until the next check refreshes it.
    #[serde(default = "default_true")]
    pub version_comparable: bool,
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

    /// Durable atomic save (write + fsync + rename + dir fsync).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let data =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize cache: {e}"))?;
        rookery_core::atomic::write_atomic(path, data.as_bytes())
            .map_err(|e| format!("write cache: {e}"))?;
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
                version_comparable: true,
                checked_at: None,
                etag: None,
            });
        }
        self.repos.iter_mut().find(|r| r.repo == repo).unwrap()
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
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(2));

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
    ///
    /// 🔴 Uses `/releases` (newest-first, prereleases INCLUDED), not
    /// `/releases/latest`, which returns only the newest NON-prerelease.
    ///
    /// ggml-org publishes both schemes at once: semver tags (`v0.3.0`) as stable
    /// and build tags (`b10778`) as prereleases. `/releases/latest` therefore
    /// reported `v0.3.0` — a tag from Aug 25 that PREDATES DFlash2 — as "latest"
    /// while b10778 sat 154 commits ahead of it, and a box running the newer
    /// build was told it was up to date. Picking the newest release of either
    /// kind is what makes that row mean anything.
    pub async fn latest_release(
        &self,
        repo: &str,
        etag: Option<&str>,
    ) -> Result<Option<(GitHubRelease, Option<String>)>, String> {
        let url = format!("https://api.github.com/repos/{repo}/releases?per_page=20");

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

        let releases: Vec<GitHubRelease> = resp
            .json()
            .await
            .map_err(|e| format!("github {repo} parse: {e}"))?;

        // GitHub returns newest-first by creation date, which is what we want:
        // it makes no assumption about which tag scheme is "higher", and a repo
        // publishing both semver and build tags is exactly the case that broke
        // the old comparison.
        let release = releases
            .into_iter()
            .find(|r| !r.draft)
            .ok_or_else(|| format!("github {repo}: no releases"))?;

        Ok(Some((release, new_etag)))
    }
}

// --- Version Parsing ---

/// Parse llama-server version output or build_info string.
/// Accepts: "version: 8650 (43a4ee4a2)" or "b8650-43a4ee4a2" or "b8650"
pub fn parse_llama_build_info(raw: &str) -> VersionInfo {
    let raw = raw.trim().to_string();

    // Try "version: 0.2.0-dev (build 10566, commit bb4caa754)" -- the format
    // llama.cpp switched to at v0.2.0. Semver first, build number from the
    // parenthesised part.
    if let Some(rest) = raw.strip_prefix("version: ")
        && let Some(semver) = parse_semver(rest.split_whitespace().next().unwrap_or(""))
    {
        let token = rest.split_whitespace().next().unwrap_or("");
        // `0.3.0-dev` vs `0.3.0` — the suffix decides whether this is a tagged
        // release or a build off master, which changes what a comparison means.
        let is_dev = token.contains('-') || token.contains('+');
        let build_number = rest
            .split("build ")
            .nth(1)
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|s| s.parse::<u32>().ok());
        let commit_hash = rest
            .split("commit ")
            .nth(1)
            .map(|s| s.trim_matches(|c| c == '(' || c == ')').trim().to_string());
        return VersionInfo {
            raw,
            build_number,
            commit_hash,
            semver: Some(semver),
            is_dev,
        };
    }

    // Try the older "version: NNNN (HASH)"
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
            semver: None,
            is_dev: false,
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
        semver: None,
        is_dev: false,
    }
}

/// Parse "0.2.0", "v0.2.0" or "0.2.0-dev" into (major, minor, patch).
///
/// The prerelease suffix is deliberately DISCARDED rather than ordered: llama.cpp
/// stamps `-dev` even on a build made from the release tag itself, so treating it
/// as "less than" the release would report an update forever while sitting exactly
/// on it. ponytail: costs us the ability to distinguish a real prerelease from its
/// release; revisit only if upstream starts shipping meaningful prereleases.
pub fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.');
    let major = it.next()?.parse::<u32>().ok()?;
    let minor = it.next()?.parse::<u32>().ok()?;
    let patch = it.next().unwrap_or("0").parse::<u32>().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Extract build number from a llama.cpp release tag like "b8650".
pub fn parse_tag_build_number(tag: &str) -> Option<u32> {
    tag.trim_start_matches('b').parse::<u32>().ok()
}

/// Compare current version against latest release tag.
///
/// Returns `Some((update_available, ahead_of_release))`, or **`None` when the two
/// cannot be compared like-for-like** -- an unrecognised tag scheme, or a binary
/// whose version string we could not parse.
///
/// `None` is not "up to date". Collapsing it into `false` is what let a semver
/// tag switch upstream silently report "up to date" while 193 commits behind;
/// callers must render it as unknown.
pub fn compare_llama_versions(current: &VersionInfo, latest_tag: &str) -> Option<(bool, bool)> {
    // Prefer semver when both sides speak it (llama.cpp >= v0.2.0, and vLLM's
    // vX.Y.Z tags, which never parsed as build numbers at all).
    if let (Some(cur), Some(latest)) = (current.semver, parse_semver(latest_tag)) {
        return Some((cur < latest, cur > latest));
    }

    // Fall back to build numbers for the legacy bNNNNN scheme.
    if let (Some(cur), Some(latest)) = (current.build_number, parse_tag_build_number(latest_tag)) {
        return Some((cur < latest, cur > latest));
    }

    None
}

/// Borrow the semver from a binary's `--version` banner for a version that only
/// carries a build number.
///
/// `/props` reports `build_info` as `bNNNNN-sha` with no semver, but the release
/// tags are now semver — so a running server can never be compared against a tag
/// on its own. The binary's banner has both halves.
///
/// Only merges when the two agree on the build number. A binary rebuilt but not
/// yet restarted into is a *different* build from the one actually serving, and
/// lending it its semver would report the running server as a version it is not.
/// Returns true if the merge happened.
pub fn borrow_semver_from_binary(running: &mut VersionInfo, binary: &VersionInfo) -> bool {
    if running.semver.is_some() || binary.semver.is_none() {
        return false;
    }
    match (running.build_number, binary.build_number) {
        (Some(a), Some(b)) if a == b => {
            running.semver = binary.semver;
            // Carry the dev flag with the semver it belongs to, or the running
            // server looks like a tagged release it is not.
            running.is_dev = binary.is_dev;
            true
        }
        _ => false,
    }
}

/// Detect llama-server version by running `--version`.
///
/// llama-server writes its build banner to **stderr**, not stdout — stdout comes
/// back empty. Scan both so this keeps working if that ever changes upstream.
pub async fn detect_llama_version(binary_path: &Path) -> Result<VersionInfo, String> {
    let output = tokio::process::Command::new(binary_path)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("spawn llama-server --version: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Find the "version: NNNN (HASH)" line
    for line in stdout.lines().chain(stderr.lines()) {
        if line.starts_with("version:") {
            return Ok(parse_llama_build_info(line));
        }
    }

    Err(format!(
        "could not parse version from llama-server output: {stdout}{stderr}"
    ))
}

/// Detect llama-server version from the running server's /props endpoint.
pub async fn detect_llama_version_from_props(port: u16) -> Result<VersionInfo, String> {
    let url = format!("http://127.0.0.1:{port}/props");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .connect_timeout(std::time::Duration::from_secs(2))
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
    fn dev_suffix_is_recorded_when_parsing_the_banner() {
        let v = parse_llama_build_info("version: 0.3.0-dev (build 1, commit 5ec4eab69)");
        assert!(v.is_dev, "0.3.0-dev must be flagged as a dev build");
        assert_eq!(v.semver, Some((0, 3, 0)));
        let rel = parse_llama_build_info("version: 0.3.0 (build 10700, commit abc123)");
        assert!(!rel.is_dev, "a tagged release must not be flagged dev");
    }

    /// Regression for the false "up to date" on 2026-09-03.
    ///
    /// The panel showed `current b1-5ec4eab69 -> latest v0.3.0  up to date` on a
    /// box running a Sep-3 build 154 commits PAST v0.3.0 — a build containing
    /// DFlash2, which v0.3.0 predates. Root cause was `/releases/latest`, which
    /// returns only the newest NON-prerelease; ggml-org ships bNNNNN builds as
    /// prereleases, so the real newest release was invisible.
    #[test]
    fn release_list_deserializes_prerelease_and_draft() {
        let body = r#"[
          {"tag_name":"b10786","name":"b10786","published_at":"2026-09-03T12:44:27Z",
           "html_url":"https://x","body":"","prerelease":true,"draft":false},
          {"tag_name":"v0.3.0","name":"v0.3.0","published_at":"2026-08-25T10:22:58Z",
           "html_url":"https://y","body":"","prerelease":false,"draft":false}
        ]"#;
        let rs: Vec<GitHubRelease> = serde_json::from_str(body).expect("must parse");
        assert_eq!(rs.len(), 2);
        assert!(
            rs[0].prerelease,
            "bNNNNN builds are published as prereleases"
        );
        assert!(!rs[0].draft);

        // Newest-first ordering is what we select on: the build tag must win
        // over the older stable tag, or the row reports a version that predates
        // what is already installed.
        let picked = rs.iter().find(|r| !r.draft).expect("a non-draft release");
        assert_eq!(picked.tag_name, "b10786");
    }

    /// Drafts are the one kind we skip: they are unpublished and not installable.
    #[test]
    fn drafts_are_skipped_but_prereleases_are_not() {
        let body = r#"[
          {"tag_name":"b99999","name":"d","published_at":null,"html_url":"https://x",
           "body":"","prerelease":true,"draft":true},
          {"tag_name":"b10786","name":"b","published_at":null,"html_url":"https://y",
           "body":"","prerelease":true,"draft":false}
        ]"#;
        let rs: Vec<GitHubRelease> = serde_json::from_str(body).unwrap();
        let picked = rs.iter().find(|r| !r.draft).unwrap();
        assert_eq!(picked.tag_name, "b10786");
    }

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
    fn test_parse_semver() {
        assert_eq!(parse_semver("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_semver("0.2.0"), Some((0, 2, 0)));
        // Prerelease and build metadata are discarded, not ordered.
        assert_eq!(parse_semver("0.2.0-dev"), Some((0, 2, 0)));
        assert_eq!(parse_semver("0.2.0+meta"), Some((0, 2, 0)));
        assert_eq!(parse_semver("v1.2"), Some((1, 2, 0)));
        assert_eq!(parse_semver("b10566"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver(""), None);
    }

    /// llama.cpp switched its --version banner at v0.2.0. Parsing the new shape
    /// is half of what stopped `rookery releases` reporting a false "up to date".
    #[test]
    fn test_parse_build_info_semver_banner() {
        let v = parse_llama_build_info("version: 0.2.0-dev (build 10566, commit bb4caa754)");
        assert_eq!(v.semver, Some((0, 2, 0)));
        assert_eq!(v.build_number, Some(10566));
        assert_eq!(v.commit_hash.as_deref(), Some("bb4caa754"));
    }

    /// Regression for the false "up to date": on 2026-08-21 ggml-org moved from
    /// bNNNNN tags to semver, and BOTH sides of the comparison stopped parsing --
    /// the banner (`0.2.0-dev` is not a u32) and the tag (`v0.2.0` is not
    /// `bNNNNN`). Each failure alone collapsed to (false, false), which the CLI
    /// rendered as "✓ up to date" while the box sat 193 commits behind.
    #[test]
    fn test_semver_tag_switch_does_not_report_up_to_date() {
        let current = parse_llama_build_info("version: 0.2.0-dev (build 10566, commit bb4caa754)");

        // Sitting exactly on the release: -dev must not read as "older".
        assert_eq!(
            compare_llama_versions(&current, "v0.2.0"),
            Some((false, false))
        );
        // A newer tag is now actually detected.
        assert_eq!(
            compare_llama_versions(&current, "v0.3.0"),
            Some((true, false))
        );
        assert_eq!(
            compare_llama_versions(&current, "v0.2.1"),
            Some((true, false))
        );
        // And an older one reads as ahead.
        assert_eq!(
            compare_llama_versions(&current, "v0.1.9"),
            Some((false, true))
        );
    }

    /// vLLM's vX.Y.Z tags never parsed as build numbers, so its row was
    /// permanently "up to date". Semver support fixes that repo too.
    #[test]
    fn test_semver_comparison_covers_vllm_style_tags() {
        let current = VersionInfo {
            is_dev: false,
            raw: "0.19.0".into(),
            build_number: None,
            commit_hash: None,
            semver: Some((0, 19, 0)),
        };
        assert_eq!(
            compare_llama_versions(&current, "v0.27.1"),
            Some((true, false))
        );
    }

    /// The whole point of the tri-state: genuinely incomparable inputs must be
    /// None, so callers can say "unknown" instead of inventing good news.
    #[test]
    fn test_incomparable_versions_return_none() {
        let semver_only = VersionInfo {
            is_dev: false,
            raw: "0.2.0".into(),
            build_number: None,
            commit_hash: None,
            semver: Some((0, 2, 0)),
        };
        // Semver binary vs legacy build-number tag: no common scheme.
        assert_eq!(compare_llama_versions(&semver_only, "b10600"), None);

        let unparseable = VersionInfo {
            is_dev: false,
            raw: "garbage".into(),
            build_number: None,
            commit_hash: None,
            semver: None,
        };
        assert_eq!(compare_llama_versions(&unparseable, "v0.2.0"), None);
        assert_eq!(compare_llama_versions(&unparseable, "b10600"), None);
    }

    /// The live case: /props gives "b10566-bb4caa754" (no semver) and the tag is
    /// "v0.2.0", so on its own the running server is incomparable forever. With
    /// the binary's banner merged in it resolves to a real answer.
    #[test]
    fn test_borrow_semver_makes_running_server_comparable() {
        let mut running = parse_llama_build_info("b10566-bb4caa754");
        assert_eq!(running.semver, None);
        assert_eq!(compare_llama_versions(&running, "v0.2.0"), None);

        let binary = parse_llama_build_info("version: 0.2.0-dev (build 10566, commit bb4caa754)");
        assert!(borrow_semver_from_binary(&mut running, &binary));
        assert_eq!(running.semver, Some((0, 2, 0)));
        assert_eq!(
            compare_llama_versions(&running, "v0.2.0"),
            Some((false, false))
        );
        assert_eq!(
            compare_llama_versions(&running, "v0.3.0"),
            Some((true, false))
        );
    }

    /// A binary rebuilt but not yet restarted into is a different build from the
    /// one serving. Lending its semver would describe the running server as a
    /// version it is not -- exactly the situation on this box between building
    /// v0.2.0 and swapping it in.
    #[test]
    fn test_borrow_semver_refuses_on_build_mismatch() {
        let mut running = parse_llama_build_info("b10380-0b1bad14f");
        let newer_binary =
            parse_llama_build_info("version: 0.2.0-dev (build 10566, commit bb4caa754)");
        assert!(!borrow_semver_from_binary(&mut running, &newer_binary));
        assert_eq!(running.semver, None);
        // Still honestly incomparable rather than wrongly "up to date".
        assert_eq!(compare_llama_versions(&running, "v0.2.0"), None);
    }

    #[test]
    fn test_borrow_semver_noop_when_already_present_or_absent() {
        // Already has semver: leave it alone.
        let mut has = parse_llama_build_info("version: 0.2.0-dev (build 10566, commit bb4caa754)");
        let other = parse_llama_build_info("version: 9.9.9-dev (build 10566, commit deadbeef)");
        assert!(!borrow_semver_from_binary(&mut has, &other));
        assert_eq!(has.semver, Some((0, 2, 0)));

        // Binary has no semver to give (both on the legacy scheme).
        let mut running = parse_llama_build_info("b10380-0b1bad14f");
        let legacy = parse_llama_build_info("version: 10380 (0b1bad14f)");
        assert!(!borrow_semver_from_binary(&mut running, &legacy));
    }

    /// An older cache has no version_comparable field; it must deserialize as
    /// true so an upgrade doesn't turn every row into "unknown".
    #[test]
    fn test_version_comparable_defaults_true_on_old_cache() {
        let json = r#"{"repos":[{"repo":"ggml-org/llama.cpp","latest":null,
            "current_version":null,"update_available":false,
            "ahead_of_release":false,"checked_at":null}]}"#;
        let cache: ReleaseCache = serde_json::from_str(json).expect("old cache should load");
        assert!(cache.repos[0].version_comparable);
    }

    #[test]
    fn test_compare_update_available() {
        let current = VersionInfo {
            is_dev: false,
            raw: "b8640".into(),
            build_number: Some(8640),
            commit_hash: None,
            semver: None,
        };
        assert_eq!(
            compare_llama_versions(&current, "b8650"),
            Some((true, false))
        );
    }

    #[test]
    fn test_compare_ahead_of_release() {
        let current = VersionInfo {
            is_dev: false,
            raw: "b8650".into(),
            build_number: Some(8650),
            commit_hash: None,
            semver: None,
        };
        assert_eq!(
            compare_llama_versions(&current, "b8646"),
            Some((false, true))
        );
    }

    #[test]
    fn test_compare_up_to_date() {
        let current = VersionInfo {
            is_dev: false,
            raw: "b8650".into(),
            build_number: Some(8650),
            commit_hash: None,
            semver: None,
        };
        assert_eq!(
            compare_llama_versions(&current, "b8650"),
            Some((false, false))
        );
    }

    #[test]
    fn test_cache_roundtrip() {
        let mut cache = ReleaseCache::default();
        let state = cache.get_or_insert("ggml-org/llama.cpp");
        state.update_available = true;
        state.checked_at = Some(Utc::now());

        // A fixed temp_dir() path is shared across concurrent `cargo test` runs
        // in different worktrees, and they clobber each other's cache file.
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("releases.json");
        cache.save(&tmp).unwrap();
        let loaded = ReleaseCache::load(&tmp);
        assert_eq!(loaded.repos.len(), 1);
        assert!(loaded.repos[0].update_available);
    }

    #[test]
    fn test_cache_load_missing() {
        let cache = ReleaseCache::load(Path::new("/nonexistent/path.json"));
        assert!(cache.repos.is_empty());
    }

    // ponytail: 192.0.2.1 is RFC 5737 TEST-NET-1, which blackholes SYNs on a
    // normal network — so without `connect_timeout` the kernel SYN retry loop
    // runs ~130s and this fails. Ceiling: on a network that rejects TEST-NET-1
    // instantly (ICMP unreachable / no route) it passes either way. Only the
    // elapsed time is asserted; whether the request errors is not our business.
    #[tokio::test]
    async fn test_github_client_bounds_connect_to_blackhole() {
        let started = std::time::Instant::now();
        let _ = GitHubClient::new(None)
            .client
            .get("http://192.0.2.1:81/")
            .send()
            .await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "connect to a blackholed peer took {elapsed:?}; connect_timeout is not applied"
        );
    }
}

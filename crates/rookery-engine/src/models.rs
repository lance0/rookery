use crate::hardware::{self, HardwareProfile, PerfEstimate};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

// --- Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfRepoInfo {
    pub id: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub likes: u64,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfFileEntry {
    #[serde(rename = "type")]
    pub file_type: String,
    pub path: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuantInfo {
    pub label: String,
    pub files: Vec<QuantFile>,
    pub total_bytes: u64,
    pub is_downloaded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perf_estimate: Option<PerfEstimate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuantFile {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CachedModel {
    pub repo: String,
    pub quant_label: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub repo: String,
    pub file: String,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuantRecommendation {
    pub label: String,
    pub total_bytes: u64,
    pub perf_estimate: PerfEstimate,
    pub reason: String,
}

// --- Quant preference ordering ---

const QUANT_PREFERENCE: &[&str] = &[
    "UD-Q4_K_XL",
    "UD-Q4_K_L",
    "UD-Q5_K_XL",
    "UD-Q5_K_L",
    "UD-Q6_K_XL",
    "Q8_0",
    "Q6_K_L",
    "Q6_K",
    "Q5_K_L",
    "Q5_K_M",
    "Q5_K_S",
    "Q4_K_L",
    "Q4_K_M",
    "Q4_K_S",
    "IQ4_XS",
    "IQ4_NL",
    "IQ3_M",
    "IQ3_S",
    "IQ3_XXS",
    "Q3_K_L",
    "Q3_K_M",
    "Q3_K_S",
    "IQ2_M",
    "IQ2_S",
    "IQ2_XS",
    "IQ2_XXS",
    "Q2_K",
    "BF16",
    "F16",
    "F32",
];

// --- Known quant patterns for extraction ---

const QUANT_PATTERNS: &[&str] = &[
    "UD-Q4_K_XL",
    "UD-Q4_K_L",
    "UD-Q5_K_XL",
    "UD-Q5_K_L",
    "UD-Q6_K_XL",
    "UD-Q4_K_M",
    "UD-Q4_K_S",
    "UD-Q5_K_M",
    "UD-Q5_K_S",
    "UD-IQ4_XS",
    "UD-IQ2_M",
    "UD-IQ2_S",
    "UD-IQ2_XS",
    "MXFP4_MOE",
    "MXFP4",
    "Q8_0",
    "Q6_K_L",
    "Q6_K",
    "Q5_K_L",
    "Q5_K_M",
    "Q5_K_S",
    "Q4_K_L",
    "Q4_K_M",
    "Q4_K_S",
    "Q4_0",
    "Q3_K_L",
    "Q3_K_M",
    "Q3_K_S",
    "Q2_K_L",
    "Q2_K",
    "Q2_K_S",
    "IQ4_XS",
    "IQ4_NL",
    "IQ3_XXS",
    "IQ3_XS",
    "IQ3_S",
    "IQ3_M",
    "IQ2_XXS",
    "IQ2_XS",
    "IQ2_S",
    "IQ2_M",
    "IQ1_S",
    "IQ1_M",
    "TQ1_0",
    "TQ2_0",
    "BF16",
    "F16",
    "F32",
];

// --- HfClient ---

pub struct HfClient {
    client: reqwest::Client,
}

impl Default for HfClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HfClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("rookery/0.4.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to create HTTP client");
        Self { client }
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<HfRepoInfo>, String> {
        let url = format!(
            "https://huggingface.co/api/models?search={}&filter=gguf&sort=downloads&direction=-1&limit={}",
            simple_encode(query),
            limit
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("HF API request failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("HF API returned {}", resp.status()));
        }

        resp.json::<Vec<HfRepoInfo>>()
            .await
            .map_err(|e| format!("failed to parse HF response: {e}"))
    }

    pub async fn list_files(&self, repo: &str) -> Result<Vec<HfFileEntry>, String> {
        let url = format!("https://huggingface.co/api/models/{}/tree/main", repo);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("HF API request failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!(
                "HF API returned {} for repo '{}'",
                resp.status(),
                repo
            ));
        }

        resp.json::<Vec<HfFileEntry>>()
            .await
            .map_err(|e| format!("failed to parse file listing: {e}"))
    }

    pub async fn download_file(
        &self,
        repo: &str,
        filename: &str,
        dest: &Path,
        progress_tx: Option<&tokio::sync::watch::Sender<DownloadProgress>>,
    ) -> Result<(), String> {
        let url = format!("https://huggingface.co/{}//resolve/main/{}", repo, filename);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("download request failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("download returned {}", resp.status()));
        }

        let total = resp.content_length().unwrap_or(0);

        // Ensure parent directory exists
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create cache dir: {e}"))?;
        }

        let part_path = dest.with_extension("gguf.part");
        let mut file = tokio::fs::File::create(&part_path)
            .await
            .map_err(|e| format!("failed to create temp file: {e}"))?;

        let mut downloaded: u64 = 0;
        let mut stream = resp.bytes_stream();

        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("download stream error: {e}"))?;
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("write error: {e}"))?;
            downloaded += chunk.len() as u64;

            if let Some(tx) = &progress_tx {
                let _ = tx.send(DownloadProgress {
                    repo: repo.to_string(),
                    file: filename.to_string(),
                    bytes_downloaded: downloaded,
                    bytes_total: total,
                    done: false,
                });
            }
        }

        file.flush()
            .await
            .map_err(|e| format!("flush error: {e}"))?;
        drop(file);

        // Rename .part to final
        tokio::fs::rename(&part_path, dest)
            .await
            .map_err(|e| format!("rename error: {e}"))?;

        if let Some(tx) = &progress_tx {
            let _ = tx.send(DownloadProgress {
                repo: repo.to_string(),
                file: filename.to_string(),
                bytes_downloaded: downloaded,
                bytes_total: total,
                done: true,
            });
        }

        Ok(())
    }
}

// --- Pure functions ---

/// Normalize a repo input: bare names get `unsloth/` prefix,
/// names without `-GGUF` get it appended.
pub fn normalize_repo(input: &str) -> String {
    let mut repo = input.to_string();
    if !repo.contains('/') {
        repo = format!("unsloth/{repo}");
    }
    if !repo.to_uppercase().ends_with("-GGUF") {
        repo = format!("{repo}-GGUF");
    }
    repo
}

/// Extract quant variants from a list of HF file entries.
pub fn extract_quants(files: &[HfFileEntry]) -> Vec<QuantInfo> {
    let mut quant_map: std::collections::HashMap<String, Vec<QuantFile>> =
        std::collections::HashMap::new();

    for file in files {
        if file.file_type != "file" {
            continue;
        }
        let path = &file.path;
        if !path.ends_with(".gguf") {
            continue;
        }
        // Skip vision projectors
        let basename = path.rsplit('/').next().unwrap_or(path);
        if basename.starts_with("mmproj") {
            continue;
        }

        let label = extract_quant_label(basename);

        quant_map.entry(label).or_default().push(QuantFile {
            path: path.clone(),
            size: file.size,
        });
    }

    let mut quants: Vec<QuantInfo> = quant_map
        .into_iter()
        .map(|(label, files)| {
            let total_bytes: u64 = files.iter().map(|f| f.size).sum();
            QuantInfo {
                label,
                files,
                total_bytes,
                is_downloaded: false,
                perf_estimate: None,
            }
        })
        .collect();

    // Sort by preference
    quants.sort_by(|a, b| {
        let a_idx = quant_preference_index(&a.label);
        let b_idx = quant_preference_index(&b.label);
        a_idx.cmp(&b_idx)
    });

    quants
}

/// Extract quant label from a GGUF filename.
fn extract_quant_label(filename: &str) -> String {
    // Remove .gguf extension
    let base = filename.strip_suffix(".gguf").unwrap_or(filename);

    // Remove shard suffix like -00001-of-00003
    let base = if let Some(pos) = base.rfind("-00") {
        // Check if this looks like a shard pattern
        let rest = &base[pos..];
        if rest.contains("-of-") {
            &base[..pos]
        } else {
            base
        }
    } else {
        base
    };

    // Try to match known quant patterns (longest first for correct matching)
    let mut sorted_patterns: Vec<&&str> = QUANT_PATTERNS.iter().collect();
    sorted_patterns.sort_by_key(|b| std::cmp::Reverse(b.len()));

    for pattern in sorted_patterns {
        if base.contains(pattern) {
            return pattern.to_string();
        }
    }

    // Fallback: use the last segment after the last dash
    base.rsplit('-').next().unwrap_or(base).to_string()
}

fn quant_preference_index(label: &str) -> usize {
    QUANT_PREFERENCE
        .iter()
        .position(|&p| p == label)
        .unwrap_or(QUANT_PREFERENCE.len())
}

/// Scan the llama.cpp cache for downloaded GGUF files.
pub fn scan_cache() -> Vec<CachedModel> {
    let cache_dir = llama_cache_dir();
    if !cache_dir.exists() {
        return Vec::new();
    }

    let mut models = Vec::new();

    let entries = match std::fs::read_dir(&cache_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        if !name.ends_with(".gguf") || name.ends_with(".gguf.part") {
            continue;
        }

        // Parse llama.cpp cache naming: {owner}_{repo}_{filename}.gguf
        // The repo uses underscores instead of slashes
        if let Some((repo, quant_label)) = parse_cache_filename(name) {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            models.push(CachedModel {
                repo,
                quant_label,
                path: path.clone(),
                size_bytes: size,
            });
        }
    }

    models.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.quant_label.cmp(&b.quant_label)));
    models
}

/// Parse a llama.cpp cache filename into (repo, quant_label).
/// Format: `owner_repo_modelname-quant.gguf`
fn parse_cache_filename(filename: &str) -> Option<(String, String)> {
    let base = filename.strip_suffix(".gguf")?;

    // The convention is {owner}_{reponame}_{rest}.gguf
    // First underscore separates owner from repo
    let first_underscore = base.find('_')?;
    let owner = &base[..first_underscore];
    let rest = &base[first_underscore + 1..];

    // Second underscore separates repo from filename
    let second_underscore = rest.find('_')?;
    let repo_name = &rest[..second_underscore];
    let model_file = &rest[second_underscore + 1..];

    let repo = format!("{owner}/{repo_name}");
    let quant_label = extract_quant_label(model_file);

    Some((repo, quant_label))
}

/// Compute the llama.cpp cache path for a given repo and filename.
pub fn cache_path(repo: &str, filename: &str) -> PathBuf {
    let cache_name = format!("{}_{}", repo.replace('/', "_"), filename);
    llama_cache_dir().join(cache_name)
}

fn simple_encode(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('+', "%2B")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('#', "%23")
}

fn llama_cache_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".cache").join("llama.cpp")
    } else {
        PathBuf::from("/tmp/llama-cache")
    }
}

/// Mark quants as downloaded by cross-referencing with the local cache.
pub fn mark_downloaded(quants: &mut [QuantInfo]) {
    let cached = scan_cache();
    for quant in quants.iter_mut() {
        quant.is_downloaded = cached.iter().any(|c| c.quant_label == quant.label);
    }
}

/// Attach performance estimates to each quant.
pub fn attach_estimates(
    quants: &mut [QuantInfo],
    profile: &HardwareProfile,
    vram_free_mb: u64,
    ram_free_mb: u64,
) {
    for quant in quants.iter_mut() {
        let model_size_mb = quant.total_bytes / (1024 * 1024);
        quant.perf_estimate = Some(hardware::estimate_performance(
            profile,
            model_size_mb,
            vram_free_mb,
            ram_free_mb,
        ));
    }
}

/// Recommend the best quant for the user's hardware.
pub fn recommend_quant(
    quants: &[QuantInfo],
    profile: &HardwareProfile,
    vram_free_mb: u64,
    ram_free_mb: u64,
) -> Option<QuantRecommendation> {
    // Find the best quant that fits fully in GPU
    let best_full_gpu = quants.iter().find(|q| {
        let model_mb = q.total_bytes / (1024 * 1024);
        let needed = (model_mb as f64 * 1.15) as u64;
        vram_free_mb >= needed
    });

    if let Some(q) = best_full_gpu {
        let model_mb = q.total_bytes / (1024 * 1024);
        let est = hardware::estimate_performance(profile, model_mb, vram_free_mb, ram_free_mb);
        return Some(QuantRecommendation {
            label: q.label.clone(),
            total_bytes: q.total_bytes,
            perf_estimate: est,
            reason: format!(
                "best quality that fits fully in GPU ({:.1}GB model, {:.1}GB VRAM free)",
                q.total_bytes as f64 / 1_073_741_824.0,
                vram_free_mb as f64 / 1024.0
            ),
        });
    }

    // Fallback: find one that fits with partial offload
    let best_partial = quants.iter().find(|q| {
        let model_mb = q.total_bytes / (1024 * 1024);
        let needed = (model_mb as f64 * 1.15) as u64;
        vram_free_mb + ram_free_mb >= needed
    });

    if let Some(q) = best_partial {
        let model_mb = q.total_bytes / (1024 * 1024);
        let est = hardware::estimate_performance(profile, model_mb, vram_free_mb, ram_free_mb);
        return Some(QuantRecommendation {
            label: q.label.clone(),
            total_bytes: q.total_bytes,
            perf_estimate: est,
            reason: format!(
                "fits with partial CPU offload ({:.1}GB model)",
                q.total_bytes as f64 / 1_073_741_824.0
            ),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_repo() {
        assert_eq!(normalize_repo("Qwen3-8B"), "unsloth/Qwen3-8B-GGUF");
        assert_eq!(
            normalize_repo("unsloth/Qwen3-8B-GGUF"),
            "unsloth/Qwen3-8B-GGUF"
        );
        assert_eq!(
            normalize_repo("bartowski/Qwen3-8B"),
            "bartowski/Qwen3-8B-GGUF"
        );
    }

    #[test]
    fn test_extract_quant_label() {
        assert_eq!(extract_quant_label("Model-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(extract_quant_label("Model-UD-Q4_K_XL.gguf"), "UD-Q4_K_XL");
        assert_eq!(extract_quant_label("Model-BF16.gguf"), "BF16");
        assert_eq!(
            extract_quant_label("Model-Q8_0-00001-of-00003.gguf"),
            "Q8_0"
        );
        assert_eq!(extract_quant_label("Model-IQ4_XS.gguf"), "IQ4_XS");
    }

    #[test]
    fn test_extract_quants() {
        let files = vec![
            HfFileEntry {
                file_type: "file".into(),
                path: "Model-Q4_K_M.gguf".into(),
                size: 5_000_000_000,
            },
            HfFileEntry {
                file_type: "file".into(),
                path: "Model-Q8_0-00001-of-00002.gguf".into(),
                size: 8_000_000_000,
            },
            HfFileEntry {
                file_type: "file".into(),
                path: "Model-Q8_0-00002-of-00002.gguf".into(),
                size: 8_000_000_000,
            },
            HfFileEntry {
                file_type: "file".into(),
                path: "mmproj-BF16.gguf".into(),
                size: 500_000_000,
            },
            HfFileEntry {
                file_type: "file".into(),
                path: "README.md".into(),
                size: 1000,
            },
        ];

        let quants = extract_quants(&files);
        assert_eq!(quants.len(), 2);

        // Q8_0 should be first (higher preference)
        assert_eq!(quants[0].label, "Q8_0");
        assert_eq!(quants[0].total_bytes, 16_000_000_000);
        assert_eq!(quants[0].files.len(), 2);

        assert_eq!(quants[1].label, "Q4_K_M");
        assert_eq!(quants[1].total_bytes, 5_000_000_000);
    }

    #[test]
    fn test_cache_path() {
        let path = cache_path("unsloth/Qwen3-8B-GGUF", "Qwen3-8B-Q4_K_M.gguf");
        assert!(
            path.to_str()
                .unwrap()
                .ends_with("unsloth_Qwen3-8B-GGUF_Qwen3-8B-Q4_K_M.gguf")
        );
    }

    #[test]
    fn test_recommend_quant_picks_highest_quality_that_fits() {
        use crate::hardware::{CpuProfile, GpuProfile, HardwareProfile};

        let profile = HardwareProfile {
            gpu: Some(GpuProfile {
                name: "RTX 4090".into(),
                vram_total_mb: 24576,
                compute_capability: (8, 9),
                memory_bandwidth_gbps: 1008.0,
            }),
            cpu: CpuProfile {
                name: "test".into(),
                cores: 8,
                threads: 16,
                ram_total_mb: 65536,
            },
        };

        // Create quants ordered by preference (higher quality first, larger)
        let quants = vec![
            QuantInfo {
                label: "Q8_0".into(),
                files: vec![],
                total_bytes: 10 * 1024 * 1024 * 1024, // 10 GB — too big
                is_downloaded: false,
                perf_estimate: None,
            },
            QuantInfo {
                label: "Q6_K".into(),
                files: vec![],
                total_bytes: 7 * 1024 * 1024 * 1024, // 7 GB — fits
                is_downloaded: false,
                perf_estimate: None,
            },
            QuantInfo {
                label: "Q4_K_M".into(),
                files: vec![],
                total_bytes: 5 * 1024 * 1024 * 1024, // 5 GB — fits
                is_downloaded: false,
                perf_estimate: None,
            },
        ];

        // 8192 MB VRAM free: Q8_0 needs ~10*1.15=11.5 GB (~11776 MB) — doesn't fit
        // Q6_K needs ~7*1.15=8.05 GB (~8243 MB) — fits in 8192 MB
        let rec = recommend_quant(&quants, &profile, 8500, 32000).unwrap();
        assert_eq!(rec.label, "Q6_K");
    }

    #[test]
    fn test_recommend_quant_returns_none_when_nothing_fits() {
        use crate::hardware::{CpuProfile, GpuProfile, HardwareProfile};

        let profile = HardwareProfile {
            gpu: Some(GpuProfile {
                name: "RTX 3060".into(),
                vram_total_mb: 12288,
                compute_capability: (8, 6),
                memory_bandwidth_gbps: 360.0,
            }),
            cpu: CpuProfile {
                name: "test".into(),
                cores: 4,
                threads: 8,
                ram_total_mb: 8192,
            },
        };

        let quants = vec![QuantInfo {
            label: "Q4_K_M".into(),
            files: vec![],
            total_bytes: 30 * 1024 * 1024 * 1024, // 30 GB — way too big
            is_downloaded: false,
            perf_estimate: None,
        }];

        // Only 2 GB VRAM + 4 GB RAM = 6 GB total, model needs ~34.5 GB
        let rec = recommend_quant(&quants, &profile, 2048, 4096);
        assert!(rec.is_none());
    }

    #[test]
    fn test_quant_preference_ordering_ud_variants_first() {
        // Verify UD variants appear before their non-UD counterparts
        let ud_idx = QUANT_PREFERENCE
            .iter()
            .position(|&p| p == "UD-Q4_K_XL")
            .unwrap();
        let q8_idx = QUANT_PREFERENCE.iter().position(|&p| p == "Q8_0").unwrap();
        let q4_km_idx = QUANT_PREFERENCE
            .iter()
            .position(|&p| p == "Q4_K_M")
            .unwrap();
        let f16_idx = QUANT_PREFERENCE.iter().position(|&p| p == "F16").unwrap();

        assert!(ud_idx < q8_idx, "UD variants should come before Q8_0");
        assert!(q8_idx < q4_km_idx, "Q8_0 should come before Q4_K_M");
        assert!(q4_km_idx < f16_idx, "Q4_K_M should come before F16");
    }

    #[test]
    fn test_scan_cache_with_empty_directory() {
        // scan_cache() reads from ~/.cache/llama.cpp which may or may not exist
        // This test verifies it doesn't panic and returns a Vec
        let models = scan_cache();
        // Just verify it returns without panic — content depends on local cache
        let _ = models.len();
    }

    #[test]
    fn test_normalize_repo_already_normalized() {
        // Already has owner/ prefix and -GGUF suffix
        assert_eq!(
            normalize_repo("bartowski/Llama-3-8B-GGUF"),
            "bartowski/Llama-3-8B-GGUF"
        );
    }

    #[test]
    fn test_normalize_repo_bare_name() {
        // Bare model name with no slash and no -GGUF
        assert_eq!(normalize_repo("Qwen3-8B"), "unsloth/Qwen3-8B-GGUF");
    }

    #[test]
    fn test_normalize_repo_with_owner_no_gguf() {
        // Has owner/ but no -GGUF suffix
        assert_eq!(
            normalize_repo("bartowski/Qwen3-8B"),
            "bartowski/Qwen3-8B-GGUF"
        );
    }

    #[test]
    fn test_normalize_repo_case_insensitive_gguf_suffix() {
        // The function uses to_uppercase().ends_with("-GGUF"), so lowercase
        // "-gguf" is also recognized as having the GGUF suffix
        assert_eq!(normalize_repo("test/model-gguf"), "test/model-gguf");
        assert_eq!(normalize_repo("test/model-GGUF"), "test/model-GGUF");
        assert_eq!(normalize_repo("test/model-Gguf"), "test/model-Gguf");
    }

    #[test]
    fn test_recommend_quant_partial_offload_fallback() {
        use crate::hardware::{CpuProfile, FitMode, GpuProfile, HardwareProfile};

        let profile = HardwareProfile {
            gpu: Some(GpuProfile {
                name: "RTX 4090".into(),
                vram_total_mb: 24576,
                compute_capability: (8, 9),
                memory_bandwidth_gbps: 1008.0,
            }),
            cpu: CpuProfile {
                name: "test".into(),
                cores: 8,
                threads: 16,
                ram_total_mb: 65536,
            },
        };

        let quants = vec![QuantInfo {
            label: "Q4_K_M".into(),
            files: vec![],
            total_bytes: 20 * 1024 * 1024 * 1024, // 20 GB
            is_downloaded: false,
            perf_estimate: None,
        }];

        // VRAM = 10 GB (too small for full GPU: 20*1.15=23 GB)
        // but VRAM + RAM = 10 + 20 = 30 GB > 23 GB → partial offload
        let rec = recommend_quant(&quants, &profile, 10240, 20480).unwrap();
        assert_eq!(rec.label, "Q4_K_M");
        assert_eq!(rec.perf_estimate.fit_mode, FitMode::PartialOffload);
    }
}

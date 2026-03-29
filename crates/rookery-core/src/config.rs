use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::error::{Error, Result};

// ── Backend type enum ────────────────────────────────────────────────

/// Which inference backend a profile uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendType {
    #[default]
    LlamaServer,
    Vllm,
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendType::LlamaServer => write!(f, "llama-server"),
            BackendType::Vllm => write!(f, "vllm"),
        }
    }
}

// ── Backend-specific config structs ──────────────────────────────────

/// llama-server backend configuration (the `[profiles.<name>.llama_server]` sub-table).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlamaServerConfig {
    #[serde(default = "default_ctx_size")]
    pub ctx_size: u32,

    #[serde(default = "default_threads")]
    pub threads: u8,

    #[serde(default = "default_threads_batch")]
    pub threads_batch: u8,

    #[serde(default = "default_batch_size")]
    pub batch_size: u32,

    #[serde(default = "default_ubatch_size")]
    pub ubatch_size: u32,

    #[serde(default = "default_gpu_layers")]
    pub gpu_layers: i32,

    #[serde(default)]
    pub gpu_index: Option<u32>,

    #[serde(default = "default_cache_type")]
    pub cache_type_k: String,

    #[serde(default = "default_cache_type")]
    pub cache_type_v: String,

    #[serde(default = "default_true")]
    pub flash_attention: bool,

    #[serde(default)]
    pub reasoning_budget: i32,

    #[serde(default)]
    pub chat_template: Option<PathBuf>,

    #[serde(default = "default_temp")]
    pub temp: f32,

    #[serde(default = "default_top_p")]
    pub top_p: f32,

    #[serde(default = "default_top_k")]
    pub top_k: u32,

    #[serde(default)]
    pub min_p: f32,

    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl Default for LlamaServerConfig {
    fn default() -> Self {
        Self {
            ctx_size: default_ctx_size(),
            threads: default_threads(),
            threads_batch: default_threads_batch(),
            batch_size: default_batch_size(),
            ubatch_size: default_ubatch_size(),
            gpu_layers: default_gpu_layers(),
            gpu_index: None,
            cache_type_k: default_cache_type(),
            cache_type_v: default_cache_type(),
            flash_attention: default_true(),
            reasoning_budget: 0,
            chat_template: None,
            temp: default_temp(),
            top_p: default_top_p(),
            top_k: default_top_k(),
            min_p: 0.0,
            extra_args: Vec::new(),
        }
    }
}

/// vLLM backend configuration (the `[profiles.<name>.vllm]` sub-table).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VllmConfig {
    pub docker_image: String,

    #[serde(default = "default_gpu_memory_utilization")]
    pub gpu_memory_utilization: f64,

    #[serde(default)]
    pub max_num_seqs: Option<u32>,

    #[serde(default)]
    pub max_num_batched_tokens: Option<u32>,

    #[serde(default)]
    pub max_model_len: Option<u64>,

    #[serde(default)]
    pub quantization: Option<String>,

    #[serde(default)]
    pub tool_call_parser: Option<String>,

    #[serde(default)]
    pub kv_cache_dtype: Option<String>,

    #[serde(default)]
    pub extra_args: Vec<String>,
}

fn default_gpu_memory_utilization() -> f64 {
    0.9
}

// ── Config ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Path to the llama-server binary. Optional when only vLLM profiles exist.
    #[serde(default)]
    pub llama_server: PathBuf,

    #[serde(default = "default_profile")]
    pub default_profile: String,

    #[serde(default = "default_listen")]
    pub listen: SocketAddr,

    #[serde(default)]
    pub idle_timeout: Option<u32>,

    #[serde(default)]
    pub models: HashMap<String, Model>,

    #[serde(default)]
    pub profiles: HashMap<String, Profile>,

    #[serde(default)]
    pub agents: HashMap<String, AgentConfig>,
}

fn default_profile() -> String {
    "default".into()
}

fn default_listen() -> SocketAddr {
    "127.0.0.1:3000".parse().unwrap()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub source: String, // "hf" or "local"

    #[serde(default)]
    pub repo: Option<String>,

    #[serde(default)]
    pub file: Option<String>,

    #[serde(default)]
    pub path: Option<PathBuf>,

    #[serde(default)]
    pub estimated_vram_mb: Option<u32>,
}

// ── Profile ──────────────────────────────────────────────────────────

/// A profile combines a model reference with backend-specific configuration.
///
/// Common fields (`model`, `port`) live directly on the profile.
/// Backend-specific fields live in exactly one sub-table:
///   - `[profiles.<name>.llama_server]` for llama-server
///   - `[profiles.<name>.vllm]` for vLLM
///
/// For backward compatibility, legacy flat fields (ctx_size, threads, etc.)
/// are still accepted directly on the profile and treated as LlamaServerConfig.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    // ── Common fields ────────────────────────────────────────────────
    pub model: String,

    #[serde(default = "default_port")]
    pub port: u16,

    // ── Backend sub-tables (new format) ──────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llama_server: Option<LlamaServerConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vllm: Option<VllmConfig>,

    // ── Legacy flat fields (backward compat with old configs) ────────
    // These are only used when no sub-table is present.
    // When llama_server sub-table IS present, these are ignored.
    #[serde(
        default = "default_ctx_size",
        skip_serializing_if = "is_default_ctx_size"
    )]
    pub ctx_size: u32,

    #[serde(default = "default_threads")]
    pub threads: u8,

    #[serde(default = "default_threads_batch")]
    pub threads_batch: u8,

    #[serde(default = "default_batch_size")]
    pub batch_size: u32,

    #[serde(default = "default_ubatch_size")]
    pub ubatch_size: u32,

    #[serde(default = "default_gpu_layers")]
    pub gpu_layers: i32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_index: Option<u32>,

    #[serde(default = "default_cache_type")]
    pub cache_type_k: String,

    #[serde(default = "default_cache_type")]
    pub cache_type_v: String,

    #[serde(default = "default_true")]
    pub flash_attention: bool,

    #[serde(default)]
    pub reasoning_budget: i32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template: Option<PathBuf>,

    #[serde(default = "default_temp")]
    pub temp: f32,

    #[serde(default = "default_top_p")]
    pub top_p: f32,

    #[serde(default = "default_top_k")]
    pub top_k: u32,

    #[serde(default)]
    pub min_p: f32,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

impl Profile {
    /// Returns which backend this profile uses.
    ///
    /// - If `vllm` sub-table is present → `Vllm`
    /// - If `llama_server` sub-table is present, OR no sub-table (legacy) → `LlamaServer`
    pub fn backend_type(&self) -> BackendType {
        if self.vllm.is_some() {
            BackendType::Vllm
        } else {
            BackendType::LlamaServer
        }
    }

    /// Returns the effective LlamaServerConfig for this profile.
    ///
    /// If an explicit `[llama_server]` sub-table exists, returns it.
    /// Otherwise, constructs one from the legacy flat fields.
    pub fn llama_server_config(&self) -> Option<LlamaServerConfig> {
        if self.vllm.is_some() {
            return None;
        }

        if let Some(ref config) = self.llama_server {
            Some(config.clone())
        } else {
            // Legacy flat fields → construct LlamaServerConfig
            Some(LlamaServerConfig {
                ctx_size: self.ctx_size,
                threads: self.threads,
                threads_batch: self.threads_batch,
                batch_size: self.batch_size,
                ubatch_size: self.ubatch_size,
                gpu_layers: self.gpu_layers,
                gpu_index: self.gpu_index,
                cache_type_k: self.cache_type_k.clone(),
                cache_type_v: self.cache_type_v.clone(),
                flash_attention: self.flash_attention,
                reasoning_budget: self.reasoning_budget,
                chat_template: self.chat_template.clone(),
                temp: self.temp,
                top_p: self.top_p,
                top_k: self.top_k,
                min_p: self.min_p,
                extra_args: self.extra_args.clone(),
            })
        }
    }

    /// Returns the effective VllmConfig for this profile, if it's a vLLM profile.
    pub fn vllm_config(&self) -> Option<&VllmConfig> {
        self.vllm.as_ref()
    }
}

// ── skip_serializing_if helpers ──────────────────────────────────────

fn is_default_ctx_size(v: &u32) -> bool {
    *v == default_ctx_size()
}

// ── Agent config ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Path to the agent binary/command
    pub command: String,

    /// Arguments to pass when starting the agent
    #[serde(default)]
    pub args: Vec<String>,

    /// Working directory for the agent process
    #[serde(default)]
    pub workdir: Option<PathBuf>,

    /// Environment variables to set for the agent
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Whether to auto-start this agent when the daemon starts
    #[serde(default)]
    pub auto_start: bool,

    /// Whether to restart the agent when the model is swapped
    #[serde(default = "default_true")]
    pub restart_on_swap: bool,

    /// Whether to auto-restart the agent if it crashes or exits unexpectedly
    #[serde(default)]
    pub restart_on_crash: bool,

    /// Port this agent depends on (e.g. llama-server on 8081). If the server
    /// on this port restarts (down→up transition), the watchdog bounces the agent.
    #[serde(default)]
    pub depends_on_port: Option<u16>,

    /// Path to a pyproject.toml or Cargo.toml to extract the agent's version.
    #[serde(default)]
    pub version_file: Option<PathBuf>,

    /// Shell command used to update the agent in place.
    #[serde(default)]
    pub update_command: Option<String>,

    /// Working directory used for the update command. Falls back to `workdir`.
    #[serde(default)]
    pub update_workdir: Option<PathBuf>,

    /// Stderr patterns that trigger an immediate restart (case-insensitive substring match).
    /// Example: ["telegram.error.TimedOut", "ReadTimeout", "CLOSE_WAIT"]
    #[serde(default)]
    pub restart_on_error_patterns: Vec<String>,
}

// ── Defaults ─────────────────────────────────────────────────────────

fn default_port() -> u16 {
    8081
}
fn default_ctx_size() -> u32 {
    262144
}
fn default_threads() -> u8 {
    4
}
fn default_threads_batch() -> u8 {
    24
}
fn default_batch_size() -> u32 {
    4096
}
fn default_ubatch_size() -> u32 {
    1024
}
fn default_gpu_layers() -> i32 {
    -1
}
fn default_cache_type() -> String {
    "q8_0".into()
}
fn default_true() -> bool {
    true
}
fn default_temp() -> f32 {
    0.7
}
fn default_top_p() -> f32 {
    0.8
}
fn default_top_k() -> u32 {
    20
}

// ── Config implementation ────────────────────────────────────────────

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Err(Error::ConfigNotFound(path));
        }
        let content = std::fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn config_path() -> PathBuf {
        if let Some(config_dir) = dirs::config_dir() {
            config_dir.join("rookery").join("config.toml")
        } else {
            PathBuf::from("/etc/rookery/config.toml")
        }
    }

    /// Returns true if any profile uses llama-server as its backend.
    fn has_llama_server_profiles(&self) -> bool {
        self.profiles
            .values()
            .any(|p| p.backend_type() == BackendType::LlamaServer)
    }

    pub fn validate(&self) -> Result<()> {
        // Only require llama_server binary when there are llama-server profiles
        if self.has_llama_server_profiles() {
            if self.llama_server.as_os_str().is_empty() {
                return Err(Error::ConfigValidation(
                    "llama_server path is required when llama-server profiles exist".into(),
                ));
            }
            if !self.llama_server.exists() {
                return Err(Error::BinaryNotFound(self.llama_server.clone()));
            }
        }

        for (name, profile) in &self.profiles {
            if !self.models.contains_key(&profile.model) {
                return Err(Error::InvalidModelRef {
                    profile: name.clone(),
                    model: profile.model.clone(),
                });
            }

            // Reject profiles with both sub-tables
            if profile.llama_server.is_some() && profile.vllm.is_some() {
                return Err(Error::ConfigValidation(format!(
                    "profile '{name}' has both llama_server and vllm sub-tables — exactly one backend must be specified"
                )));
            }

            // Validate vLLM-specific fields
            if let Some(ref vllm) = profile.vllm {
                let gpu_mem = vllm.gpu_memory_utilization;
                if gpu_mem <= 0.0 || gpu_mem > 1.0 {
                    return Err(Error::ConfigValidation(format!(
                        "profile '{name}': gpu_memory_utilization must be in (0.0, 1.0], got {gpu_mem}"
                    )));
                }
            }
        }

        if !self.profiles.contains_key(&self.default_profile) {
            return Err(Error::ProfileNotFound(self.default_profile.clone()));
        }

        Ok(())
    }

    pub fn resolve_command_line(&self, profile_name: &str) -> Result<Vec<String>> {
        let profile = self
            .profiles
            .get(profile_name)
            .ok_or_else(|| Error::ProfileNotFound(profile_name.into()))?;

        let model = self
            .models
            .get(&profile.model)
            .ok_or_else(|| Error::ModelNotFound(profile.model.clone()))?;

        match profile.backend_type() {
            BackendType::LlamaServer => self.resolve_llama_server_command_line(profile, model),
            BackendType::Vllm => self.resolve_vllm_command_line(profile, model),
        }
    }

    /// Build command line for llama-server backend.
    fn resolve_llama_server_command_line(
        &self,
        profile: &Profile,
        model: &Model,
    ) -> Result<Vec<String>> {
        let ls = profile.llama_server_config().unwrap_or_default();

        let mut args = vec![self.llama_server.to_string_lossy().into_owned()];

        // Model source
        match model.source.as_str() {
            "hf" => {
                if let (Some(repo), Some(file)) = (&model.repo, &model.file) {
                    args.extend(["-hf".into(), format!("{repo}:{file}")]);
                }
            }
            "local" => {
                if let Some(path) = &model.path {
                    args.extend(["-m".into(), path.to_string_lossy().into_owned()]);
                }
            }
            _ => {}
        }

        // Template
        args.push("--jinja".into());
        if let Some(template) = &ls.chat_template {
            args.extend([
                "--chat-template-file".into(),
                template.to_string_lossy().into_owned(),
            ]);
        }

        // Reasoning
        args.extend(["--reasoning-budget".into(), ls.reasoning_budget.to_string()]);

        // Threading
        args.extend(["--threads".into(), ls.threads.to_string()]);
        args.extend(["--threads-batch".into(), ls.threads_batch.to_string()]);

        // Context and batching
        args.extend(["--ctx-size".into(), ls.ctx_size.to_string()]);
        args.extend(["--batch-size".into(), ls.batch_size.to_string()]);
        args.extend(["--ubatch-size".into(), ls.ubatch_size.to_string()]);

        // KV cache
        args.extend(["--cache-type-k".into(), ls.cache_type_k.clone()]);
        args.extend(["--cache-type-v".into(), ls.cache_type_v.clone()]);

        // Parallelism
        args.extend(["--parallel".into(), "1".into()]);

        // Sampling
        args.extend(["--temp".into(), ls.temp.to_string()]);
        args.extend(["--top-p".into(), ls.top_p.to_string()]);
        args.extend(["--top-k".into(), ls.top_k.to_string()]);
        args.extend(["--min-p".into(), ls.min_p.to_string()]);
        args.extend(["--repeat-penalty".into(), "1.0".into()]);

        // Flash attention
        if ls.flash_attention {
            args.extend(["-fa".into(), "on".into()]);
        }

        // Network
        args.extend(["--port".into(), profile.port.to_string()]);
        args.extend(["--host".into(), "0.0.0.0".into()]);

        // GPU
        let ngl = if ls.gpu_layers < 0 { 99 } else { ls.gpu_layers };
        args.extend(["-ngl".into(), ngl.to_string()]);

        // Extra args passthrough
        args.extend(ls.extra_args.clone());

        Ok(args)
    }

    /// Build command line arguments for vLLM backend (used by compose generation).
    fn resolve_vllm_command_line(&self, profile: &Profile, model: &Model) -> Result<Vec<String>> {
        let vllm = profile
            .vllm
            .as_ref()
            .ok_or_else(|| Error::ConfigValidation("vLLM profile missing vllm sub-table".into()))?;

        let mut args = vec![vllm.docker_image.clone()];

        // Model — vLLM uses the HuggingFace repo ID directly
        if let Some(repo) = &model.repo {
            args.extend(["--model".into(), repo.clone()]);
        }

        // GPU memory
        args.extend([
            "--gpu-memory-utilization".into(),
            vllm.gpu_memory_utilization.to_string(),
        ]);

        // Port (internal container port, always 8000 for vLLM)
        args.extend(["--port".into(), "8000".into()]);

        // Optional vLLM-specific flags
        if let Some(max_seqs) = vllm.max_num_seqs {
            args.extend(["--max-num-seqs".into(), max_seqs.to_string()]);
        }
        if let Some(max_batched) = vllm.max_num_batched_tokens {
            args.extend(["--max-num-batched-tokens".into(), max_batched.to_string()]);
        }
        if let Some(max_len) = vllm.max_model_len {
            args.extend(["--max-model-len".into(), max_len.to_string()]);
        }
        if let Some(ref quant) = vllm.quantization {
            args.extend(["--quantization".into(), quant.clone()]);
        }
        if let Some(ref parser) = vllm.tool_call_parser {
            args.extend(["--tool-call-parser".into(), parser.clone()]);
        }
        if let Some(ref kv_dtype) = vllm.kv_cache_dtype {
            args.extend(["--kv-cache-dtype".into(), kv_dtype.clone()]);
        }

        // Extra args passthrough
        args.extend(vllm.extra_args.clone());

        Ok(args)
    }

    pub fn resolve_profile_name<'a>(&'a self, name: Option<&'a str>) -> &'a str {
        let name = name.unwrap_or(&self.default_profile);
        for (profile_name, profile) in &self.profiles {
            if profile.aliases.iter().any(|a| a == name) {
                return profile_name;
            }
        }
        name
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::config_path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        let content = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Parse llama-server profile with sub-table
    #[test]
    fn test_config_parse_llama_server_subtable() {
        let toml_str = r#"
llama_server = "/usr/local/bin/llama-server"
default_profile = "fast"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 25800

[profiles.fast]
model = "qwen35"
port = 8081

[profiles.fast.llama_server]
ctx_size = 131072
threads = 8
threads_batch = 16
batch_size = 2048
ubatch_size = 512
gpu_layers = -1
cache_type_k = "f16"
cache_type_v = "f16"
flash_attention = false
reasoning_budget = 10
chat_template = "/path/to/template.jinja"
temp = 0.5
top_p = 0.9
top_k = 40
min_p = 0.1
extra_args = ["--verbose"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let profile = &config.profiles["fast"];

        assert_eq!(profile.backend_type(), BackendType::LlamaServer);
        assert_eq!(profile.port, 8081);
        assert_eq!(profile.model, "qwen35");

        let ls = profile.llama_server_config().unwrap();
        assert_eq!(ls.ctx_size, 131072);
        assert_eq!(ls.threads, 8);
        assert_eq!(ls.threads_batch, 16);
        assert_eq!(ls.batch_size, 2048);
        assert_eq!(ls.ubatch_size, 512);
        assert_eq!(ls.gpu_layers, -1);
        assert_eq!(ls.cache_type_k, "f16");
        assert_eq!(ls.cache_type_v, "f16");
        assert!(!ls.flash_attention);
        assert_eq!(ls.reasoning_budget, 10);
        assert_eq!(
            ls.chat_template,
            Some(PathBuf::from("/path/to/template.jinja"))
        );
        assert_eq!(ls.temp, 0.5);
        assert_eq!(ls.top_p, 0.9);
        assert_eq!(ls.top_k, 40);
        assert_eq!(ls.min_p, 0.1);
        assert_eq!(ls.extra_args, vec!["--verbose".to_string()]);
    }

    // Parse vLLM profile with sub-table
    #[test]
    fn test_config_parse_vllm_subtable() {
        let toml_str = r#"
default_profile = "vllm_prod"

[models.qwen_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"
estimated_vram_mb = 20000

[profiles.vllm_prod]
model = "qwen_nvfp4"
port = 8081

[profiles.vllm_prod.vllm]
docker_image = "vllm/vllm-openai:cu130-nightly"
gpu_memory_utilization = 0.89
max_num_seqs = 4
max_num_batched_tokens = 4096
max_model_len = 234567
quantization = "awq_marlin"
tool_call_parser = "qwen3_coder"
kv_cache_dtype = "fp8"
extra_args = ["--enable-chunked-prefill"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let profile = &config.profiles["vllm_prod"];

        assert_eq!(profile.backend_type(), BackendType::Vllm);
        assert_eq!(profile.port, 8081);

        let vllm = profile.vllm_config().unwrap();
        assert_eq!(vllm.docker_image, "vllm/vllm-openai:cu130-nightly");
        assert_eq!(vllm.gpu_memory_utilization, 0.89);
        assert_eq!(vllm.max_num_seqs, Some(4));
        assert_eq!(vllm.max_num_batched_tokens, Some(4096));
        assert_eq!(vllm.max_model_len, Some(234567));
        assert_eq!(vllm.quantization.as_deref(), Some("awq_marlin"));
        assert_eq!(vllm.tool_call_parser.as_deref(), Some("qwen3_coder"));
        assert_eq!(vllm.kv_cache_dtype.as_deref(), Some("fp8"));
        assert_eq!(
            vllm.extra_args,
            vec!["--enable-chunked-prefill".to_string()]
        );
    }

    // Backward compatibility — flat profile defaults to llama-server
    #[test]
    fn test_config_backward_compat_flat_profile() {
        let toml_str = r#"
llama_server = "/usr/local/bin/llama-server"
default_profile = "fast"
listen = "127.0.0.1:3000"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 25800

[profiles.fast]
model = "qwen35"
port = 8081
ctx_size = 262144
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let profile = &config.profiles["fast"];

        assert_eq!(profile.backend_type(), BackendType::LlamaServer);
        assert!(profile.llama_server.is_none(), "no explicit sub-table");
        assert!(profile.vllm.is_none());

        // Legacy flat fields should be accessible via llama_server_config()
        let ls = profile.llama_server_config().unwrap();
        assert_eq!(ls.ctx_size, 262144);
        assert_eq!(ls.threads, 4); // default
        assert_eq!(ls.threads_batch, 24); // default
        assert_eq!(ls.batch_size, 4096); // default
        assert_eq!(ls.ubatch_size, 1024); // default
        assert_eq!(ls.gpu_layers, -1); // default
        assert_eq!(ls.cache_type_k, "q8_0"); // default
        assert_eq!(ls.cache_type_v, "q8_0"); // default
        assert!(ls.flash_attention); // default true
        assert_eq!(ls.temp, 0.7); // default
        assert_eq!(ls.top_p, 0.8); // default
        assert_eq!(ls.top_k, 20); // default
        assert_eq!(ls.min_p, 0.0); // default
    }

    // Default values for vLLM sub-table fields
    #[test]
    fn test_config_vllm_defaults() {
        let toml_str = r#"
default_profile = "vllm_min"

[models.test_model]
source = "hf"
repo = "test/model"

[profiles.vllm_min]
model = "test_model"
port = 8081

[profiles.vllm_min.vllm]
docker_image = "vllm/vllm-openai:latest"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let vllm = config.profiles["vllm_min"].vllm_config().unwrap();

        assert_eq!(vllm.docker_image, "vllm/vllm-openai:latest");
        assert_eq!(vllm.gpu_memory_utilization, 0.9); // default
        assert_eq!(vllm.max_num_seqs, None);
        assert_eq!(vllm.max_num_batched_tokens, None);
        assert_eq!(vllm.max_model_len, None);
        assert_eq!(vllm.quantization, None);
        assert_eq!(vllm.tool_call_parser, None);
        assert_eq!(vllm.kv_cache_dtype, None);
        assert!(vllm.extra_args.is_empty());
    }

    // Config with mixed backend profiles parses successfully
    #[test]
    fn test_config_mixed_backends() {
        let toml_str = r#"
llama_server = "/usr/bin/llama-server"
default_profile = "llama_fast"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"

[models.qwen_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"

[profiles.llama_fast]
model = "qwen35"
port = 8081

[profiles.llama_fast.llama_server]
ctx_size = 262144
threads = 4

[profiles.vllm_prod]
model = "qwen_nvfp4"
port = 8082

[profiles.vllm_prod.vllm]
docker_image = "vllm/vllm-openai:latest"
gpu_memory_utilization = 0.85
"#;
        let config: Config = toml::from_str(toml_str).unwrap();

        let llama = &config.profiles["llama_fast"];
        assert_eq!(llama.backend_type(), BackendType::LlamaServer);
        assert_eq!(llama.llama_server_config().unwrap().ctx_size, 262144);

        let vllm = &config.profiles["vllm_prod"];
        assert_eq!(vllm.backend_type(), BackendType::Vllm);
        assert_eq!(vllm.vllm_config().unwrap().gpu_memory_utilization, 0.85);
    }

    // vLLM model source — repo without file field
    #[test]
    fn test_config_vllm_model_hf_repo_no_file() {
        let toml_str = r#"
default_profile = "vllm_prod"

[models.qwen_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"

[profiles.vllm_prod]
model = "qwen_nvfp4"
port = 8081

[profiles.vllm_prod.vllm]
docker_image = "vllm/vllm-openai:latest"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let model = &config.models["qwen_nvfp4"];
        assert_eq!(model.source, "hf");
        assert_eq!(model.repo.as_deref(), Some("kaitchup/Qwen3.5-27B-NVFP4"));
        assert!(model.file.is_none(), "vLLM models don't need a file field");

        // Should validate without error
        config.validate().unwrap();
    }

    // Validation rejects profile with both backend sub-tables
    #[test]
    fn test_config_validate_rejects_dual_backend() {
        let toml_str = r#"
llama_server = "/usr/bin/llama-server"
default_profile = "bad"

[models.test]
source = "hf"
repo = "test/model"
file = "test.gguf"

[profiles.bad]
model = "test"
port = 8081

[profiles.bad.llama_server]
ctx_size = 4096

[profiles.bad.vllm]
docker_image = "vllm/vllm-openai:latest"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("both llama_server and vllm"),
            "error should mention both sub-tables: {msg}"
        );
    }

    // Validation rejects vLLM profile missing docker_image
    #[test]
    fn test_config_validate_rejects_vllm_missing_docker_image() {
        // docker_image is a required field in VllmConfig, so this should fail at parse time
        let toml_str = r#"
default_profile = "bad"

[models.test]
source = "hf"
repo = "test/model"

[profiles.bad]
model = "test"
port = 8081

[profiles.bad.vllm]
gpu_memory_utilization = 0.9
"#;
        let result: std::result::Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err(), "should fail: docker_image is required");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("docker_image"),
            "error should mention docker_image: {err}"
        );
    }

    // Validation rejects invalid gpu_memory_utilization range
    #[test]
    fn test_config_validate_gpu_memory_utilization_boundaries() {
        let make_config = |gpu_mem: f64| -> Config {
            Config {
                llama_server: PathBuf::new(),
                default_profile: "v".into(),
                listen: default_listen(),
                idle_timeout: None,
                models: HashMap::from([(
                    "m".into(),
                    Model {
                        source: "hf".into(),
                        repo: Some("test/model".into()),
                        file: None,
                        path: None,
                        estimated_vram_mb: None,
                    },
                )]),
                profiles: HashMap::from([(
                    "v".into(),
                    Profile {
                        model: "m".into(),
                        port: 8081,
                        llama_server: None,
                        vllm: Some(VllmConfig {
                            docker_image: "vllm/vllm-openai:latest".into(),
                            gpu_memory_utilization: gpu_mem,
                            max_num_seqs: None,
                            max_num_batched_tokens: None,
                            max_model_len: None,
                            quantization: None,
                            tool_call_parser: None,
                            kv_cache_dtype: None,
                            extra_args: Vec::new(),
                        }),
                        // Legacy fields (unused for vLLM)
                        ctx_size: default_ctx_size(),
                        threads: default_threads(),
                        threads_batch: default_threads_batch(),
                        batch_size: default_batch_size(),
                        ubatch_size: default_ubatch_size(),
                        gpu_layers: default_gpu_layers(),
                        gpu_index: None,
                        cache_type_k: default_cache_type(),
                        cache_type_v: default_cache_type(),
                        flash_attention: default_true(),
                        reasoning_budget: 0,
                        chat_template: None,
                        temp: default_temp(),
                        top_p: default_top_p(),
                        top_k: default_top_k(),
                        min_p: 0.0,
                        aliases: Vec::new(),
                        extra_args: Vec::new(),
                    },
                )]),
                agents: HashMap::new(),
            }
        };

        // 0.0 → reject
        assert!(make_config(0.0).validate().is_err());
        // -0.1 → reject
        assert!(make_config(-0.1).validate().is_err());
        // 1.0 → accept
        assert!(make_config(1.0).validate().is_ok());
        // 1.01 → reject
        assert!(make_config(1.01).validate().is_err());
        // 0.5 → accept
        assert!(make_config(0.5).validate().is_ok());
    }

    // resolve_command_line for vLLM produces Docker command
    #[test]
    fn test_resolve_vllm_command_line() {
        let toml_str = r#"
default_profile = "vllm_prod"

[models.qwen_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"

[profiles.vllm_prod]
model = "qwen_nvfp4"
port = 8081

[profiles.vllm_prod.vllm]
docker_image = "vllm/vllm-openai:cu130-nightly"
gpu_memory_utilization = 0.89
max_num_seqs = 4
max_num_batched_tokens = 4096
max_model_len = 234567
quantization = "awq_marlin"
tool_call_parser = "qwen3_coder"
kv_cache_dtype = "fp8"
extra_args = ["--enable-chunked-prefill"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let args = config.resolve_command_line("vllm_prod").unwrap();

        // docker_image should be the first element (matching llama-server binary path convention)
        assert_eq!(
            args[0], "vllm/vllm-openai:cu130-nightly",
            "docker_image should be args[0]"
        );

        // Should contain vLLM-specific args
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"kaitchup/Qwen3.5-27B-NVFP4".to_string()));
        assert!(args.contains(&"--gpu-memory-utilization".to_string()));
        assert!(args.contains(&"0.89".to_string()));
        assert!(args.contains(&"--port".to_string()));
        assert!(args.contains(&"8000".to_string())); // internal vLLM port
        assert!(args.contains(&"--max-num-seqs".to_string()));
        assert!(args.contains(&"4".to_string()));
        assert!(args.contains(&"--max-num-batched-tokens".to_string()));
        assert!(args.contains(&"--max-model-len".to_string()));
        assert!(args.contains(&"--quantization".to_string()));
        assert!(args.contains(&"awq_marlin".to_string()));
        assert!(args.contains(&"--tool-call-parser".to_string()));
        assert!(args.contains(&"--kv-cache-dtype".to_string()));
        assert!(args.contains(&"--enable-chunked-prefill".to_string()));

        // Should NOT contain llama-server-specific args
        assert!(!args.contains(&"--ctx-size".to_string()));
        assert!(!args.contains(&"--threads".to_string()));
        assert!(!args.contains(&"-ngl".to_string()));
        assert!(!args.contains(&"-fa".to_string()));
        assert!(!args.contains(&"--jinja".to_string()));
    }

    // vLLM command omits optional params when None
    #[test]
    fn test_resolve_vllm_command_line_minimal() {
        let toml_str = r#"
default_profile = "vllm_min"

[models.test_model]
source = "hf"
repo = "test/model"

[profiles.vllm_min]
model = "test_model"
port = 8081

[profiles.vllm_min.vllm]
docker_image = "vllm/vllm-openai:latest"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let args = config.resolve_command_line("vllm_min").unwrap();

        // docker_image should be the first element
        assert_eq!(
            args[0], "vllm/vllm-openai:latest",
            "docker_image should be args[0]"
        );

        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"--gpu-memory-utilization".to_string()));
        assert!(args.contains(&"0.9".to_string())); // default

        // Optional params should be absent
        assert!(!args.contains(&"--max-num-seqs".to_string()));
        assert!(!args.contains(&"--max-num-batched-tokens".to_string()));
        assert!(!args.contains(&"--max-model-len".to_string()));
        assert!(!args.contains(&"--quantization".to_string()));
        assert!(!args.contains(&"--tool-call-parser".to_string()));
        assert!(!args.contains(&"--kv-cache-dtype".to_string()));
    }

    // Config serialization roundtrip preserves all fields
    #[test]
    fn test_config_serialization_roundtrip() {
        // Test with explicit llama_server sub-table
        let toml_str = r#"
llama_server = "/usr/bin/llama-server"
default_profile = "llama_fast"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 25800

[models.qwen_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"

[profiles.llama_fast]
model = "qwen35"
port = 8081

[profiles.llama_fast.llama_server]
ctx_size = 131072
threads = 8
threads_batch = 16
flash_attention = true
temp = 0.5

[profiles.vllm_prod]
model = "qwen_nvfp4"
port = 8082

[profiles.vllm_prod.vllm]
docker_image = "vllm/vllm-openai:latest"
gpu_memory_utilization = 0.85
max_num_seqs = 4
quantization = "awq_marlin"
extra_args = ["--enable-chunked-prefill"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();

        // Serialize and deserialize
        let serialized = toml::to_string_pretty(&config).unwrap();
        let restored: Config = toml::from_str(&serialized).unwrap();

        // Verify llama-server profile roundtrips
        let llama = &restored.profiles["llama_fast"];
        assert_eq!(llama.backend_type(), BackendType::LlamaServer);
        let ls = llama.llama_server.as_ref().unwrap();
        assert_eq!(ls.ctx_size, 131072);
        assert_eq!(ls.threads, 8);
        assert_eq!(ls.threads_batch, 16);
        assert!(ls.flash_attention);
        assert_eq!(ls.temp, 0.5);

        // Verify vLLM profile roundtrips
        let vllm_profile = &restored.profiles["vllm_prod"];
        assert_eq!(vllm_profile.backend_type(), BackendType::Vllm);
        let vllm = vllm_profile.vllm.as_ref().unwrap();
        assert_eq!(vllm.docker_image, "vllm/vllm-openai:latest");
        assert_eq!(vllm.gpu_memory_utilization, 0.85);
        assert_eq!(vllm.max_num_seqs, Some(4));
        assert_eq!(vllm.quantization.as_deref(), Some("awq_marlin"));
        assert_eq!(
            vllm.extra_args,
            vec!["--enable-chunked-prefill".to_string()]
        );

        // Verify models roundtrip
        assert_eq!(
            restored.models["qwen35"].file.as_deref(),
            Some("UD-Q4_K_XL")
        );
        assert_eq!(restored.models["qwen_nvfp4"].file, None);
    }

    // Legacy flat profile roundtrip
    #[test]
    fn test_config_serialization_roundtrip_flat_profile() {
        let toml_str = r#"
llama_server = "/usr/bin/llama-server"
default_profile = "fast"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"

[profiles.fast]
model = "qwen35"
port = 8081
ctx_size = 131072
threads = 8
temp = 0.5
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let restored: Config = toml::from_str(&serialized).unwrap();

        let profile = &restored.profiles["fast"];
        assert_eq!(profile.backend_type(), BackendType::LlamaServer);

        let ls = profile.llama_server_config().unwrap();
        assert_eq!(ls.ctx_size, 131072);
        assert_eq!(ls.threads, 8);
        assert_eq!(ls.temp, 0.5);
    }

    // config.example.toml parses and contains both backend types
    #[test]
    fn test_config_example_toml_parses() {
        let example_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config.example.toml");
        let content = std::fs::read_to_string(&example_path)
            .unwrap_or_else(|e| panic!("failed to read config.example.toml: {e}"));

        let config: Config = toml::from_str(&content)
            .unwrap_or_else(|e| panic!("failed to parse config.example.toml: {e}"));

        // Verify it has at least one of each backend type
        let has_llama = config
            .profiles
            .values()
            .any(|p| p.backend_type() == BackendType::LlamaServer);
        let has_vllm = config
            .profiles
            .values()
            .any(|p| p.backend_type() == BackendType::Vllm);

        assert!(
            has_llama,
            "config.example.toml should have a llama-server profile"
        );
        assert!(has_vllm, "config.example.toml should have a vLLM profile");
    }

    // llama_server binary path not required for vLLM-only configs
    #[test]
    fn test_config_vllm_only_no_llama_binary_required() {
        let toml_str = r#"
default_profile = "vllm_only"

[models.test_model]
source = "hf"
repo = "test/model"

[profiles.vllm_only]
model = "test_model"
port = 8081

[profiles.vllm_only.vllm]
docker_image = "vllm/vllm-openai:latest"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        // llama_server field defaults to empty PathBuf, no binary check needed
        assert!(config.llama_server.as_os_str().is_empty());
        config.validate().unwrap(); // should NOT error
    }

    // validate() errors when llama-server profiles exist but path is empty
    #[test]
    fn test_config_validate_llama_empty_path_with_llama_profiles() {
        let toml_str = r#"
default_profile = "fast"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"

[profiles.fast]
model = "qwen35"
port = 8081
ctx_size = 262144
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        // llama_server path is empty (omitted from config)
        assert!(config.llama_server.as_os_str().is_empty());
        // Should error because llama-server profiles exist
        let err = config.validate().unwrap_err();
        assert!(
            matches!(err, Error::ConfigValidation(ref msg) if msg.contains("llama_server path is required")),
            "should error on empty llama_server path with llama-server profiles: {err}"
        );
    }

    // Config validation checks llama_server binary for llama-server profiles
    #[test]
    fn test_config_validate_llama_binary_required_for_llama_profiles() {
        let toml_str = r#"
llama_server = "/nonexistent/path/to/llama-server"
default_profile = "fast"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"

[profiles.fast]
model = "qwen35"
port = 8081
ctx_size = 262144
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(
            matches!(err, Error::BinaryNotFound(_)),
            "should error on missing llama-server binary: {err}"
        );
    }

    // vLLM-only config with missing binary passes
    #[test]
    fn test_config_validate_vllm_only_missing_binary_passes() {
        let toml_str = r#"
llama_server = "/nonexistent/path/to/llama-server"
default_profile = "vllm_only"

[models.test_model]
source = "hf"
repo = "test/model"

[profiles.vllm_only]
model = "test_model"
port = 8081

[profiles.vllm_only.vllm]
docker_image = "vllm/vllm-openai:latest"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        // No llama-server profiles, so binary check should be skipped
        config.validate().unwrap();
    }

    // === Existing test: backward compat parse
    #[test]
    fn test_parse_config() {
        let toml_str = r#"
llama_server = "/usr/local/bin/llama-server"
default_profile = "fast"
listen = "127.0.0.1:3000"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 25800

[profiles.fast]
model = "qwen35"
port = 8081
ctx_size = 262144
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_profile, "fast");
        assert_eq!(config.models.len(), 1);
        assert_eq!(config.profiles["fast"].port, 8081);
        assert_eq!(config.profiles["fast"].ctx_size, 262144);
        // Check defaults
        assert_eq!(config.profiles["fast"].threads, 4);
        assert_eq!(config.profiles["fast"].threads_batch, 24);
    }

    // === Existing test: backward compat resolve_command_line
    #[test]
    fn test_resolve_command_line() {
        let toml_str = r#"
llama_server = "/tmp/fake-llama-server"
default_profile = "fast"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"

[profiles.fast]
model = "qwen35"
port = 8081
ctx_size = 262144
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let args = config.resolve_command_line("fast").unwrap();
        assert!(args.contains(&"-hf".to_string()));
        assert!(args.contains(&"unsloth/Qwen3.5-35B-A3B-GGUF:UD-Q4_K_XL".to_string()));
        assert!(args.contains(&"--port".to_string()));
        assert!(args.contains(&"8081".to_string()));
        assert!(args.contains(&"--jinja".to_string()));
    }

    // === BackendType serde tests
    #[test]
    fn test_backend_type_serde() {
        // Serialize
        let llama = serde_json::to_string(&BackendType::LlamaServer).unwrap();
        assert_eq!(llama, "\"llama_server\"");
        let vllm = serde_json::to_string(&BackendType::Vllm).unwrap();
        assert_eq!(vllm, "\"vllm\"");

        // Deserialize
        let restored: BackendType = serde_json::from_str("\"llama_server\"").unwrap();
        assert_eq!(restored, BackendType::LlamaServer);
        let restored: BackendType = serde_json::from_str("\"vllm\"").unwrap();
        assert_eq!(restored, BackendType::Vllm);

        // Default
        assert_eq!(BackendType::default(), BackendType::LlamaServer);
    }

    // === BackendType Display
    #[test]
    fn test_backend_type_display() {
        assert_eq!(BackendType::LlamaServer.to_string(), "llama-server");
        assert_eq!(BackendType::Vllm.to_string(), "vllm");
    }

    // Config edge cases

    #[test]
    fn test_config_save_load_roundtrip_via_filesystem() {
        // Write config to a temp file and read it back — does NOT use Config::save()
        // or Config::load() which hardcode XDG paths and can clobber real config.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let config = Config {
            llama_server: PathBuf::from("/usr/bin/llama-server"),
            default_profile: "fast".into(),
            listen: "127.0.0.1:3000".parse().unwrap(),
            idle_timeout: None,
            models: HashMap::from([(
                "qwen".into(),
                Model {
                    source: "hf".into(),
                    repo: Some("unsloth/Qwen3-8B-GGUF".into()),
                    file: Some("Q4_K_M.gguf".into()),
                    path: None,
                    estimated_vram_mb: Some(5000),
                },
            )]),
            profiles: HashMap::from([(
                "fast".into(),
                Profile {
                    model: "qwen".into(),
                    port: 8081,
                    llama_server: Some(LlamaServerConfig {
                        ctx_size: 131072,
                        ..LlamaServerConfig::default()
                    }),
                    vllm: None,
                    ctx_size: default_ctx_size(),
                    threads: default_threads(),
                    threads_batch: default_threads_batch(),
                    batch_size: default_batch_size(),
                    ubatch_size: default_ubatch_size(),
                    gpu_layers: default_gpu_layers(),
                    gpu_index: None,
                    cache_type_k: default_cache_type(),
                    cache_type_v: default_cache_type(),
                    flash_attention: default_true(),
                    reasoning_budget: 0,
                    chat_template: None,
                    temp: default_temp(),
                    top_p: default_top_p(),
                    top_k: default_top_k(),
                    min_p: 0.0,
                    aliases: Vec::new(),
                    extra_args: Vec::new(),
                },
            )]),
            agents: HashMap::new(),
        };

        // Serialize and write to temp file (same logic as Config::save but safe path)
        let content = toml::to_string_pretty(&config).unwrap();
        std::fs::write(&config_path, &content).unwrap();

        // Read back and deserialize
        let loaded_str = std::fs::read_to_string(&config_path).unwrap();
        let loaded: Config = toml::from_str(&loaded_str).unwrap();

        assert_eq!(loaded.default_profile, "fast");
        assert_eq!(loaded.models["qwen"].estimated_vram_mb, Some(5000));
        let ls = loaded.profiles["fast"].llama_server_config().unwrap();
        assert_eq!(ls.ctx_size, 131072);
    }

    #[test]
    fn test_validate_rejects_missing_default_profile() {
        let config = Config {
            llama_server: PathBuf::new(),
            default_profile: "nonexistent_profile".into(),
            listen: "127.0.0.1:3000".parse().unwrap(),
            idle_timeout: None,
            models: HashMap::from([(
                "m".into(),
                Model {
                    source: "hf".into(),
                    repo: Some("test/model".into()),
                    file: None,
                    path: None,
                    estimated_vram_mb: None,
                },
            )]),
            profiles: HashMap::from([(
                "existing".into(),
                Profile {
                    model: "m".into(),
                    port: 8081,
                    llama_server: None,
                    vllm: Some(VllmConfig {
                        docker_image: "vllm/vllm-openai:latest".into(),
                        gpu_memory_utilization: 0.9,
                        max_num_seqs: None,
                        max_num_batched_tokens: None,
                        max_model_len: None,
                        quantization: None,
                        tool_call_parser: None,
                        kv_cache_dtype: None,
                        extra_args: Vec::new(),
                    }),
                    ctx_size: default_ctx_size(),
                    threads: default_threads(),
                    threads_batch: default_threads_batch(),
                    batch_size: default_batch_size(),
                    ubatch_size: default_ubatch_size(),
                    gpu_layers: default_gpu_layers(),
                    gpu_index: None,
                    cache_type_k: default_cache_type(),
                    cache_type_v: default_cache_type(),
                    flash_attention: default_true(),
                    reasoning_budget: 0,
                    chat_template: None,
                    temp: default_temp(),
                    top_p: default_top_p(),
                    top_k: default_top_k(),
                    min_p: 0.0,
                    aliases: Vec::new(),
                    extra_args: Vec::new(),
                },
            )]),
            agents: HashMap::new(),
        };

        let err = config.validate().unwrap_err();
        assert!(
            matches!(err, crate::error::Error::ProfileNotFound(ref name) if name == "nonexistent_profile"),
            "should reject missing default_profile: {err}"
        );
    }

    #[test]
    fn test_resolve_profile_name_none_returns_default() {
        let toml_str = r#"
default_profile = "my_default"

[models.m]
source = "hf"
repo = "test/model"

[profiles.my_default]
model = "m"
port = 8081
[profiles.my_default.vllm]
docker_image = "vllm/vllm-openai:latest"

[profiles.other]
model = "m"
port = 8082
[profiles.other.vllm]
docker_image = "vllm/vllm-openai:latest"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.resolve_profile_name(None), "my_default");
        assert_eq!(config.resolve_profile_name(Some("other")), "other");
    }

    #[test]
    fn test_resolve_profile_name_alias() {
        let toml_str = r#"
default_profile = "qwen_fast"

[models.m]
source = "hf"
repo = "test/model"

[profiles.qwen_fast]
model = "m"
port = 8081
aliases = ["fast", "moe"]

[profiles.qwen_dense]
model = "m"
port = 8081
aliases = ["dense", "27b"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.resolve_profile_name(Some("fast")), "qwen_fast");
        assert_eq!(config.resolve_profile_name(Some("moe")), "qwen_fast");
        assert_eq!(config.resolve_profile_name(Some("dense")), "qwen_dense");
        assert_eq!(config.resolve_profile_name(Some("27b")), "qwen_dense");
        assert_eq!(config.resolve_profile_name(Some("qwen_fast")), "qwen_fast");
        assert_eq!(
            config.resolve_profile_name(Some("nonexistent")),
            "nonexistent"
        );
        assert_eq!(config.resolve_profile_name(None), "qwen_fast");
    }
}

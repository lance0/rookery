use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub llama_server: PathBuf,

    #[serde(default = "default_profile")]
    pub default_profile: String,

    #[serde(default = "default_listen")]
    pub listen: SocketAddr,

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub model: String,

    #[serde(default = "default_port")]
    pub port: u16,

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
}

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

    pub fn validate(&self) -> Result<()> {
        if !self.llama_server.exists() {
            return Err(Error::BinaryNotFound(self.llama_server.clone()));
        }

        for (name, profile) in &self.profiles {
            if !self.models.contains_key(&profile.model) {
                return Err(Error::InvalidModelRef {
                    profile: name.clone(),
                    model: profile.model.clone(),
                });
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
        if let Some(template) = &profile.chat_template {
            args.extend([
                "--chat-template-file".into(),
                template.to_string_lossy().into_owned(),
            ]);
        }

        // Reasoning
        args.extend([
            "--reasoning-budget".into(),
            profile.reasoning_budget.to_string(),
        ]);

        // Threading
        args.extend(["--threads".into(), profile.threads.to_string()]);
        args.extend(["--threads-batch".into(), profile.threads_batch.to_string()]);

        // Context and batching
        args.extend(["--ctx-size".into(), profile.ctx_size.to_string()]);
        args.extend(["--batch-size".into(), profile.batch_size.to_string()]);
        args.extend(["--ubatch-size".into(), profile.ubatch_size.to_string()]);

        // KV cache
        args.extend(["--cache-type-k".into(), profile.cache_type_k.clone()]);
        args.extend(["--cache-type-v".into(), profile.cache_type_v.clone()]);

        // Parallelism
        args.extend(["--parallel".into(), "1".into()]);

        // Sampling
        args.extend(["--temp".into(), profile.temp.to_string()]);
        args.extend(["--top-p".into(), profile.top_p.to_string()]);
        args.extend(["--top-k".into(), profile.top_k.to_string()]);
        args.extend(["--min-p".into(), profile.min_p.to_string()]);
        args.extend(["--repeat-penalty".into(), "1.0".into()]);

        // Flash attention
        if profile.flash_attention {
            args.extend(["-fa".into(), "on".into()]);
        }

        // Network
        args.extend(["--port".into(), profile.port.to_string()]);
        args.extend(["--host".into(), "0.0.0.0".into()]);

        // GPU
        let ngl = if profile.gpu_layers < 0 {
            99
        } else {
            profile.gpu_layers
        };
        args.extend(["-ngl".into(), ngl.to_string()]);

        // Extra args passthrough
        args.extend(profile.extra_args.clone());

        Ok(args)
    }

    pub fn resolve_profile_name<'a>(&'a self, name: Option<&'a str>) -> &'a str {
        name.unwrap_or(&self.default_profile)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        let content = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

use std::collections::BTreeMap;

use rookery_core::config::Config;
use rookery_core::error::{Error, Result};
use serde::Serialize;

/// Path where the generated compose file is written.
pub const COMPOSE_FILE_NAME: &str = "vllm-compose.yml";

/// Returns the default compose file path: `~/.config/rookery/vllm-compose.yml`.
pub fn compose_file_path() -> Result<std::path::PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| Error::ConfigValidation("cannot determine config directory".into()))?;
    Ok(config_dir.join("rookery").join(COMPOSE_FILE_NAME))
}

/// Generate a Docker Compose YAML string for the given vLLM profile.
///
/// The compose file includes:
/// - Service name `vllm`
/// - Image from `VllmConfig.docker_image`
/// - `runtime: nvidia`
/// - Port mapping `{profile_port}:8000` (vLLM serves on 8000 internally)
/// - Environment: `HUGGING_FACE_HUB_TOKEN=${HF_TOKEN}`
/// - Command args: `--model`, `--gpu-memory-utilization`, `--port 8000`,
///   plus optional args when set, plus `extra_args` verbatim
/// - `deploy.resources.reservations.devices` for GPU
pub fn generate_compose(config: &Config, profile: &str) -> Result<String> {
    let prof = config
        .profiles
        .get(profile)
        .ok_or_else(|| Error::ProfileNotFound(profile.into()))?;

    let vllm = prof.vllm.as_ref().ok_or_else(|| {
        Error::ConfigValidation(format!("profile '{profile}' is not a vLLM profile"))
    })?;

    let model = config
        .models
        .get(&prof.model)
        .ok_or_else(|| Error::ModelNotFound(prof.model.clone()))?;

    // Build the command args (everything except docker_image which is the image field).
    let mut command: Vec<String> = Vec::new();

    // --model (required for vLLM)
    let repo = model.repo.as_ref().ok_or_else(|| {
        Error::ConfigValidation(format!(
            "vLLM profile '{profile}' references model '{}' which has no 'repo' field",
            prof.model
        ))
    })?;
    command.extend(["--model".into(), repo.clone()]);

    // --gpu-memory-utilization
    command.extend([
        "--gpu-memory-utilization".into(),
        vllm.gpu_memory_utilization.to_string(),
    ]);

    // --port 8000 (internal vLLM port)
    command.extend(["--port".into(), "8000".into()]);

    // Optional vLLM-specific flags — omitted when None
    if let Some(max_seqs) = vllm.max_num_seqs {
        command.extend(["--max-num-seqs".into(), max_seqs.to_string()]);
    }
    if let Some(max_batched) = vllm.max_num_batched_tokens {
        command.extend(["--max-num-batched-tokens".into(), max_batched.to_string()]);
    }
    if let Some(max_len) = vllm.max_model_len {
        command.extend(["--max-model-len".into(), max_len.to_string()]);
    }
    if let Some(ref quant) = vllm.quantization {
        command.extend(["--quantization".into(), quant.clone()]);
    }
    if let Some(ref parser) = vllm.tool_call_parser {
        command.extend(["--tool-call-parser".into(), parser.clone()]);
    }
    if let Some(ref kv_dtype) = vllm.kv_cache_dtype {
        command.extend(["--kv-cache-dtype".into(), kv_dtype.clone()]);
    }

    // extra_args verbatim
    command.extend(vllm.extra_args.clone());

    // Build the compose structure using serde types
    let compose = ComposeFile {
        services: {
            let mut services = BTreeMap::new();
            services.insert(
                "vllm".to_string(),
                Service {
                    image: vllm.docker_image.clone(),
                    runtime: "nvidia".to_string(),
                    ports: vec![format!("{}:8000", prof.port)],
                    environment: vec!["HUGGING_FACE_HUB_TOKEN=${HF_TOKEN}".to_string()],
                    command,
                    deploy: Deploy {
                        resources: Resources {
                            reservations: Reservations {
                                devices: vec![Device {
                                    driver: "nvidia".to_string(),
                                    count: 1,
                                    capabilities: vec!["gpu".to_string()],
                                }],
                            },
                        },
                    },
                },
            );
            services
        },
    };

    serde_yaml::to_string(&compose)
        .map_err(|e| Error::ConfigValidation(format!("failed to serialize compose YAML: {e}")))
}

// ── Serde types for Docker Compose structure ──────────────────────────

#[derive(Debug, Serialize)]
struct ComposeFile {
    services: BTreeMap<String, Service>,
}

#[derive(Debug, Serialize)]
struct Service {
    image: String,
    runtime: String,
    ports: Vec<String>,
    environment: Vec<String>,
    command: Vec<String>,
    deploy: Deploy,
}

#[derive(Debug, Serialize)]
struct Deploy {
    resources: Resources,
}

#[derive(Debug, Serialize)]
struct Resources {
    reservations: Reservations,
}

#[derive(Debug, Serialize)]
struct Reservations {
    devices: Vec<Device>,
}

#[derive(Debug, Serialize)]
struct Device {
    driver: String,
    count: u32,
    capabilities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rookery_core::config::{Config, VllmConfig};

    /// Helper: build a Config with a single vLLM profile and model.
    fn make_vllm_config(vllm: VllmConfig, port: u16, repo: &str) -> Config {
        let toml_str = format!(
            r#"
default_profile = "test_vllm"

[models.test_model]
source = "hf"
repo = "{repo}"

[profiles.test_vllm]
model = "test_model"
port = {port}

[profiles.test_vllm.vllm]
docker_image = "{docker_image}"
gpu_memory_utilization = {gpu_mem}
{optional_fields}
"#,
            docker_image = vllm.docker_image,
            gpu_mem = vllm.gpu_memory_utilization,
            optional_fields = build_optional_toml_fields(&vllm),
        );
        toml::from_str(&toml_str).expect("test config should parse")
    }

    fn build_optional_toml_fields(vllm: &VllmConfig) -> String {
        let mut lines = Vec::new();
        if let Some(v) = vllm.max_num_seqs {
            lines.push(format!("max_num_seqs = {v}"));
        }
        if let Some(v) = vllm.max_num_batched_tokens {
            lines.push(format!("max_num_batched_tokens = {v}"));
        }
        if let Some(v) = vllm.max_model_len {
            lines.push(format!("max_model_len = {v}"));
        }
        if let Some(ref v) = vllm.quantization {
            lines.push(format!("quantization = \"{v}\""));
        }
        if let Some(ref v) = vllm.tool_call_parser {
            lines.push(format!("tool_call_parser = \"{v}\""));
        }
        if let Some(ref v) = vllm.kv_cache_dtype {
            lines.push(format!("kv_cache_dtype = \"{v}\""));
        }
        if !vllm.extra_args.is_empty() {
            let args: Vec<String> = vllm.extra_args.iter().map(|a| format!("\"{a}\"")).collect();
            lines.push(format!("extra_args = [{}]", args.join(", ")));
        }
        lines.join("\n")
    }

    // === VAL-VLLM-001: Compose file includes correct Docker image and port mapping ===
    #[test]
    fn test_compose_image_and_port_mapping() {
        let config = make_vllm_config(
            VllmConfig {
                docker_image: "vllm/vllm-openai:cu130-nightly".into(),
                gpu_memory_utilization: 0.89,
                ..default_vllm_config()
            },
            8081,
            "kaitchup/Qwen3.5-27B-NVFP4",
        );

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let svc = &parsed["services"]["vllm"];
        assert_eq!(
            svc["image"].as_str().unwrap(),
            "vllm/vllm-openai:cu130-nightly"
        );

        let ports = svc["ports"].as_sequence().unwrap();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].as_str().unwrap(), "8081:8000");
    }

    // === VAL-VLLM-002: Compose file includes NVIDIA GPU reservation and runtime ===
    #[test]
    fn test_compose_gpu_reservation_and_runtime() {
        let config = make_vllm_config(default_vllm_config(), 8081, "test/model");

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let svc = &parsed["services"]["vllm"];

        // runtime: nvidia
        assert_eq!(svc["runtime"].as_str().unwrap(), "nvidia");

        // deploy.resources.reservations.devices
        let devices = &svc["deploy"]["resources"]["reservations"]["devices"];
        let device = &devices[0];
        assert_eq!(device["driver"].as_str().unwrap(), "nvidia");
        assert_eq!(device["count"].as_u64().unwrap(), 1);
        let caps = device["capabilities"].as_sequence().unwrap();
        assert_eq!(caps[0].as_str().unwrap(), "gpu");
    }

    // === VAL-VLLM-003: Compose file includes --model argument from profile ===
    #[test]
    fn test_compose_model_argument() {
        let config = make_vllm_config(default_vllm_config(), 8081, "kaitchup/Qwen3.5-27B-NVFP4");

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let command: Vec<String> = parsed["services"]["vllm"]["command"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let model_idx = command
            .iter()
            .position(|a| a == "--model")
            .expect("--model should be present");
        assert_eq!(command[model_idx + 1], "kaitchup/Qwen3.5-27B-NVFP4");
    }

    // === VAL-VLLM-004: Compose file includes gpu_memory_utilization ===
    #[test]
    fn test_compose_gpu_memory_utilization() {
        let config = make_vllm_config(
            VllmConfig {
                gpu_memory_utilization: 0.89,
                ..default_vllm_config()
            },
            8081,
            "test/model",
        );

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let command: Vec<String> = parsed["services"]["vllm"]["command"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let idx = command
            .iter()
            .position(|a| a == "--gpu-memory-utilization")
            .expect("--gpu-memory-utilization should be present");
        assert_eq!(command[idx + 1], "0.89");
    }

    // === VAL-VLLM-005: Compose file omits optional params when None, includes when set ===
    #[test]
    fn test_compose_optional_params_present_when_set() {
        let config = make_vllm_config(
            VllmConfig {
                docker_image: "vllm/vllm-openai:latest".into(),
                gpu_memory_utilization: 0.9,
                max_num_seqs: Some(4),
                max_num_batched_tokens: Some(4096),
                max_model_len: Some(234567),
                quantization: Some("awq_marlin".into()),
                tool_call_parser: Some("qwen3_coder".into()),
                kv_cache_dtype: Some("fp8".into()),
                extra_args: vec![],
            },
            8081,
            "test/model",
        );

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let command: Vec<String> = parsed["services"]["vllm"]["command"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        assert!(command.contains(&"--max-num-seqs".to_string()));
        assert!(command.contains(&"4".to_string()));
        assert!(command.contains(&"--max-num-batched-tokens".to_string()));
        assert!(command.contains(&"4096".to_string()));
        assert!(command.contains(&"--max-model-len".to_string()));
        assert!(command.contains(&"234567".to_string()));
        assert!(command.contains(&"--quantization".to_string()));
        assert!(command.contains(&"awq_marlin".to_string()));
        assert!(command.contains(&"--tool-call-parser".to_string()));
        assert!(command.contains(&"qwen3_coder".to_string()));
        assert!(command.contains(&"--kv-cache-dtype".to_string()));
        assert!(command.contains(&"fp8".to_string()));
    }

    #[test]
    fn test_compose_optional_params_absent_when_none() {
        let config = make_vllm_config(default_vllm_config(), 8081, "test/model");

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let command: Vec<String> = parsed["services"]["vllm"]["command"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        // These optional flags should NOT be present
        assert!(!command.contains(&"--max-num-seqs".to_string()));
        assert!(!command.contains(&"--max-num-batched-tokens".to_string()));
        assert!(!command.contains(&"--max-model-len".to_string()));
        assert!(!command.contains(&"--quantization".to_string()));
        assert!(!command.contains(&"--tool-call-parser".to_string()));
        assert!(!command.contains(&"--kv-cache-dtype".to_string()));

        // Required args should still be present
        assert!(command.contains(&"--model".to_string()));
        assert!(command.contains(&"--gpu-memory-utilization".to_string()));
        assert!(command.contains(&"--port".to_string()));
    }

    // === VAL-VLLM-006: Compose file passes extra_args verbatim ===
    #[test]
    fn test_compose_extra_args_verbatim() {
        let config = make_vllm_config(
            VllmConfig {
                extra_args: vec![
                    "--enable-prefix-caching".into(),
                    "--disable-log-requests".into(),
                ],
                ..default_vllm_config()
            },
            8081,
            "test/model",
        );

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let command: Vec<String> = parsed["services"]["vllm"]["command"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        assert!(command.contains(&"--enable-prefix-caching".to_string()));
        assert!(command.contains(&"--disable-log-requests".to_string()));

        // extra_args should come after standard args (--port 8000 is a standard arg)
        let port_idx = command.iter().position(|a| a == "--port").unwrap();
        let extra_idx = command
            .iter()
            .position(|a| a == "--enable-prefix-caching")
            .unwrap();
        assert!(
            extra_idx > port_idx,
            "extra_args should appear after standard args"
        );
    }

    // === VAL-VLLM-007: Compose file passes HF_TOKEN from environment ===
    #[test]
    fn test_compose_hf_token_environment() {
        let config = make_vllm_config(default_vllm_config(), 8081, "test/model");

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let env = parsed["services"]["vllm"]["environment"]
            .as_sequence()
            .unwrap();

        let has_hf_token = env.iter().any(|v| {
            v.as_str()
                .map(|s| s == "HUGGING_FACE_HUB_TOKEN=${HF_TOKEN}")
                .unwrap_or(false)
        });
        assert!(
            has_hf_token,
            "environment should include HF_TOKEN passthrough"
        );
    }

    // === VAL-VLLM-008: Generated compose file is valid YAML ===
    #[test]
    fn test_compose_valid_yaml() {
        let config = make_vllm_config(
            VllmConfig {
                docker_image: "vllm/vllm-openai:cu130-nightly".into(),
                gpu_memory_utilization: 0.89,
                max_num_seqs: Some(4),
                max_num_batched_tokens: Some(4096),
                max_model_len: Some(234567),
                quantization: Some("awq_marlin".into()),
                tool_call_parser: Some("qwen3_coder".into()),
                kv_cache_dtype: Some("fp8".into()),
                extra_args: vec!["--enable-chunked-prefill".into()],
            },
            8081,
            "kaitchup/Qwen3.5-27B-NVFP4",
        );

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();

        // Must be parseable as valid YAML
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&yaml_str).expect("generated compose must be valid YAML");

        // Verify structure is a mapping with "services" key
        assert!(parsed.is_mapping(), "top-level should be a mapping");
        assert!(parsed["services"].is_mapping(), "should have services key");
        assert!(
            parsed["services"]["vllm"].is_mapping(),
            "should have vllm service"
        );
    }

    // === Additional: error cases ===

    #[test]
    fn test_compose_error_profile_not_found() {
        let config: Config = toml::from_str(
            r#"
default_profile = "default"
[profiles.default]
model = "m"
port = 8081
[profiles.default.llama_server]
[models.m]
source = "local"
"#,
        )
        .unwrap();

        let result = generate_compose(&config, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("nonexistent"));
    }

    #[test]
    fn test_compose_error_not_vllm_profile() {
        let config: Config = toml::from_str(
            r#"
default_profile = "default"
[profiles.default]
model = "m"
port = 8081
[profiles.default.llama_server]
[models.m]
source = "local"
"#,
        )
        .unwrap();

        let result = generate_compose(&config, "default");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a vLLM profile")
        );
    }

    #[test]
    fn test_compose_error_model_not_found() {
        let config: Config = toml::from_str(
            r#"
default_profile = "vp"
[profiles.vp]
model = "missing_model"
port = 8081
[profiles.vp.vllm]
docker_image = "vllm/vllm-openai:latest"
"#,
        )
        .unwrap();

        let result = generate_compose(&config, "vp");
        assert!(result.is_err());
    }

    #[test]
    fn test_compose_file_path() {
        let path = compose_file_path().unwrap();
        assert!(path.ends_with("rookery/vllm-compose.yml"));
    }

    // === Additional: comprehensive full config test ===
    #[test]
    fn test_compose_full_config_all_fields() {
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
        let yaml_str = generate_compose(&config, "vllm_prod").unwrap();

        // Parse back and verify all parts
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();
        let svc = &parsed["services"]["vllm"];

        // Image
        assert_eq!(
            svc["image"].as_str().unwrap(),
            "vllm/vllm-openai:cu130-nightly"
        );

        // Runtime
        assert_eq!(svc["runtime"].as_str().unwrap(), "nvidia");

        // Ports
        assert_eq!(svc["ports"][0].as_str().unwrap(), "8081:8000");

        // Environment
        let env: Vec<&str> = svc["environment"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(env.contains(&"HUGGING_FACE_HUB_TOKEN=${HF_TOKEN}"));

        // Command
        let cmd: Vec<&str> = svc["command"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(cmd.contains(&"--model"));
        assert!(cmd.contains(&"kaitchup/Qwen3.5-27B-NVFP4"));
        assert!(cmd.contains(&"--gpu-memory-utilization"));
        assert!(cmd.contains(&"0.89"));
        assert!(cmd.contains(&"--port"));
        assert!(cmd.contains(&"8000"));
        assert!(cmd.contains(&"--max-num-seqs"));
        assert!(cmd.contains(&"4"));
        assert!(cmd.contains(&"--max-num-batched-tokens"));
        assert!(cmd.contains(&"4096"));
        assert!(cmd.contains(&"--max-model-len"));
        assert!(cmd.contains(&"234567"));
        assert!(cmd.contains(&"--quantization"));
        assert!(cmd.contains(&"awq_marlin"));
        assert!(cmd.contains(&"--tool-call-parser"));
        assert!(cmd.contains(&"qwen3_coder"));
        assert!(cmd.contains(&"--kv-cache-dtype"));
        assert!(cmd.contains(&"fp8"));
        assert!(cmd.contains(&"--enable-chunked-prefill"));

        // GPU reservation
        let device = &svc["deploy"]["resources"]["reservations"]["devices"][0];
        assert_eq!(device["driver"].as_str().unwrap(), "nvidia");
        assert_eq!(device["count"].as_u64().unwrap(), 1);
        assert_eq!(device["capabilities"][0].as_str().unwrap(), "gpu");
    }

    // === Additional: model with no repo (edge case) ===
    #[test]
    fn test_compose_model_without_repo() {
        let toml_str = r#"
default_profile = "vp"

[models.local_model]
source = "local"
path = "/data/models/test"

[profiles.vp]
model = "local_model"
port = 9090

[profiles.vp.vllm]
docker_image = "vllm/vllm-openai:latest"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let result = generate_compose(&config, "vp");

        // vLLM profiles require a model repo — should fail validation
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no 'repo' field"),
            "expected repo validation error, got: {err}"
        );
    }

    // === Additional: different port ===
    #[test]
    fn test_compose_different_port() {
        let config = make_vllm_config(default_vllm_config(), 9999, "test/model");

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        assert_eq!(
            parsed["services"]["vllm"]["ports"][0].as_str().unwrap(),
            "9999:8000"
        );
    }

    // === VAL-EDGE-007: Compose generation edge cases ===

    #[test]
    fn test_compose_all_optional_vllm_params_set_simultaneously() {
        let config = make_vllm_config(
            VllmConfig {
                docker_image: "vllm/vllm-openai:cu130-nightly".into(),
                gpu_memory_utilization: 0.95,
                max_num_seqs: Some(8),
                max_num_batched_tokens: Some(8192),
                max_model_len: Some(65536),
                quantization: Some("gptq".into()),
                tool_call_parser: Some("hermes".into()),
                kv_cache_dtype: Some("fp8_e5m2".into()),
                extra_args: vec![
                    "--enable-prefix-caching".into(),
                    "--disable-log-requests".into(),
                    "--max-log-len".into(),
                    "100".into(),
                ],
            },
            7777,
            "org/model-repo",
        );

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let command: Vec<String> = parsed["services"]["vllm"]["command"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        // All optional params present
        assert!(command.contains(&"--max-num-seqs".to_string()));
        assert!(command.contains(&"8".to_string()));
        assert!(command.contains(&"--max-num-batched-tokens".to_string()));
        assert!(command.contains(&"8192".to_string()));
        assert!(command.contains(&"--max-model-len".to_string()));
        assert!(command.contains(&"65536".to_string()));
        assert!(command.contains(&"--quantization".to_string()));
        assert!(command.contains(&"gptq".to_string()));
        assert!(command.contains(&"--tool-call-parser".to_string()));
        assert!(command.contains(&"hermes".to_string()));
        assert!(command.contains(&"--kv-cache-dtype".to_string()));
        assert!(command.contains(&"fp8_e5m2".to_string()));
        // Extra args present
        assert!(command.contains(&"--enable-prefix-caching".to_string()));
        assert!(command.contains(&"--disable-log-requests".to_string()));
        assert!(command.contains(&"--max-log-len".to_string()));
        assert!(command.contains(&"100".to_string()));

        // Port mapping uses the custom port
        assert_eq!(
            parsed["services"]["vllm"]["ports"][0].as_str().unwrap(),
            "7777:8000"
        );
    }

    #[test]
    fn test_compose_with_max_model_len_field() {
        let config = make_vllm_config(
            VllmConfig {
                docker_image: "vllm/vllm-openai:latest".into(),
                gpu_memory_utilization: 0.9,
                max_num_seqs: None,
                max_num_batched_tokens: None,
                max_model_len: Some(32768),
                quantization: None,
                tool_call_parser: None,
                kv_cache_dtype: None,
                extra_args: vec![],
            },
            8081,
            "test/model",
        );

        let yaml_str = generate_compose(&config, "test_vllm").unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();

        let command: Vec<String> = parsed["services"]["vllm"]["command"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let idx = command
            .iter()
            .position(|a| a == "--max-model-len")
            .expect("--max-model-len should be present");
        assert_eq!(command[idx + 1], "32768");

        // Other optional params should NOT be present
        assert!(!command.contains(&"--max-num-seqs".to_string()));
        assert!(!command.contains(&"--quantization".to_string()));
    }

    /// Default VllmConfig for test convenience.
    fn default_vllm_config() -> VllmConfig {
        VllmConfig {
            docker_image: "vllm/vllm-openai:latest".into(),
            gpu_memory_utilization: 0.9,
            max_num_seqs: None,
            max_num_batched_tokens: None,
            max_model_len: None,
            quantization: None,
            tool_call_parser: None,
            kv_cache_dtype: None,
            extra_args: vec![],
        }
    }
}

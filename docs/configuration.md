# Configuration Reference

Config file: `~/.config/rookery/config.toml`

## Top-Level

```toml
llama_server = "/path/to/llama-server"    # path to llama-server binary
default_profile = "qwen_fast"              # profile used when no name specified
listen = "0.0.0.0:3131"                   # daemon listen address
auto_start = true                          # start default profile on daemon boot
idle_timeout = 1800                        # seconds before auto-sleep; 0/omitted disables
model_dirs = ["/mnt/models"]              # extra dirs to scan for model files (optional)
```

`idle_timeout` is daemon-wide. When the active backend has been idle for that many seconds with no inference traffic, Rookery unloads it and transitions to `sleeping`. The next `/api/chat` request wakes the last active profile automatically before proxying.

`model_dirs` adds custom directories to the model scanner. Rookery always scans the HuggingFace hub cache and llama.cpp cache automatically — use `model_dirs` for models stored outside those standard locations.

## Models

Define what models are available. Referenced by profiles.

### HuggingFace models (GGUF — for llama-server)

```toml
[models.qwen35]
source = "hf"                              # "hf" (HuggingFace) or "local"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"    # HF repo
file = "UD-Q5_K_XL"                       # quant label (without .gguf)
estimated_vram_mb = 29200                  # for capacity gate (optional)
```

### HuggingFace models (any format — for vLLM)

vLLM supports safetensors, AWQ, GPTQ, NVFP4, and other formats. No `file` field needed — vLLM manages the model inside Docker.

```toml
[models.qwen35_27b_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"
estimated_vram_mb = 20000
```

### Local models

Point directly at a model file on disk (GGUF for llama-server, or any format for vLLM).

```toml
[models.local_model]
source = "local"
path = "/path/to/model.gguf"              # local file path
estimated_vram_mb = 20000
```

## Profiles

Define how to run a model. Multiple profiles can share a model.

```toml
[profiles.qwen_fast]
model = "qwen35"                # references [models.qwen35]
aliases = ["qwen", "fast"]      # optional alternate names for this profile
port = 8081                     # llama-server listen port
ctx_size = 262144               # context window (tokens)
threads = 4                     # CPU threads for inference
threads_batch = 24              # CPU threads for batch processing
batch_size = 4096               # batch size
ubatch_size = 1024              # micro-batch size
gpu_layers = -1                 # -1 = all layers on GPU
cache_type_k = "q8_0"          # KV cache key quantization
cache_type_v = "q8_0"          # KV cache value quantization
flash_attention = true          # enable flash attention
reasoning_budget = 0            # -1 = unlimited thinking, 0 = disabled
chat_template = "/path/to/template.jinja"  # custom chat template (optional)
temp = 0.7                      # sampling temperature
top_p = 0.8                     # nucleus sampling
top_k = 20                      # top-k sampling
min_p = 0.0                     # min-p sampling
extra_args = ["--no-mmap"]      # additional llama-server args (optional)
```

### KV Cache Quantization

| Type | Quality | VRAM Usage | Notes |
|------|---------|------------|-------|
| `f16` | Best | Highest | Default if not specified |
| `q8_0` | Near-lossless | ~50% of f16 | Recommended for most models |
| `q4_0` | Good | ~25% of f16 | Use when VRAM is tight (e.g., Q6 model weights) |

### Reasoning Budget

| Value | Behavior |
|-------|----------|
| `0` | Thinking disabled (no `<think>` tags) |
| `-1` | Unlimited thinking (model decides) |
| `N` | Cap thinking to N tokens |

## vLLM Backend

Profiles can use vLLM instead of llama-server by adding a `[profiles.<name>.vllm]` sub-table:

```toml
[models.qwen35_27b_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"
estimated_vram_mb = 25800

[profiles.qwen_nvfp4]
model = "qwen35_27b_nvfp4"
port = 8081

[profiles.qwen_nvfp4.vllm]
docker_image = "vllm/vllm-openai:cu130-nightly"   # Docker image
gpu_memory_utilization = 0.89                       # fraction of VRAM to use
max_num_seqs = 4                                    # max concurrent sequences
max_num_batched_tokens = 4096                       # per-batch token budget
max_model_len = 234567                              # max context length
quantization = "awq_marlin"                         # quantization method
tool_call_parser = "qwen3_coder"                    # tool call format parser
kv_cache_dtype = "fp8"                              # KV cache quantization
extra_args = ["--enable-chunked-prefill"]            # additional vLLM flags
```

### Prerequisites for vLLM

- Docker + Docker Compose v2+
- NVIDIA Container Toolkit (`nvidia-container-toolkit`)
- HuggingFace token: set `HF_TOKEN` env var (for gated models)

### How It Works

1. Rookery generates `~/.config/rookery/vllm-compose.yml` from your profile config
2. `rookery start` runs `docker compose up -d` instead of spawning llama-server
3. `rookery stop` runs `docker compose down`
4. Health checks, inference canary, and agent management work identically
5. CUDA errors detected in docker logs trigger the same immediate canary

### Backend Selection

The backend is determined by the profile's sub-table:
- `[profiles.name.llama_server]` → llama-server (default, can also be flat with no sub-table)
- `[profiles.name.vllm]` → vLLM via Docker Compose

```bash
rookery start qwen_fast     # uses llama-server (has llama_server sub-table)
rookery start qwen_nvfp4    # uses vLLM (has vllm sub-table)
rookery swap qwen_fast       # swaps between backends seamlessly
```

## Agents

See [Agent Management](agents.md) for full documentation.

```toml
[agents.hermes]
command = "/home/lance/.local/bin/hermes"
args = ["gateway", "run", "--replace"]
auto_start = true
restart_on_swap = true
restart_on_crash = true
depends_on_port = 8081
version_file = "/path/to/pyproject.toml"
update_command = "/home/lance/.local/bin/hermes update"
update_workdir = "/path/to/agent/repo"
restart_on_error_patterns = ["telegram.error.TimedOut"]
```

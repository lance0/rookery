# Configuration Reference

Config file: `~/.config/rookery/config.toml`

## Top-Level

```toml
llama_server = "/path/to/llama-server"    # path to llama-server binary
default_profile = "qwen_fast"              # profile used when no name specified
listen = "0.0.0.0:3131"                   # daemon listen address
```

## Models

Define what models are available. Referenced by profiles.

```toml
[models.qwen35]
source = "hf"                              # "hf" (HuggingFace) or "local"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"    # HF repo
file = "UD-Q5_K_XL"                       # quant filename (without .gguf)
estimated_vram_mb = 29200                  # for capacity gate (optional)

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
restart_on_error_patterns = ["telegram.error.TimedOut"]
```

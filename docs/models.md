# Model Management

Rookery supports models from HuggingFace, local files, and Docker-managed formats.

## Model Sources

### HuggingFace (GGUF) — llama-server backend
The most common setup. Rookery downloads GGUF models from HuggingFace via `llama-server -hf`.

```toml
[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 25800
```

Models are cached in the HuggingFace hub cache (`$HF_HOME/hub/` or `~/.cache/huggingface/hub/`). Set `HF_HOME` in your environment or systemd unit to customize the cache location.

### HuggingFace (any format) — vLLM backend
vLLM supports safetensors, AWQ, GPTQ, NVFP4, and other formats — not just GGUF. Point at any HuggingFace repo and vLLM handles the download inside Docker.

```toml
[models.qwen35_27b_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"
estimated_vram_mb = 20000
```

No `file` field needed — vLLM manages the model lifecycle in its Docker container.

### Local files
Point directly at a model file on disk. Works with any format your backend supports.

```toml
[models.my_local]
source = "local"
path = "/home/user/models/my-model-Q4_K_M.gguf"
estimated_vram_mb = 8000
```

## Model Discovery

Rookery scans multiple locations to find downloaded models:

1. **HuggingFace hub cache** — `$HF_HOME/hub/` or `~/.cache/huggingface/hub/`
2. **llama.cpp cache** — `~/.cache/llama.cpp/`
3. **Custom directories** — via `model_dirs` in config

```toml
# Scan additional directories for model files
model_dirs = ["/mnt/models", "/home/user/gguf-collection"]
```

The Models tab in the dashboard and `rookery models list` show all discovered models across all sources.

## CLI

```bash
rookery models search Qwen3.5           # search HuggingFace for GGUF repos
rookery models quants Qwen3.5-27B       # list available quants with sizes + VRAM fit
rookery models recommend Qwen3.5-27B    # best quant for your hardware
rookery models list                     # locally cached models
rookery models pull Qwen3.5-27B         # download best-fit quant
rookery models pull Qwen3.5-27B --quant Q6_K  # download specific quant
rookery models hardware                 # show GPU/CPU/RAM profile
```

All commands support `--json` for scripting.

## Features

- **Auto-prefix**: bare names like `Qwen3.5-27B` get `unsloth/` prefix and `-GGUF` suffix
- **VRAM-aware recommendations**: considers free VRAM, model size with 15% overhead, and RAM for partial offload
- **Quant preference**: UD variants first (UD-Q4_K_XL > UD-Q4_K_L), then standard (Q8_0 > Q6_K > Q4_K_M)
- **Multi-cache scanner**: finds models in HF hub cache, llama.cpp cache, and custom directories
- **Dashboard**: Models tab has search, quant browser, and one-click download

## API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/models/search?q=query` | GET | Search HuggingFace for GGUF repos |
| `/api/models/quants?repo=name` | GET | List quants with sizes and VRAM fit |
| `/api/models/recommend?repo=name` | GET | VRAM-aware best quant recommendation |
| `/api/models/cached` | GET | All discovered models (HF + llama.cpp + local + custom dirs) |
| `/api/models/pull` | POST | Download `{"repo": "...", "quant": "..."}` |
| `/api/hardware` | GET | Hardware profile (GPU, CPU, RAM) |

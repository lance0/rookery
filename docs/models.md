# Model Management

Rookery can search HuggingFace, recommend quants for your hardware, and download GGUF models.

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

## Features

- **Auto-prefix**: bare names like `Qwen3.5-27B` get `unsloth/` prefix and `-GGUF` suffix
- **VRAM-aware recommendations**: considers free VRAM, model size with 15% overhead, and RAM for partial offload
- **Quant preference**: UD variants first (UD-Q4_K_XL > UD-Q4_K_L), then standard (Q8_0 > Q6_K > Q4_K_M)
- **Cache scanner**: finds models in `~/.cache/llama.cpp/`
- **Dashboard**: Models tab has search, quant browser, and one-click download

## API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/models/search?q=query` | GET | Search HuggingFace for GGUF repos |
| `/api/models/quants?repo=name` | GET | List quants with sizes and VRAM fit |
| `/api/models/recommend?repo=name` | GET | VRAM-aware best quant recommendation |
| `/api/models/cached` | GET | Locally cached models |
| `/api/models/pull` | POST | Download `{"repo": "...", "quant": "..."}` |
| `/api/hardware` | GET | Hardware profile (GPU, CPU, RAM) |

All commands support `--json` for scripting.

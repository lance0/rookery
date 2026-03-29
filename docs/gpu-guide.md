# GPU Guide

Model recommendations by NVIDIA GPU. All estimates assume llama-server with full GPU offload, flash attention enabled, and KV cache quantization.

## How to Choose

Token generation speed is bounded by **memory bandwidth** — the GPU spends most of its time reading model weights. Rule of thumb:

```
gen_tok/s ≈ memory_bandwidth_GB/s / model_size_GB
```

For example: RTX 4090 (1008 GB/s) running a 20GB model ≈ 50 tok/s. Bigger models are slower, smaller quants are faster.

**VRAM budget**: model weights + KV cache + ~500MB overhead. Use KV cache quantization (`q8_0` or `q4_0`) to fit larger models or longer contexts.

## Recommendations by GPU

### RTX 3060 12GB (384 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q4_K_M | ~5GB | 32K | ~60 |
| Llama-3.1-8B | Q4_K_M | ~5GB | 32K | ~60 |
| Mistral-7B | Q4_K_M | ~4.5GB | 32K | ~70 |
| Phi-4 (14B) | Q4_K_M | ~8.5GB | 16K | ~35 |

```toml
[models.qwen3_8b]
source = "hf"
repo = "unsloth/Qwen3-8B-GGUF"
file = "Q4_K_M"
estimated_vram_mb = 6000

[profiles.default.llama_server]
ctx_size = 32768
cache_type_k = "q8_0"
cache_type_v = "q8_0"
flash_attention = true
```

**Tips**: Stick to 7B-8B models. 13B+ models need partial offload which kills generation speed. Use `q8_0` KV cache to save VRAM for longer contexts.

### RTX 3080 10GB / 3080 Ti 12GB (760/912 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q6_K | ~7GB | 32K | ~90 |
| Llama-3.1-8B | Q6_K | ~7GB | 32K | ~90 |
| Mistral-7B | Q8_0 | ~8GB | 16K | ~80 |

**Tips**: Higher bandwidth than the 3060 but less VRAM (10GB on the 3080). Similar model size limits. The 3080 Ti (12GB) can fit Q6_K quants of 8B models comfortably.

### RTX 3090 24GB (936 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q8_0 | ~9GB | 128K | ~80 |
| Llama-3.1-8B | Q8_0 | ~9GB | 128K | ~80 |
| Qwen3.5-27B | Q4_K_M | ~16GB | 32K | ~45 |
| Llama-3.3-70B | Q3_K_M | ~20GB | 8K | ~30 |
| Qwen3.5-35B-A3B (MoE) | Q4_K_XL | ~20GB | 64K | ~120 |

```toml
[models.qwen35_moe]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "Q4_K_XL"
estimated_vram_mb = 22000

[profiles.default.llama_server]
ctx_size = 65536
cache_type_k = "q8_0"
cache_type_v = "q8_0"
flash_attention = true
```

**Tips**: The sweet spot GPU. 24GB fits 27B dense models at Q4 or MoE models at higher quants. MoE models (Qwen3.5-35B-A3B) are excellent here — only 3B active params means fast generation despite the large total size.

### RTX 4060 Ti 16GB (288 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q6_K | ~7GB | 64K | ~35 |
| Phi-4 (14B) | Q4_K_M | ~8.5GB | 32K | ~25 |
| Qwen3.5-27B | IQ3_XXS | ~10GB | 16K | ~20 |

**Tips**: More VRAM than the 3060 but lower bandwidth. Good for longer contexts with smaller models. Generation speed will feel slow on 14B+ models — prioritize quality quants on 8B models over cramming larger models in.

### RTX 4070 Ti Super 16GB (672 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q8_0 | ~9GB | 64K | ~60 |
| Phi-4 (14B) | Q6_K | ~12GB | 32K | ~45 |
| Qwen3.5-35B-A3B (MoE) | IQ4_XS | ~14GB | 32K | ~100 |

**Tips**: Good balance of VRAM and bandwidth. MoE models shine here — the Qwen3.5 MoE fits at lower quants and still generates fast thanks to only 3B active params.

### RTX 4080 16GB (717 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q8_0 | ~9GB | 64K | ~65 |
| Phi-4 (14B) | Q6_K | ~12GB | 32K | ~50 |
| Qwen3.5-35B-A3B (MoE) | IQ4_XS | ~14GB | 32K | ~110 |

**Tips**: Similar to 4070 Ti Super with slightly more bandwidth. Same model recommendations apply.

### RTX 4090 24GB (1008 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q8_0 | ~9GB | 128K | ~90 |
| Qwen3.5-27B | Q4_K_M | ~16GB | 64K | ~50 |
| Qwen3.5-27B | Q6_K | ~20GB | 32K | ~40 |
| Qwen3.5-35B-A3B (MoE) | Q4_K_XL | ~20GB | 128K | ~140 |
| Llama-3.3-70B | Q3_K_M | ~20GB | 16K | ~35 |

```toml
[models.qwen35_27b]
source = "hf"
repo = "unsloth/Qwen3.5-27B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 18000

[profiles.dense.llama_server]
ctx_size = 65536
cache_type_k = "q4_0"
cache_type_v = "q4_0"
flash_attention = true
```

**Tips**: The enthusiast sweet spot. 24GB + 1 TB/s bandwidth means you can run 27B dense models at good quants or MoE models at high quants with long context. Use `q4_0` KV cache to fit Q6_K dense models.

### RTX 5090 32GB (1792 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3.5-27B | Q6_K_XL | ~26GB | 128K | ~55 |
| Qwen3.5-35B-A3B (MoE) | UD-Q5_K_XL | ~25GB | 262K | ~170 |
| Llama-3.3-70B | Q4_K_M | ~28GB | 16K | ~45 |
| Nemotron-Cascade-2 (MoE) | Q4_K_M | ~23GB | 128K | ~295 |

```toml
[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q5_K_XL"
estimated_vram_mb = 29200

[profiles.fast.llama_server]
ctx_size = 262144
cache_type_k = "q8_0"
cache_type_v = "q8_0"
flash_attention = true
```

**Tips**: 32GB + 1.8 TB/s is the current consumer ceiling. Run 27B dense at Q6+ for best quality, or MoE models at Q5+ with full 262K context. MoE generation speed is exceptional (160-295 tok/s) because bandwidth is so high relative to active parameter count.

## KV Cache Quantization

KV cache can be a significant VRAM consumer at long context lengths. Quantizing it trades minimal quality loss for major VRAM savings:

| Type | Quality | VRAM vs f16 | Best For |
|------|---------|-------------|----------|
| `f16` | Best | 100% | Short context, plenty of VRAM |
| `q8_0` | Near-lossless | ~50% | Default recommendation |
| `q4_0` | Good | ~25% | VRAM-constrained (large model + long context) |

KV cache size scales with context length. At 128K context with a 27B model:
- `f16`: ~8GB KV cache
- `q8_0`: ~4GB
- `q4_0`: ~2GB

Always enable `flash_attention = true` when using KV cache quantization.

## MoE vs Dense

**MoE (Mixture of Experts)** models like Qwen3.5-35B-A3B have many total parameters but only activate a small fraction per token. This means:
- **Faster generation** — only ~3B active params need to be read from VRAM per token
- **More VRAM** — all 35B params must be loaded, even though only 3B are active
- **Better quality per tok/s** — you get 35B model quality at 3B model speed

**Dense models** like Qwen3.5-27B read all 27B parameters per token:
- **Slower generation** — 27B params read per token
- **More predictable** — no routing overhead
- **Better for tool calling** — some MoE models have weaker structured output

**Rule of thumb**: If you want speed, use MoE. If you want the most reliable tool calling and structured output, use the largest dense model that fits.

## Partial Offload (GPU + CPU)

If a model doesn't fit entirely in VRAM, llama-server can offload some layers to CPU RAM. Set `gpu_layers` to a number less than the total layer count:

```toml
gpu_layers = 30   # offload 30 layers to GPU, rest stays on CPU
```

**Reality check**: Partial offload tanks generation speed because CPU memory bandwidth (~50 GB/s DDR5) is 10-30x slower than GPU bandwidth. A model that generates at 50 tok/s fully on GPU might drop to 5-10 tok/s with partial offload. Prompt processing stays reasonable because it's compute-bound, not bandwidth-bound.

Use partial offload only when:
- You need a larger model for quality and can tolerate slow generation
- You're doing batch processing where latency doesn't matter
- The alternative is not running the model at all

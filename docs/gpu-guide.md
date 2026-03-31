# GPU Guide

Model recommendations by NVIDIA GPU. All estimates assume llama-server with full GPU offload, flash attention enabled, and KV cache quantization.

## How to Choose

Token generation speed is bounded by **memory bandwidth** — the GPU spends most of its time reading model weights:

```
gen_tok/s ≈ memory_bandwidth_GB/s / model_size_GB
```

**VRAM budget**: model weights + KV cache + ~500MB overhead. Use KV cache quantization (`q8_0` or `q4_0`) to fit larger models or longer contexts.

## Quick Reference

| GPU | VRAM | Bandwidth | Best Dense | Best MoE | Gen tok/s |
|-----|------|-----------|-----------|----------|-----------|
| RTX 3060 12GB | 12 GB | 360 GB/s | 8B Q5_K_M | — | ~50 |
| RTX 3080 10GB | 10 GB | 760 GB/s | 8B Q5_K_M | — | ~85 |
| RTX 3080 Ti 12GB | 12 GB | 912 GB/s | 14B Q4_K_M | — | ~55 |
| RTX 3090 24GB | 24 GB | 936 GB/s | 27B Q4_K_M | Qwen3.5-35B-A3B Q4_K_XL | ~35 / ~100 |
| RTX 4060 Ti 16GB | 16 GB | 288 GB/s | 14B Q4_K_M | Qwen3.5-35B-A3B Q3_K_M | ~25 / ~40 |
| RTX 4070 Ti Super 16GB | 16 GB | 672 GB/s | 14B Q5_K_M | Qwen3.5-35B-A3B Q3_K_M | ~55 / ~80 |
| RTX 4080 16GB | 16 GB | 717 GB/s | 14B Q5_K_M | Qwen3.5-35B-A3B Q3_K_M | ~60 / ~85 |
| RTX 4090 24GB | 24 GB | 1,008 GB/s | 27B Q5_K_M | Qwen3.5-35B-A3B Q4_K_XL | ~45 / ~130 |
| RTX 5090 32GB | 32 GB | 1,792 GB/s | 27B Q6_K_XL | Qwen3.5-35B-A3B Q5_K_XL | ~52 / ~196 |

## Detailed Recommendations

### RTX 3060 12GB (360 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q5_K_M | ~5.3GB | 16K | ~50 |
| Llama-3.1-8B | Q4_K_M | ~4.6GB | 32K | ~55 |
| Phi-4 (14B) | Q4_K_M | ~8.7GB | 4-8K | ~28 |

**Tips**: Stick to 7B-8B models. 14B fits but context is tight. The 192-bit bus limits throughput — this card is fine for casual use but not speed-critical workloads.

```toml
[models.qwen3_8b]
source = "hf"
repo = "unsloth/Qwen3-8B-GGUF"
file = "Q5_K_M"
estimated_vram_mb = 6500

[profiles.default.llama_server]
ctx_size = 16384
cache_type_k = "q8_0"
cache_type_v = "q8_0"
flash_attention = true
```

### RTX 3080 10GB / 3080 Ti 12GB (760 / 912 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B (3080) | Q5_K_M | ~5.3GB | 16K | ~85 |
| Qwen3-8B (3080 Ti) | Q6_K | ~6.1GB | 32K | ~100 |
| Phi-4 14B (3080 Ti) | Q4_K_M | ~8.7GB | 8K | ~55 |

**Tips**: The 3080 (10GB) is VRAM-limited despite great bandwidth — same models as the 3060 but 2x faster. The 3080 Ti (12GB, 912 GB/s) is significantly better: more VRAM + the highest bandwidth in the 12GB tier.

### RTX 3090 24GB (936 GB/s)

The best value card for local inference in 2026 — used prices of $600-800 for 24GB of high-bandwidth VRAM.

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q8_0 | ~8GB | 128K | ~90 |
| Phi-4 (14B) | Q6_K | ~12GB | 32K | ~60 |
| Qwen3.5-27B | Q4_K_M | ~16GB | 16-32K | ~35 |
| Qwen3.5-35B-A3B (MoE) | Q4_K_XL | ~20GB | 64K | ~100 |
| Llama-3.3-70B | Q3_K_M | ~30GB | — | Doesn't fit |

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

**Tips**: The sweet spot GPU. MoE models are excellent here — Qwen3.5-35B-A3B gives 35B quality at ~100 tok/s because only 3B params are active. For dense models, 27B Q4_K_M fits with room for 16-32K context.

### RTX 4060 Ti 16GB (288 GB/s)

> **Warning**: This card has 16GB VRAM but a 128-bit memory bus. Models fit but run slowly. A used RTX 3090 ($600-800) gives 24GB VRAM + 3x the bandwidth. Avoid this card for LLM inference.

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q5_K_M | ~5.3GB | 32K | ~40 |
| Phi-4 (14B) | Q4_K_M | ~8.7GB | 16K | ~25 |
| Qwen3.5-35B-A3B (MoE) | Q3_K_M | ~12GB | 16K | ~40 |

### RTX 4070 Ti Super 16GB (672 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q8_0 | ~8GB | 64K | ~80 |
| Phi-4 (14B) | Q5_K_M | ~10GB | 16K | ~55 |
| Qwen3.5-35B-A3B (MoE) | Q3_K_M | ~12GB | 32K | ~80 |

**Tips**: Good balance of VRAM and bandwidth. MoE models shine — small active parameter count offsets the 16GB VRAM limit.

### RTX 4080 16GB (717 GB/s)

Same VRAM as the 4070 Ti Super with ~7% more bandwidth. Same model selection — the speed difference is marginal.

> **Note**: A used RTX 3090 (24GB, 936 GB/s) has 8GB more VRAM and 30% more bandwidth for less money. Worth considering if you can find one.

### RTX 4090 24GB (1,008 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3-8B | Q8_0 | ~8GB | 128K | ~128 |
| Qwen3.5-27B | Q5_K_M | ~19GB | 8-16K | ~45 |
| Qwen3.5-35B-A3B (MoE) | Q4_K_XL | ~20GB | 128K | ~130 |
| 32B | Q4_K_M | ~19GB | 8K | ~40 |
| Llama-3.3-70B | Q4_K_M | ~42GB | — | Doesn't fit |

```toml
[models.qwen35_27b]
source = "hf"
repo = "unsloth/Qwen3.5-27B-GGUF"
file = "UD-Q5_K_M"
estimated_vram_mb = 21000

[profiles.dense.llama_server]
ctx_size = 16384
cache_type_k = "q4_0"
cache_type_v = "q4_0"
flash_attention = true
```

**Tips**: The enthusiast sweet spot. 24GB + 1 TB/s bandwidth runs 27B dense at good quants or MoE at high quants with long context. Use `q4_0` KV cache to fit Q6_K dense models.

### RTX 5090 32GB (1,792 GB/s)

| Model | Quant | Size | Context | Est. tok/s |
|-------|-------|------|---------|------------|
| Qwen3.5-27B | Q6_K_XL | ~26GB | 128K | ~54 |
| Qwen3.5-35B-A3B (MoE) | UD-Q5_K_XL | ~25GB | 262K | ~196 |
| Nemotron-Cascade-2 (MoE) | Q4_K_M | ~23GB | 128K | ~295 |
| 32B | Q5_K_M | ~23GB | 16-32K | ~60 |
| Llama-3.3-70B | Q4_K_M | ~42GB | — | Doesn't fit |

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

**Tips**: 32GB + 1.8 TB/s is the current consumer ceiling. Run 27B dense at Q6+ for best quality, or MoE at Q5+ with full 262K context. MoE generation is exceptional (~196 tok/s llama-bench, up to ~213 in-server). PP hits 6,670 tok/s at 512+ tokens. **Build with CUDA 12.8** — CUDA 13.x has compiler bugs on Blackwell (confirmed crash on our hardware).

## KV Cache Quantization

KV cache can dominate VRAM at long context lengths. Quantizing it is nearly free quality-wise:

| Type | VRAM vs f16 | Quality Impact | When to Use |
|------|-------------|----------------|-------------|
| `f16` | 100% | None | Short context, plenty of VRAM |
| `q8_0` | 50% | Negligible (~0.01% perplexity) | **Default for all GPUs** |
| `q4_0` | 25% | Minimal (~0.8% perplexity) | VRAM-constrained (large model + long context) |

Approximate KV cache size per 1K tokens (f16):

| Model Size | Per 1K tokens | 32K context | 128K context |
|------------|---------------|-------------|--------------|
| 8B | ~0.11 GB | ~3.5 GB | ~14 GB |
| 14B | ~0.18 GB | ~5.8 GB | ~23 GB |
| 27B | ~0.30 GB | ~9.6 GB | ~38 GB |

With `q8_0`, halve those. With `q4_0`, quarter them. **Flash attention is required** (`flash_attention = true`).

**Rules of thumb**:
- 24+ GB VRAM: use `q8_0` (no reason to go lower)
- 16 GB VRAM: use `q8_0` for 8-14B, `q4_0` for 27B+
- 10-12 GB VRAM: use `q4_0` whenever context matters

## MoE vs Dense

**MoE (Mixture of Experts)** models like Qwen3.5-35B-A3B activate only a fraction of parameters per token:
- **Faster generation** — only ~3B active params read from VRAM per token
- **More VRAM** — all 35B params must be loaded
- **Better quality per tok/s** — 35B model quality at 3B model speed
- **Weaker structured output** — some MoE models have less reliable tool calling

**Dense models** like Qwen3.5-27B read all parameters per token:
- **Slower generation** — all 27B params read per token
- **More predictable** — no routing overhead
- **Better for tool calling** — more reliable structured output and function calls

**Rule of thumb**: MoE for speed, dense for reliability. If you're running an agent that makes tool calls, prefer the largest dense model that fits.

## Partial Offload (GPU + CPU)

If a model doesn't fit in VRAM, llama-server can offload layers to CPU RAM:

```toml
gpu_layers = 30   # offload 30 layers to GPU, rest on CPU
```

**For dense models**: Not recommended. CPU memory bandwidth (~50-80 GB/s DDR5) is 10-30x slower than GPU. A 70B model with partial offload drops to ~10-18 tok/s. Pick a smaller model that fits entirely in VRAM.

**For MoE models**: More viable. Only active experts need to be read per token, and llama.cpp can stream expert activations between CPU and GPU. Models like Qwen3-235B-A22B can run at usable speeds with partial offload on a single GPU.

**Bottom line**: Always prefer a model that fits fully in VRAM. If you must go larger, pick an MoE architecture.

## GGUF Size Reference

Approximate file sizes by quant level:

| Params | Q3_K_M | Q4_K_M | Q5_K_M | Q6_K | Q8_0 |
|--------|--------|--------|--------|------|------|
| 7-8B | ~3.3 GB | ~4.6 GB | ~5.3 GB | ~6.1 GB | ~8 GB |
| 14B | ~6.5 GB | ~8.7 GB | ~10 GB | ~12 GB | ~15 GB |
| 27B | ~12 GB | ~16 GB | ~19 GB | ~22 GB | ~29 GB |
| 32B | ~14 GB | ~19 GB | ~23 GB | ~26 GB | ~34 GB |
| 70B | ~30 GB | ~42 GB | ~49 GB | ~58 GB | ~75 GB |

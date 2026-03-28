# Competitive Landscape (March 2026)

## Direct Competitors

### llama-swap (2.9k stars)
**Closest competitor.** Go binary that hot-swaps models on API requests.

| Feature | llama-swap | Rookery |
|---------|-----------|---------|
| Model hot-swap | API-based auto-swap | CLI/API profile swap |
| GPU monitoring | None | NVML (VRAM, temp, power, processes) |
| Agent management | None | Full lifecycle (watchdog, health, restart) |
| Inference health | None | Canary + CUDA stderr detection |
| Metrics export | Prometheus + Grafana | Dashboard only (no Prometheus yet) |
| Backend support | llama.cpp, vLLM, tabbyAPI, SD | llama.cpp (vLLM planned) |
| Model aliasing | Yes | No (use profile names) |
| Request rewriting | Yes | No |
| Docker orchestration | Yes | No (planned for vLLM) |

### llamactl (101 stars)
**Architecturally similar.** Go binary with React dashboard, multi-backend.

- Has: vLLM + MLX + llama.cpp backends, SQLite persistence, port range allocation
- Missing: GPU monitoring, agent management, inference canary, capacity gate

### Ollama (130k stars)
**Mainstream default.** User-friendly but adds ~20-30% overhead.

- Has: Massive ecosystem, Modelfile system, model library, cross-platform
- Missing: Direct llama.cpp control, agent management, per-profile params, reliability features
- Known issues: Stability under sustained load, hangs requiring restarts

### LM Studio (proprietary)
**GUI-first.** Desktop app with visual model browser.

- Has: Multi-GPU controls, polished UI, headless daemon mode, speculative decoding
- Missing: Agent management, inference canary, open source, CLI-first workflow

### GPUStack (7k stars)
**Enterprise cluster manager.** Multi-node GPU scheduling.

- Has: Multi-node, load balancing, Grafana/Prometheus, user management
- Overkill for single-machine use

### LocalAI (30k stars)
**Multi-modal Swiss army knife.** 35+ backends, image/audio/video.

- Has: Broad backend/modal support, dynamic memory reclaimer
- Missing: Agent management, per-model tuning, lightweight design

## Rookery's Unique Position

No single competitor has this combination:
1. **Inference canary** with auto-restart on CUDA zombie state
2. **Agent lifecycle management** (watchdog, error patterns, dependency port health)
3. **Orphan process adoption** across daemon restarts
4. **VRAM capacity gate** before model loading
5. **Hardware-aware model recommendations** from HuggingFace
6. **Per-profile sampling params** + custom chat templates

## Features to Adopt from Competitors

| Feature | Source | Priority | Effort |
|---------|--------|----------|--------|
| Prometheus metrics export | llama-swap, GPUStack | High | Medium |
| Multi-backend (vLLM, MLX) | llamactl | High | Already planned |
| Model aliasing / friendly names | llama-swap | Medium | Low |
| Request rewriting / filtering | llama-swap | Low | Medium |
| Docker container orchestration | llama-swap | Medium | Part of vLLM plan |
| Auto-unload on idle timeout | llama-swap, KoboldCpp | Medium | Medium |
| Swagger/OpenAPI docs | llamactl | Low | Low |

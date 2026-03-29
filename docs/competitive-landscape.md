# Competitive Landscape

How Rookery compares to other local inference management tools.

## Comparison

### llama-swap
Go binary that hot-swaps models on API requests. The closest tool to Rookery in concept.

| Feature | llama-swap | Rookery |
|---------|-----------|---------|
| Model hot-swap | Auto-swap on request | CLI/API/dashboard profile swap |
| GPU monitoring | None | NVML (VRAM, temp, power, per-process) |
| Agent management | None | Full lifecycle (watchdog, health, error patterns) |
| Inference health | None | Canary + CUDA stderr detection + auto-restart |
| Idle management | Auto-unload | Auto-sleep with wake-on-request |
| Metrics | Prometheus + Grafana | Prometheus `/metrics` + live dashboard |
| Backend support | llama.cpp, vLLM, tabbyAPI, SD | llama.cpp, vLLM (Docker Compose) |
| Model aliasing | Yes | Yes (profile aliases) |
| Request rewriting | Yes | No |
| Dashboard | No | Embedded WASM (7 tabs) |
| Model discovery | No | HuggingFace search + VRAM-aware recommendations |

### Ollama (130k+ stars)
The mainstream default for local inference. User-friendly but adds overhead and limits control.

- Massive ecosystem, Modelfile system, built-in model library, cross-platform
- Adds ~20-30% inference overhead vs raw llama.cpp
- No direct control over llama.cpp parameters, no agent management, no per-profile sampling params
- Known stability issues under sustained load

### LM Studio (proprietary)
GUI-first desktop app with a polished model browser.

- Multi-GPU controls, visual quant selector, headless daemon mode, speculative decoding
- Proprietary, no CLI-first workflow, no agent management, no inference canary

### GPUStack (7k stars)
Enterprise-grade multi-node GPU cluster manager.

- Multi-node scheduling, load balancing, Grafana/Prometheus, user management
- Designed for clusters — overkill for single-machine setups

### LocalAI (30k stars)
Multi-modal Swiss army knife with 35+ backends.

- Broad backend and modality support (image, audio, video), dynamic memory reclaimer
- No agent management, limited per-model tuning, heavier footprint

### llamactl
Go binary with React dashboard and multi-backend support.

- vLLM + MLX + llama.cpp backends, SQLite persistence, port range allocation
- No GPU monitoring, no agent management, no inference canary

## What Makes Rookery Different

Rookery is built for **always-on single-machine inference** — the use case where you run a local model 24/7 for agents, coding tools, and chat, and you need it to stay healthy without babysitting.

No single competitor has this combination:

1. **Inference canary** — periodic completion requests detect CUDA zombie state, auto-restart on double failure
2. **Agent lifecycle management** — watchdog with crash recovery, dependency port health, error pattern restart, exponential backoff
3. **Orphan process adoption** — daemon restart discovers and adopts running llama-server processes
4. **Auto-sleep / wake-on-request** — unloads after idle timeout, wakes transparently on next API call
5. **VRAM capacity gate** — checks free GPU memory before loading a model
6. **Model discovery** — search HuggingFace, browse quants with VRAM-aware recommendations, one-click download
7. **Live dashboard** — embedded WASM frontend with GPU gauges, agent controls, chat playground, model browser
8. **Multi-backend** — llama-server and vLLM from the same config, hot-swap between them

## Feature Matrix

| Feature | Rookery | llama-swap | Ollama | GPUStack | LocalAI |
|---------|---------|-----------|--------|----------|---------|
| Hot-swap profiles | Yes | Yes | No | No | No |
| Multi-backend | Yes | Partial | No | Partial | Yes |
| Live dashboard | Yes | No | No | Yes | No |
| Agent management | Yes | No | No | No | No |
| Model browser + download | Yes | No | No | Yes | Yes |
| VRAM-aware recommendations | Yes | No | No | Yes | No |
| Auto-sleep / wake | Yes | Yes | Partial | No | No |
| Inference canary | Yes | No | No | Yes | No |
| CUDA crash detection | Yes | No | No | No | No |
| Prometheus metrics | Yes | Yes | No | Yes | Yes |
| Single binary + dashboard | Yes | Yes | Yes | No | No |
| API key auth | Yes | No | No | Yes | Yes |
| systemd + OOM protection | Yes | No | No | Yes | No |

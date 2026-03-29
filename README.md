# rookery

Local inference command center. Manage llama-server and vLLM backends, hot-swap models, monitor GPU, run agents, and browse models — all from one daemon + CLI + live dashboard.

[![CI](https://github.com/lance0/rookery/actions/workflows/ci.yml/badge.svg)](https://github.com/lance0/rookery/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

## Quick Start

```bash
git clone https://github.com/lance0/rookery.git
cd rookery
sudo make install                  # build, install to /usr/local/bin, set up systemd
sudo systemctl enable --now rookery

# Use the CLI
rookery status                     # server state + uptime
rookery gpu                        # VRAM, temp, power, processes
rookery swap qwen_thinking         # hot-swap to another model profile
rookery bench                      # quick PP + gen speed benchmark
```

Open the dashboard at your configured address (default `http://localhost:3000`) — live GPU gauges, profile switcher, agent controls, chat playground, model browser.

See [Installation](#installation) below for all methods.

## Features

- **Multi-backend** — manage llama-server (GGUF) and vLLM (safetensors, AWQ, GPTQ, NVFP4) from the same config
- **Hot-swap** — switch between model profiles without restarting the daemon
- **Live dashboard** — Leptos WASM frontend with 7 tabs: Overview, Settings, Agents, Chat, Bench, Logs, Models
- **GPU monitoring** — real-time VRAM, temperature, utilization, power draw, per-process memory via NVML
- **Agent management** — spawn, stop, update, and watchdog external processes (coding agents, Telegram bots, etc.)
- **Model discovery** — search HuggingFace, browse quants, VRAM-aware recommendations, one-click download
- **Auto-sleep** — unloads the model after idle timeout, wakes transparently on next request
- **Inference canary** — periodic health checks detect CUDA zombies and auto-restart
- **Prometheus metrics** — `/metrics` endpoint for GPU, server, agent, and canary telemetry
- **Optional API key auth** — single bearer token gates all API and dashboard access
- **systemd integration** — OOM protection, journal logging, graceful shutdown

### vs Alternatives

| Feature | rookery | llama-swap | GPUStack | LocalAI |
|---------|---------|------------|----------|---------|
| Hot-swap profiles | Yes | Yes | No | No |
| Multi-backend (llama.cpp + vLLM) | Yes | No | Partial | Yes |
| Live dashboard | Yes (WASM) | No | Yes | No |
| Agent lifecycle management | Yes | No | No | No |
| Model browser + download | Yes | No | Yes | Yes |
| VRAM-aware recommendations | Yes | No | Yes | No |
| Auto-sleep / wake-on-request | Yes | Yes | No | No |
| Inference canary + auto-restart | Yes | No | Yes | No |
| Prometheus metrics | Yes | No | Yes | Yes |
| Single binary + embedded dashboard | Yes | Yes | No | No |

## Real-World Use Cases

### Daily Driver for a Telegram Agent
Run a dense model for reliable tool calling, with auto-restart if the agent crashes:
```bash
rookery start qwen_dense           # 27B Q6 for best tool accuracy
rookery agent start hermes         # Telegram gateway with crash watchdog
```

### Quick Experimentation
Hot-swap between models without restarting anything:
```bash
rookery start qwen_fast            # MoE at 160 tok/s
rookery bench                      # measure performance
rookery swap qwen_dense            # switch to dense 27B
rookery bench                      # compare
```

### Headless Server with Auto-Sleep
Run 24/7 with minimal power draw when idle:
```toml
auto_start = true
idle_timeout = 1800   # unload after 30 min idle
```
The model unloads after inactivity. Next API request wakes it transparently.

### Model Shopping
Find the best quant for your GPU without leaving the terminal:
```bash
rookery models search Qwen3.5-27B
rookery models quants Qwen3.5-27B  # shows VRAM fit + estimated tok/s
rookery models pull Qwen3.5-27B    # downloads best-fit quant
```

## Installation

### From Source (Recommended)

Requires [Rust 1.85+](https://www.rust-lang.org/tools/install) and an NVIDIA GPU with CUDA drivers.

```bash
git clone https://github.com/lance0/rookery.git
cd rookery
sudo make install
```

This builds both binaries, installs them to `/usr/local/bin`, and sets up a systemd unit. Customize with:

```bash
sudo make install PREFIX=/opt/rookery SERVICE_USER=myuser HF_HOME=/mnt/models
```

### Pre-built Binaries

Download from [GitHub Releases](https://github.com/lance0/rookery/releases):

| Platform | Target |
|----------|--------|
| Linux x86_64 | `rookery-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `rookery-aarch64-unknown-linux-gnu.tar.gz` |

```bash
curl -LO https://github.com/lance0/rookery/releases/latest/download/rookery-x86_64-unknown-linux-gnu.tar.gz
tar xzf rookery-*.tar.gz
sudo mv rookeryd rookery /usr/local/bin/
```

### Quick Install Script

> **Note**: Review scripts before piping to sh. See the [install script](install.sh) source.

```bash
curl -fsSL https://raw.githubusercontent.com/lance0/rookery/main/install.sh | sh
```

## Configuration

Config file: `~/.config/rookery/config.toml`

Models define *what* to run, profiles define *how* to run it. Multiple profiles can share a model.

```toml
llama_server = "/path/to/llama-server"
default_profile = "qwen_fast"
auto_start = true
idle_timeout = 1800

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 25800

[profiles.qwen_fast]
model = "qwen35"
aliases = ["fast", "moe"]
port = 8081

[profiles.qwen_fast.llama_server]
ctx_size = 262144
flash_attention = true
reasoning_budget = 0
temp = 0.7
top_p = 0.8
```

See [config.example.toml](config.example.toml) for all options including vLLM backend, KV cache tuning, agent management, and API key auth.

Full reference: [docs/configuration.md](docs/configuration.md)

## Dashboard

The embedded dashboard runs at your configured `listen` address. Seven tabs with keyboard shortcuts:

| Tab | Key | Purpose |
|-----|-----|---------|
| Overview | `1` | GPU gauges, server status, model info, agent summary |
| Settings | `2` | Profile switcher, sampling param editor |
| Agents | `3` | Agent cards, controls, watchdog state, filtered logs |
| Chat | `4` | Streaming chat playground with abort |
| Bench | `5` | PP + gen speed benchmark |
| Logs | `6` | Live log viewer |
| Models | `7` | Search HF, browse quants, download |

Additional shortcuts: `s` start, `x` stop, `t` toggle theme.

## CLI Reference

```
rookery status              # server state, profile, PID, uptime
rookery gpu                 # VRAM, temp, utilization, power, processes
rookery start [profile]     # start server (or default profile)
rookery stop                # stop server
rookery sleep               # unload model, keep profile for fast wake
rookery wake                # wake sleeping profile
rookery swap <profile>      # hot-swap to another profile
rookery profiles            # list available profiles
rookery bench               # PP + gen speed benchmark
rookery logs [-f] [-n N]    # fetch or follow log lines
rookery agent start <name>  # start a managed agent
rookery agent stop <name>   # stop a managed agent
rookery agent update <name> # stop, update, restart
rookery agent status        # list agents
rookery agent describe <name> # detailed health, watchdog, errors
rookery models search <q>   # search HuggingFace
rookery models quants <repo> # list quants with VRAM fit
rookery models pull <repo>  # download best-fit quant
rookery models list         # locally cached models
rookery models hardware     # GPU/CPU/RAM profile
rookery config              # validate config
rookery auth generate       # generate a random API key
rookery completions <shell> # generate shell completions
```

All commands support `--json` for scripting.

## API

The daemon exposes a REST API. When `api_key` is configured, all routes require `Authorization: Bearer <key>` except `/api/health` and `/metrics`.

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/health` | GET | Daemon health check (always open) |
| `/api/status` | GET | Server state, profile, PID, uptime |
| `/api/gpu` | GET | GPU stats (VRAM, temp, utilization, power, processes) |
| `/api/start` | POST | Start server `{ "profile": "name" }` |
| `/api/stop` | POST | Stop server |
| `/api/sleep` | POST | Put server into sleeping state |
| `/api/wake` | POST | Wake sleeping profile |
| `/api/swap` | POST | Hot-swap `{ "profile": "name" }` |
| `/api/profiles` | GET | List available profiles |
| `/api/bench` | GET | Run PP + gen benchmark |
| `/api/logs` | GET | Fetch log lines `?n=50` |
| `/api/events` | GET | SSE stream (gpu, state, log events) |
| `/api/chat` | POST | Streaming chat proxy (auto-wakes sleeping backends) |
| `/api/agents` | GET | List agents with health metrics |
| `/api/agents/start` | POST | Start agent `{ "name": "..." }` |
| `/api/agents/stop` | POST | Stop agent |
| `/api/agents/{name}/update` | POST | Stop, update, restart agent |
| `/api/agents/{name}/health` | GET | Detailed health (watchdog, backoff, deps) |
| `/api/config` | GET | Full config (secrets redacted) |
| `/api/config/profile/{name}` | PUT | Update profile sampling params |
| `/api/model-info` | GET | Model ID, context window |
| `/api/server-stats` | GET | Slot status, request count |
| `/api/hardware` | GET | Hardware profile (GPU, CPU, RAM) |
| `/api/models/search` | GET | Search HuggingFace `?q=query` |
| `/api/models/quants` | GET | List quants `?repo=name` |
| `/api/models/cached` | GET | Locally cached models |
| `/api/models/pull` | POST | Download model `{ "repo": "...", "quant": "..." }` |
| `/metrics` | GET | Prometheus/OpenMetrics (always open) |

## Architecture

```
crates/
  rookery-core/       # config, state machine, shared types
  rookery-engine/     # process manager, GPU monitor, health checker, agent manager
  rookery-daemon/     # axum REST API, SSE, auth middleware, embedded dashboard
  rookery-dashboard/  # Leptos WASM frontend (built with trunk, embedded into daemon)
  rookery-cli/        # clap CLI client
```

Two binaries:
- **`rookeryd`** — long-running daemon (axum REST API + embedded dashboard)
- **`rookery`** — thin CLI that talks to the daemon over HTTP

The daemon reconciles persisted state on startup, adopts orphan processes, auto-starts configured agents, and cleans up stale llama-servers. The `InferenceBackend` trait abstracts over llama-server and vLLM backends.

## Platform Support

| Platform | Status |
|----------|--------|
| Linux x86_64 + NVIDIA GPU | Supported |
| Linux ARM64 + NVIDIA GPU | Supported (Jetson, etc.) |
| AMD GPUs (ROCm) | Not tested |
| macOS (Metal) | Not supported (no NVML) |

## Documentation

- [Quick Start](docs/quickstart.md) — build, configure, run
- [Configuration](docs/configuration.md) — full config reference
- [Agent Management](docs/agents.md) — watchdog, crash recovery, update
- [API Reference](docs/api.md) — REST endpoints
- [CLI Reference](docs/cli.md) — all commands and flags
- [Models](docs/models.md) — model sources, discovery, download
- [Dashboard](docs/dashboard.md) — tabs, shortcuts, features
- [Architecture](docs/architecture.md) — crate structure, design decisions
- [vLLM Integration](docs/vllm-integration-summary.md) — Docker backend

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, code style, and PR guidelines.

## License

Licensed under either of:

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.

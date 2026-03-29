# Rookery

Local inference command center. Manages llama-server and vLLM backends, GPU monitoring, model profiles, agent lifecycle, and hot-swap from a single daemon + CLI. Supports multiple inference backends via the `InferenceBackend` trait.

**[Documentation](docs/README.md)** — Quick start, configuration reference, agent management, API reference, CLI reference, architecture.

## Quick Start

```bash
# Build
cargo build --release

# Copy and edit config
mkdir -p ~/.config/rookery
cp config.example.toml ~/.config/rookery/config.toml

# Start daemon
./target/release/rookeryd &

# Use CLI
rookery status              # show server state
rookery gpu                 # GPU stats (VRAM, temp, power, processes)
rookery start               # start default profile
rookery start qwen_dense    # start specific profile
rookery stop                # stop server
rookery sleep               # unload the model but keep last profile for fast wake
rookery wake                # wake the sleeping profile
rookery swap qwen_thinking  # hot-swap to another profile
rookery profiles            # list available profiles
rookery bench               # quick PP + gen speed benchmark
rookery logs                # last 50 log lines
rookery logs -f             # follow mode (stream via SSE)
rookery agent start hermes  # start a managed agent
rookery agent stop hermes   # stop a managed agent
rookery agent status        # list agents and their status
rookery agent describe hermes # detailed health, restarts, errors
rookery config              # validate config, show resolved commands
rookery completions bash    # generate shell completions
```

## Dashboard

Open the dashboard at your configured `listen` address (default: `http://127.0.0.1:3000/`):

- **Overview** — GPU gauges, server status, model info, server stats, agent panel with health metrics
- **Settings** — profile switcher, sampling param editor (saves to config.toml), agent controls
- **Chat** — streaming chat playground (SSE proxy to llama-server)
- **Bench** — PP + gen speed benchmark with error toasts
- **Logs** — live log viewer
- **Models** — search HuggingFace, browse quants, VRAM-aware recommendations, download

Keyboard shortcuts: `1`-`6` switch tabs, `s` start, `x` stop, `t` toggle theme. Mobile responsive. All data streams via SSE with automatic reconnection.

## Architecture

Two binaries:

- **`rookeryd`** — long-running daemon (axum REST API on configured `listen` address, default `127.0.0.1:3000`)
- **`rookery`** — thin CLI that talks to the daemon over HTTP

The daemon manages the llama-server lifecycle, monitors GPU via NVML, captures logs, manages agents, and persists state across restarts. On startup it reconciles persisted state (both server and agent PIDs), adopts orphan processes, auto-starts configured agents, and cleans up stale llama-servers hogging VRAM.

## Config

`~/.config/rookery/config.toml` — models define *what* to run, profiles define *how* to run it. Multiple profiles can share a model. Agents define external processes (coding agents, etc.) managed alongside the server.

```toml
llama_server = "/path/to/llama-server"
default_profile = "qwen_fast"
idle_timeout = 1800

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 25800

[profiles.qwen_fast]
model = "qwen35"
aliases = ["qwen", "fast"]
port = 8081
ctx_size = 262144
reasoning_budget = 0
# ... see config.example.toml for full options

[agents.hermes]
command = "opencode"
args = []
workdir = "/home/user/projects/myproject"
auto_start = false
restart_on_swap = true
```

## API

The daemon exposes a REST API:

| Endpoint | Method | Description |
|---|---|---|
| `/` | GET | Live HTML dashboard |
| `/api/health` | GET | Daemon health check |
| `/api/status` | GET | Server state, profile, PID, uptime |
| `/api/gpu` | GET | GPU stats (VRAM, temp, utilization, power, processes) |
| `/api/start` | POST | Start server `{ "profile": "name" }` (idempotent, capacity-gated) |
| `/api/stop` | POST | Stop server |
| `/api/sleep` | POST | Put the running server into `sleeping` state |
| `/api/wake` | POST | Wake the sleeping profile |
| `/api/swap` | POST | Hot-swap profile `{ "profile": "name" }` |
| `/api/profiles` | GET | List available profiles |
| `/api/bench` | GET | Run benchmark (PP + gen tok/s) |
| `/api/logs` | GET | Fetch log lines `?n=50` |
| `/api/events` | GET | SSE stream (gpu, state, log events) |
| `/metrics` | GET | Prometheus/OpenMetrics scrape endpoint |
| `/api/agents` | GET | List agents and their status |
| `/api/agents/start` | POST | Start agent `{ "name": "hermes" }` |
| `/api/agents/stop` | POST | Stop agent `{ "name": "hermes" }` |
| `/api/config` | GET | Full config (agent env vars redacted) |
| `/api/config/profile/{name}` | PUT | Update profile sampling params |
| `/api/model-info` | GET | Model ID, context window from llama-server |
| `/api/server-stats` | GET | Slot status, request count from llama-server |
| `/api/chat` | POST | Streaming chat proxy to llama-server (auto-wakes sleeping backends) |
| `/api/agents/{name}/health` | GET | Detailed agent health (uptime, restarts, errors) |
| `/api/hardware` | GET | Hardware profile (GPU, CPU, RAM) |
| `/api/models/search` | GET | Search HuggingFace for GGUF repos |
| `/api/models/quants` | GET | List available quants for a repo |
| `/api/models/recommend` | GET | VRAM-aware quant recommendation |
| `/api/models/cached` | GET | List locally cached models |
| `/api/models/pull` | POST | Download a model |

`/metrics` exposes Prometheus-compatible text for GPU, server, canary, agent, chat, and SSE telemetry. GPU and health-style gauges are computed on scrape from current daemon state; restart and request counters are daemon-runtime counters and reset when `rookeryd` restarts. `idle_timeout` enables daemon-side auto-sleep: after inference inactivity, the backend unloads into `sleeping` state and the next `/api/chat` request wakes it transparently.

## systemd

```bash
sudo cp rookery.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now rookeryd
```

The unit file grants `CAP_SYS_RESOURCE` so the daemon can set `oom_score_adj=-900` on llama-server, protecting it from the OOM killer. Logs go to journalctl (`journalctl -u rookeryd -f`).

## Workspace

```
crates/
  rookery-core/       # config, state machine (server + agent), shared types
  rookery-engine/     # process manager, GPU monitor, health checker, log buffer, agent manager
  rookery-daemon/     # axum REST API server, SSE, embedded dashboard
  rookery-dashboard/  # Leptos WASM frontend (built with trunk, embedded into daemon)
  rookery-cli/        # clap CLI client
```

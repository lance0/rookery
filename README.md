# Rookery

Local inference command center. Manages llama-server processes, GPU monitoring, model profiles, agent lifecycle, and hot-swap from a single daemon + CLI.

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
rookery swap qwen_thinking  # hot-swap to another profile
rookery profiles            # list available profiles
rookery bench               # quick PP + gen speed benchmark
rookery logs                # last 50 log lines
rookery logs -f             # follow mode (stream via SSE)
rookery agent start hermes  # start a managed agent
rookery agent stop hermes   # stop a managed agent
rookery agent status        # list agents and their status
rookery config              # validate config, show resolved commands
rookery completions bash    # generate shell completions
```

## Dashboard

Open `http://127.0.0.1:3000/` in a browser for a live dashboard with GPU gauges, server status, profile switcher, agent controls, and a log viewer. All data streams via SSE.

## Architecture

Two binaries:

- **`rookeryd`** — long-running daemon (axum REST API on `127.0.0.1:3000`)
- **`rookery`** — thin CLI that talks to the daemon over HTTP

The daemon manages the llama-server lifecycle, monitors GPU via NVML, captures logs, manages agents, and persists state across restarts. On startup it reconciles persisted state, adopts orphan processes, and cleans up stale llama-servers hogging VRAM.

## Config

`~/.config/rookery/config.toml` — models define *what* to run, profiles define *how* to run it. Multiple profiles can share a model. Agents define external processes (coding agents, etc.) managed alongside the server.

```toml
llama_server = "/path/to/llama-server"
default_profile = "qwen_fast"

[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 25800

[profiles.qwen_fast]
model = "qwen35"
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
| `/api/swap` | POST | Hot-swap profile `{ "profile": "name" }` |
| `/api/profiles` | GET | List available profiles |
| `/api/bench` | GET | Run benchmark (PP + gen tok/s) |
| `/api/logs` | GET | Fetch log lines `?n=50` |
| `/api/events` | GET | SSE stream (gpu, state, log events) |
| `/api/agents` | GET | List agents and their status |
| `/api/agents/start` | POST | Start agent `{ "name": "hermes" }` |
| `/api/agents/stop` | POST | Stop agent `{ "name": "hermes" }` |

## Workspace

```
crates/
  rookery-core/     # config, state machine, shared types
  rookery-engine/   # process manager, GPU monitor, health checker, log buffer, agent manager
  rookery-daemon/   # axum REST API server, SSE, dashboard
  rookery-cli/      # clap CLI client
```

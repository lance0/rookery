# Rookery

Local inference command center. Manages llama-server processes, GPU monitoring, and model profiles from a single daemon + CLI.

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
rookery status          # show server state
rookery gpu             # GPU stats (VRAM, temp, power)
rookery start           # start default profile
rookery start qwen_dense  # start specific profile
rookery stop            # stop server
rookery config          # validate config, show resolved commands
```

## Architecture

Two binaries:

- **`rookeryd`** — long-running daemon (axum REST API on `127.0.0.1:3000`)
- **`rookery`** — thin CLI that talks to the daemon over HTTP

The daemon manages the llama-server lifecycle, monitors GPU via NVML, captures logs, and persists state across restarts.

## Config

`~/.config/rookery/config.toml` — models define *what* to run, profiles define *how* to run it. Multiple profiles can share a model.

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
```

## API

The daemon exposes a REST API:

| Endpoint | Method | Description |
|---|---|---|
| `/api/health` | GET | Daemon health check |
| `/api/status` | GET | Server state, profile, PID, uptime |
| `/api/gpu` | GET | GPU stats (VRAM, temp, utilization, power) |
| `/api/start` | POST | Start server `{ "profile": "name" }` |
| `/api/stop` | POST | Stop server |

## Workspace

```
crates/
  rookery-core/     # config, state machine, shared types
  rookery-engine/   # process manager, GPU monitor, health checker, log buffer
  rookery-daemon/   # axum REST API server
  rookery-cli/      # clap CLI client
```

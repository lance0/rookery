# Rookery

Local inference command center — Rust daemon + CLI for managing llama-server.

All 7 phases complete (MVP, agents, hot-swap, dashboard, polish, reliability, production hardening).

## Stack
- **Rust** (2024 edition, workspace with 4 crates)
- **axum** — daemon REST API + SSE streaming
- **clap** — CLI with derive macros + clap_complete for shell completions
- **nvml-wrapper** — GPU monitoring via NVML (stats + process enumeration)
- **tokio** — async runtime, broadcast channels, signal handling
- **chrono** — timestamps for state persistence and uptime tracking
- **nix** — SIGTERM/SIGKILL for process management

## Build
```bash
cargo build --release    # release binaries in target/release/
cargo test --workspace   # run all tests
```

## Binaries
- `rookeryd` — daemon, listens on 127.0.0.1:3000
- `rookery` — CLI, talks to daemon via HTTP

## CLI Commands
- `start [profile]` — start server (idempotent, capacity-gated)
- `stop` — stop server
- `status [--json]` — server state, profile, PID, uptime
- `gpu [--json]` — GPU stats
- `swap <profile>` — hot-swap to different profile
- `profiles` — list available profiles
- `bench` — quick PP + gen benchmark
- `logs [-f] [-n N]` — view/follow server logs
- `agent start|stop|status` — manage agents
- `config` — validate config, show resolved commands
- `completions <shell>` — generate shell completions

## Config
- Location: `~/.config/rookery/config.toml`
- State: `~/.local/state/rookery/state.json`
- Example: `config.example.toml`

## Crate Layout
- `rookery-core` — Config (TOML parsing, model/profile/agent definitions), ServerState (state machine + persistence + reconciliation), Error types
- `rookery-engine` — ProcessManager (spawn/stop/adopt llama-server), AgentManager (spawn/stop agents), GpuMonitor (NVML stats + orphan detection), HealthChecker (exponential backoff), LogBuffer (ring buffer + broadcast)
- `rookery-daemon` — axum routes, SSE (merged gpu/state/log streams), AppState, dashboard (embedded HTML), signal handling, graceful shutdown
- `rookery-cli` — clap commands, DaemonClient (HTTP)

## Key Patterns
- **Daemon owns all state** — CLI is stateless, just HTTP calls
- **State machine**: Stopped -> Starting -> Running -> Stopping -> Failed
- **State persisted to JSON**, reconciled on daemon restart via /proc/<pid>/exe
- **Orphan process adoption** — on restart, daemon finds previously-running llama-server via persisted state, adopts its PID into ProcessManager (no child handle, falls back to kill-by-PID on stop)
- **Orphan cleanup** — on startup, scans NVML GPU process list for untracked llama-server processes, kills them (SIGTERM then SIGKILL)
- **Capacity gate** — checks free VRAM against model's estimated_vram_mb before starting, rejects with clear error
- **Operation mutex** — `op_lock` serializes start/stop/swap to prevent concurrent state-changing operations from racing
- **Atomic saves** — config and state persistence write to `.tmp` then `rename()` to prevent corruption on crash
- **OOM protection** — llama-server gets `oom_score_adj=-900` after spawn (requires CAP_SYS_RESOURCE via systemd)
- **Swap drain** — 5s grace period before killing old server, new chat requests get 503 during drain
- **Agent persistence** — agent PIDs saved to `agents.json`, reconciled+adopted on daemon restart; `auto_start` config honored
- **Idempotent start** — if already running with same profile, returns success no-op; if different profile, returns error with hint to use swap
- **SSE streaming** — single `/api/events` endpoint merges three streams: GPU stats (2s interval), state changes (broadcast channel), log lines (broadcast channel)
- **Graceful shutdown** — SIGTERM/SIGINT triggers stop of all agents and llama-server, persists Stopped state
- **Agent management** — agents are external processes (e.g., coding agents) with config for command, args, workdir, env; restart_on_swap auto-restarts after model swap
- **Config separates models (what) from profiles (how)** — multiple profiles can share a model

## Dependencies on lancebox-inference
- `llama_server` path in config points to lancebox-inference's build
- `chat_template` path in profiles points to the patched jinja template
- No code dependency — just config file paths

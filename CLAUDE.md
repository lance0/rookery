# Rookery

Local inference command center — Rust daemon + CLI for managing llama-server.

## Stack
- **Rust** (2024 edition, workspace with 4 crates)
- **axum** — daemon REST API
- **clap** — CLI with derive macros
- **nvml-wrapper** — GPU monitoring via NVML
- **tokio** — async runtime

## Build
```bash
cargo build --release    # release binaries in target/release/
cargo test --workspace   # run all tests
```

## Binaries
- `rookeryd` — daemon, listens on 127.0.0.1:3000
- `rookery` — CLI, talks to daemon via HTTP

## Config
- Location: `~/.config/rookery/config.toml`
- State: `~/.local/state/rookery/state.json`
- Example: `config.example.toml`

## Crate Layout
- `rookery-core` — Config (TOML parsing), ServerState (state machine + persistence), Error types
- `rookery-engine` — ProcessManager (spawn/stop llama-server), GpuMonitor (NVML), HealthChecker (exponential backoff), LogBuffer (ring buffer + broadcast)
- `rookery-daemon` — axum routes, AppState, signal handling
- `rookery-cli` — clap commands, DaemonClient (HTTP)

## Key Patterns
- Daemon owns all state — CLI is stateless, just HTTP calls
- State machine: Stopped → Starting → Running → Stopping → Failed
- State persisted to JSON, reconciled on daemon restart via /proc/<pid>/exe
- ProcessManager captures stdout/stderr into LogBuffer
- GpuMonitor returns Vec<GpuStats> (multi-GPU ready, single GPU for now)
- Config separates models (what) from profiles (how)

## Dependencies on lancebox-inference
- `llama_server` path in config points to lancebox-inference's build
- `chat_template` path in profiles points to the patched jinja template
- No code dependency — just config file paths

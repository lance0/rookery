# Architecture

## Workspace Structure

```
crates/
  rookery-core/       Config, state machine, shared types
  rookery-engine/     Process manager, GPU monitor, health checker,
                      log buffer, agent manager, model discovery
  rookery-daemon/     Axum REST API, SSE, embedded dashboard
  rookery-dashboard/  Leptos WASM frontend (built with trunk)
  rookery-cli/        Clap CLI client
```

## Design Decisions

### Two Binaries

- **rookeryd** — long-running daemon that owns the llama-server process, monitors GPU, manages agents
- **rookery** — stateless CLI that makes HTTP calls to the daemon

This split means the CLI can be used from any terminal, scripts, or CI. The daemon maintains all state.

### State Persistence

Server state and agent state are persisted to JSON files (`~/.config/rookery/state.json`, `~/.config/rookery/agents.json`). On daemon restart, state is reconciled:

1. Check if persisted PIDs are still alive (not zombies — reads `/proc/pid/stat`)
2. Adopt running processes or mark as stopped
3. Run inference canary on adopted servers (not just health check)
4. Bounce adopted agents for fresh connections
5. Auto-start configured agents

### Agent Watchdog

A single background task polls every 30 seconds:

1. Check dependency ports for down→up transitions (server restart detection)
2. Check agent PIDs for liveness (zombie-aware via `/proc/pid/stat`)
3. Listen for fatal error patterns via watch channel (immediate restart)
4. Restart dead agents with exponential backoff (1s → 60s cap)
5. Reset backoff after 5 minutes of healthy uptime

### Inference Canary

Separate from the agent watchdog. Sends a 1-token completion request every 60 seconds to verify the CUDA inference pipeline is functional. Also triggered immediately on CUDA error detection in llama-server stderr. On double failure, auto-restarts the server.

### Dashboard Embedding

The Leptos WASM dashboard is compiled to static files via `trunk build`, then embedded into the daemon binary via `include_dir!`. No separate web server needed. After rebuilding the dashboard, touch a daemon source file and rebuild to re-embed.

### Atomic Config Saves

All config and state writes use write-to-tempfile + rename pattern to prevent corruption from crashes during write.

### Operation Mutex

Start, stop, and swap operations acquire an `op_lock` mutex to prevent concurrent state mutations. The inference canary also acquires this lock before restarting the server.

## Key Patterns

| Pattern | Used By | Purpose |
|---------|---------|---------|
| `watch::channel` | CUDA errors, fatal agent errors | Trigger immediate action from background tasks |
| `broadcast::channel` | Log buffer, SSE events | Fan-out to multiple subscribers |
| `Arc<AtomicU32>` | Agent error counts | Shared counter between stderr task and health endpoint |
| `Arc<Mutex<HashMap>>` | Agent tracking | Concurrent access from watchdog + API handlers |
| `/proc/pid/stat` parsing | Zombie detection | Distinguish dead processes from zombies |
| Exponential backoff | Agent restarts | Prevent restart storms (1s, 2s, 4s... 60s cap) |

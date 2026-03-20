# Rookery Design

## Philosophy

Rookery is a personal tool for managing local LLM inference on a single workstation with high-end GPU(s). It's not a cloud orchestrator or a multi-user system. Design for one user, one machine, simplicity over configurability.

## Architecture: Daemon + CLI

The daemon (`rookeryd`) is the single source of truth. It owns:
- Process lifecycle (spawn, health check, stop, crash detection)
- GPU monitoring (NVML polling)
- State machine (persisted across restarts)
- Log capture (ring buffer with broadcast streaming)
- REST API for all interactions

The CLI (`rookery`) is a thin HTTP client. It has no local state. This means:
- `rookery status` works from any terminal
- Multiple shells can interact with the same daemon
- The daemon can outlive any terminal session

### Why not CLI-as-orchestrator?

A CLI-only approach means every invocation has to rediscover state (read PID files, check processes, parse logs). Features like `logs -f` or live GPU streaming become awkward hacks. With a daemon, these are simple SSE streams.

## State Machine

```
Stopped ──→ Starting ──→ Running
   ↑            │            │
   │            ↓            ↓
   ├──────── Failed      Stopping
   │                        │
   └────────────────────────┘
```

Future: `Swapping { from, to, phase }` state for hot-swap.

State is persisted to `~/.local/state/rookery/state.json`. On daemon restart, it reconciles: checks if the previously-running PID is still alive and matches the expected executable. Transient states (Starting, Stopping) reset to Stopped.

## Config: Models vs Profiles

Models define *what* to run (source, file, VRAM estimate). Profiles define *how* to run it (context size, threads, sampling params, reasoning budget). Multiple profiles can reference the same model.

This separation means:
- Adding a "thinking" variant is just a new profile, not a new model download
- VRAM estimates live on the model, not duplicated per profile
- Swapping between profiles that share a model is faster (no re-download)

## Process Management

ProcessManager spawns llama-server as a child process with stdout/stderr captured. On stop:
1. SIGTERM (graceful)
2. Wait 10 seconds
3. SIGKILL (forced)

Process identity is tracked beyond just PID — we record the executable path and verify via `/proc/<pid>/exe` on reconciliation. This prevents the "PID reused by unrelated process" footgun.

## Security

- Daemon binds to `127.0.0.1` only — not exposed to network
- llama-server binds to `0.0.0.0` for LAN access (configurable per profile)
- No authentication on daemon API (localhost only)
- Future: optional bearer token for remote access

## Future Phases

See ROADMAP.md for planned features and timeline.

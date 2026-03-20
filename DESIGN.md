# Rookery Design

## Philosophy

Rookery is a personal tool for managing local LLM inference on a single workstation with high-end GPU(s). It's not a cloud orchestrator or a multi-user system. Design for one user, one machine, simplicity over configurability.

## Architecture: Daemon + CLI

The daemon (`rookeryd`) is the single source of truth. It owns:
- Process lifecycle (spawn, health check, stop, crash detection)
- GPU monitoring (NVML polling)
- State machine (persisted across restarts)
- Log capture (ring buffer with broadcast streaming)
- Agent management (spawn/stop external processes)
- REST API + SSE for all interactions
- Embedded web dashboard

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

State is persisted to `~/.local/state/rookery/state.json`. On daemon restart, it reconciles: checks if the previously-running PID is still alive and matches the expected executable. Transient states (Starting, Stopping) reset to Stopped.

## Orphan Process Adoption

When the daemon restarts while a llama-server is still running:

1. **Reconciliation** — load persisted state, verify the PID is alive via `/proc/<pid>/exe`
2. **Adoption** — if the process is alive and matches, call `ProcessManager::adopt()` to register the PID. No child handle exists (it was spawned by the previous daemon instance), so `stop()` falls back to kill-by-PID (SIGTERM, wait up to 10s, then SIGKILL).
3. **Orphan cleanup** — separately, NVML enumerates all GPU compute processes. Any `llama-server` or `llama_server` process not matching the tracked PID is considered an orphan and killed (SIGTERM, wait 2s, SIGKILL if still alive).

This means the daemon survives restarts without losing control of its server process, and stale processes from crashed daemons are cleaned up automatically.

## Capacity Gate

Before starting a model, the daemon checks available VRAM:

1. Read the profile's model reference to get `estimated_vram_mb`
2. Query NVML for current free VRAM on GPU 0
3. If free VRAM < estimated requirement, reject the start with a clear error showing needed vs. available VRAM

This prevents the common failure mode of starting a model that won't fit in memory, which otherwise results in a slow OOM crash of llama-server.

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

For adopted processes (after daemon restart), there's no child handle, so stop falls back to kill-by-PID with the same SIGTERM/wait/SIGKILL sequence, polling `/proc/<pid>` for exit.

## SSE Architecture

The `/api/events` endpoint provides a single SSE stream that merges three independent sources:

1. **GPU stream** — `IntervalStream` polling NVML every 2 seconds, emits `event: gpu` with JSON stats
2. **State stream** — `BroadcastStream` from a tokio broadcast channel, emits `event: state` on start/stop/swap/fail transitions
3. **Log stream** — `BroadcastStream` from LogBuffer's broadcast channel, emits `event: log` for every captured stdout/stderr line

On connection, an initial `state` event is sent immediately so the client has current state without waiting for a transition. The three streams are merged with `futures_util::stream::select` (nested for three-way merge). A 15-second keep-alive prevents connection timeouts.

The dashboard and `rookery logs -f` both consume this same SSE endpoint, filtering for their relevant event types.

## Agent Management

Agents are external processes (coding agents like OpenCode, Hermes, etc.) managed alongside the inference server. Each agent has:

- **Config**: command, args, workdir, env vars, auto_start flag, restart_on_swap flag
- **Lifecycle**: spawn with stdout/stderr captured into LogBuffer (prefixed with `[agent:name]`), stop via SIGTERM/SIGKILL
- **Swap integration**: agents with `restart_on_swap = true` are automatically stopped and restarted after a model hot-swap completes

AgentManager tracks running agents in a `HashMap<String, ManagedAgent>` behind a `Mutex`. The `list()` method checks each child's exit status, cleaning up dead agents and reporting their final status.

## Graceful Shutdown

On SIGTERM or SIGINT:
1. axum's graceful shutdown drains in-flight HTTP requests
2. AgentManager stops all running agents (SIGTERM, 5s wait, SIGKILL)
3. ProcessManager stops the llama-server (SIGTERM, 10s wait, SIGKILL)
4. StatePersistence saves `Stopped` state

This ensures no orphan processes are left behind on daemon stop.

## Security

- Daemon binds to `127.0.0.1` only — not exposed to network
- llama-server binds to `0.0.0.0` for LAN access (configurable per profile)
- No authentication on daemon API (localhost only)
- Future: optional bearer token for remote access

## Future Phases

See ROADMAP.md for planned features and timeline.

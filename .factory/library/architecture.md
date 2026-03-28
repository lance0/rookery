# Architecture

How the system works — components, relationships, data flows, invariants.

## Overview

Rookery is a Rust daemon + CLI for managing local LLM inference on a single workstation. The vLLM integration adds Docker Compose–managed vLLM as a second backend alongside the existing llama-server process backend. Both backends implement a common trait, keeping the daemon and CLI backend-agnostic.

## Crate Layout

Four crates in a Cargo workspace:

```
rookery-core      — types, config, state, errors (no async, no I/O beyond file reads)
rookery-engine    — backend trait + implementations, GPU, agents, health, logs
rookery-daemon    — axum HTTP server, SSE, dashboard, orchestration
rookery-cli       — clap CLI, thin HTTP client to the daemon
```

### rookery-core

Owns the data model. No runtime behavior.

- **Config** — TOML deserialization of models, profiles, agents. A `Profile` has common fields (`model`, `port`) plus exactly one backend-specific sub-table: `[profiles.<name>.llama_server]` or `[profiles.<name>.vllm]`. The sub-table determines which backend to instantiate.
- **BackendType** — enum `{ LlamaServer, Vllm }`, derived from the profile's sub-table. Serialized into persisted state.
- **ServerState** — tagged enum state machine (`Stopped → Starting → Running → Stopping → Failed`). The `Running` variant carries `backend_type: BackendType` and an optional `container_id: String` (populated for vLLM).
- **Error** — unified error type used across all crates.

### rookery-engine

Runtime components. Each is independently testable.

- **InferenceBackend trait** — the abstraction boundary between daemon orchestration and backend specifics. Methods: `start`, `stop`, `is_running`, `process_info`, `adopt`, `to_server_state`, `is_draining`, `subscribe_errors`. Both backends implement this trait.
- **LlamaServerBackend** — wraps the existing `ProcessManager`. Spawns llama-server as a child process, captures stdout/stderr into `LogBuffer`, manages lifecycle via SIGTERM/SIGKILL. No behavior change from the current implementation.
- **VllmBackend** — manages a Docker Compose lifecycle. Generates a `compose.yml` from profile config, runs `docker compose up -d`, captures logs via `docker compose logs -f`, and stops via `docker compose down`. Tracks the container ID for state persistence.
- **GpuMonitor** — NVML polling for GPU stats and orphan process detection.
- **AgentManager** — spawns/stops external agent processes, handles persistence and crash recovery.
- **HealthChecker** — HTTP health checks (`GET /health`, `POST /v1/chat/completions` canary). Works identically for both backends since both expose the same OpenAI-compatible API.
- **LogBuffer** — ring buffer with broadcast channel for log streaming.

### rookery-daemon

Orchestration layer. Routes use the backend through `Box<dyn InferenceBackend>`.

- **AppState** — holds `Box<dyn InferenceBackend>` (instead of a bare `ProcessManager`), config, GPU monitor, agent manager, log buffer, broadcast channels, and the operation mutex.
- **Routes** — REST API handlers for start/stop/swap/status/bench/chat. Swap orchestration (drain → stop → start → health check) lives here, not in the trait — the daemon owns the multi-step workflow.
- **SSE** — single `/api/events` endpoint merging GPU stats, state changes, and log lines.
- **Dashboard** — Leptos WASM app built with trunk, embedded via `include_dir!`.

### rookery-cli

Stateless HTTP client. Talks to the daemon's REST API. Unaware of backend type — the daemon abstracts it.

## Component Relationships

```
┌─────────────┐         HTTP          ┌──────────────────────┐
│  rookery    │ ──────────────────►   │   rookery-daemon     │
│  (CLI)      │                       │                      │
└─────────────┘                       │  AppState            │
                                      │   ├ config (RwLock)  │
                                      │   ├ backend (trait)  │
                                      │   ├ agent_manager    │
                                      │   ├ gpu_monitor      │
                                      │   ├ log_buffer       │
                                      │   └ op_lock (Mutex)  │
                                      └──────────┬───────────┘
                                                  │
                              ┌────────────────────┼────────────────────┐
                              │                    │                    │
                     ┌────────▼────────┐  ┌───────▼────────┐  ┌───────▼───────┐
                     │ LlamaServer     │  │ VllmBackend    │  │ AgentManager  │
                     │ Backend         │  │                │  │               │
                     │                 │  │ docker compose │  │ child procs   │
                     │ child process   │  │ up/down/logs   │  │ SIGTERM/KILL  │
                     │ SIGTERM/SIGKILL │  │ compose.yml    │  │ persistence   │
                     └────────┬────────┘  └───────┬────────┘  └───────────────┘
                              │                    │
                     ┌────────▼────────┐  ┌───────▼────────┐
                     │ llama-server    │  │ vLLM container │
                     │ (native proc)  │  │ (Docker)       │
                     │ :8081          │  │ :8081          │
                     └────────────────┘  └────────────────┘
```

## Data Flows

### Starting a Profile

1. CLI sends `POST /api/start { profile }` to daemon.
2. Daemon acquires `op_lock`, reads config, resolves profile name.
3. Config determines `BackendType` from the profile's sub-table.
4. Idempotency check: if already running with same profile, return success no-op.
5. Capacity gate: query NVML for free VRAM, compare against model's `estimated_vram_mb`.
6. Persist `Starting` state, broadcast state change.
7. Instantiate the appropriate backend and call `backend.start()`:
   - **LlamaServer**: spawn child process, set OOM protection, capture logs.
   - **vLLM**: generate `compose.yml` from profile config, run `docker compose up -d`, start log capture via `docker compose logs -f`.
8. Health check: poll `GET /health` with exponential backoff (120s timeout).
9. Persist `Running` state (with `backend_type` and optional `container_id`), broadcast.

### Hot-Swap

Swap is a daemon-level operation, not part of the `InferenceBackend` trait:

1. Acquire `op_lock`.
2. Set drain flag → new chat requests get 503 for 5 seconds.
3. Call `backend.stop()` on the current backend.
4. Clear drain flag.
5. Determine new backend type from the target profile's config.
6. Instantiate new backend, call `backend.start()`.
7. Health check the new backend.
8. Persist new `Running` state, restart agents with `restart_on_swap`.

This allows swapping between backend types (e.g., llama-server → vLLM).

### Daemon Restart (Reconciliation)

1. Load persisted `state.json` — includes `backend_type` and optional `container_id`.
2. For `Running` state:
   - **LlamaServer**: verify PID alive via `/proc/<pid>/exe`, adopt into `LlamaServerBackend`.
   - **vLLM**: verify container running via `docker compose ps`, adopt container ID into `VllmBackend`.
3. Transient states (`Starting`, `Stopping`) reset to `Stopped`.
4. Orphan cleanup: NVML scan for untracked GPU processes.

## Invariants

- **One backend at a time.** Only one inference backend is active. The daemon holds a single `Box<dyn InferenceBackend>`.
- **Op lock serializes mutations.** Start, stop, and swap acquire `op_lock` to prevent races.
- **Config determines backend type.** The profile's sub-table (`llama_server` or `vllm`) is the single source of truth for which backend to use.
- **State persistence is atomic.** Write-to-tmp + rename prevents corruption.
- **Health checks are backend-agnostic.** Both backends expose the OpenAI-compatible API on the configured port. The same health check logic (`GET /health`, inference canary) works for both.
- **Drain is at the daemon level.** The 5-second drain window before swap applies regardless of backend type.
- **Dashboard and CLI are backend-unaware.** They consume the same REST API and SSE events. `BackendType` appears in status responses for display but doesn't change client behavior.

## Config Structure

```toml
[models.qwen]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"
estimated_vram_mb = 25800

# llama-server profile — backend-specific fields in [llama_server] sub-table
[profiles.fast]
model = "qwen"
port = 8081

[profiles.fast.llama_server]
ctx_size = 262144
threads = 4
flash_attention = true
cache_type_k = "q8_0"

# vLLM profile — backend-specific fields in [vllm] sub-table
[profiles.vllm-prod]
model = "qwen"
port = 8081

[profiles.vllm-prod.vllm]
tensor_parallel_size = 1
gpu_memory_utilization = 0.90
max_model_len = 32768
```

Common fields (`model`, `port`) live on the profile. Backend-specific tuning lives in the sub-table. A profile has exactly one sub-table — having both or neither is a config validation error.

## vLLM Backend Lifecycle

The `VllmBackend` manages vLLM through Docker Compose rather than spawning a process directly:

1. **Start**: Generate a `compose.yml` with GPU device mappings, port bindings, model paths, and vLLM-specific flags from the profile's `[vllm]` sub-table. Run `docker compose up -d`. Spawn a log-capture task via `docker compose logs -f`.
2. **Stop**: Run `docker compose down`. The container and its resources are cleaned up by Docker.
3. **Adopt**: On daemon restart, check `docker compose ps` for the persisted container ID. If running, resume log capture.
4. **Health**: Same HTTP health check as llama-server — vLLM exposes the OpenAI-compatible API on the same port.

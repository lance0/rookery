# Changelog

## 0.2.0 — 2026-03-20

Phases 2–5 complete. Agent management, hot-swap, dashboard, and polish.

### Added
- **Phase 2: Agent management** — `[agents.*]` config section, AgentManager engine, `/api/agents` endpoints, `rookery agent start|stop|status` CLI commands, `restart_on_swap` flag to auto-restart agents after model swap
- **Phase 3: Hot-swap + profiles** — `rookery swap <profile>` for zero-downtime model switching, `/api/swap` and `/api/profiles` endpoints, `rookery profiles` to list available profiles with model/context/VRAM info
- **Phase 4: Dashboard + SSE + logs** — embedded HTML dashboard at `http://127.0.0.1:3000/` with live GPU gauges, status card, profile switcher, agent controls, and log viewer; `/api/events` SSE stream merging GPU stats (2s interval), state changes, and log lines; `/api/logs?n=N` endpoint; `rookery logs` and `rookery logs -f` (follow mode via SSE); state change broadcasting via tokio broadcast channel
- **Phase 5: Polish**
  - `rookery bench` — quick benchmark hitting llama-server's `/v1/chat/completions` with short/medium prompts, reports PP and gen tok/s
  - Graceful daemon shutdown — SIGTERM/SIGINT handler stops all agents and llama-server, persists Stopped state
  - Shell completions — `rookery completions bash|zsh|fish` via clap_complete
  - Idempotent start — `rookery start` is a no-op if already running with the same profile, returns error with hint to use `swap` if a different profile is active
  - Capacity gate — checks free VRAM against model's `estimated_vram_mb` before starting, rejects with clear error if insufficient
  - Orphan process cleanup — on daemon startup, scans NVML GPU process list for untracked llama-server processes, SIGTERM then SIGKILL
  - Orphan process adoption — on daemon startup, reconciles persisted state and adopts the running llama-server PID so stop/swap work across daemon restarts
  - GPU process visibility — `GpuStats` includes per-GPU compute process list (PID, name, VRAM) from NVML

## 0.1.0 — 2026-03-20

Initial release. Phase 1 MVP.

### Added
- `rookeryd` daemon with axum REST API on `127.0.0.1:3000`
- `rookery` CLI with commands: `start`, `stop`, `status`, `gpu`, `config`
- TOML config with model/profile separation (`~/.config/rookery/config.toml`)
- State machine (Stopped/Starting/Running/Stopping/Failed) with JSON persistence
- ProcessManager: spawn/stop llama-server, PID tracking, stdout/stderr capture
- HealthChecker: exponential backoff polling of `/health` endpoint
- GpuMonitor: NVML-based GPU stats (VRAM, temp, utilization, power)
- LogBuffer: 10K line ring buffer with broadcast channel for streaming
- State reconciliation on daemon restart (verifies PID via `/proc/<pid>/exe`)
- `--json` flag on `status` and `gpu` for scripting
- `config` command: validates config, prints resolved command lines per profile
- Seed config with 3 profiles: qwen_fast (MoE 262K), qwen_thinking (MoE 131K), qwen_dense (27B 131K)

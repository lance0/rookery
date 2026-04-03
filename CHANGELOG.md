# Changelog

## 0.1.3 — 2026-04-03

Upstream release monitoring.

### Added
- **Upstream release monitor** — background task polls GitHub releases for `ggml-org/llama.cpp` and `vllm-project/vllm` every 30 minutes (configurable via `release_check_interval`, set to 0 to disable)
- **`/api/releases` endpoint** — returns cached release state with version comparison, update availability, and check timestamp
- **`rookery releases` CLI command** — shows current vs latest version with color-coded status; `--json` for scripting
- **Dashboard UpdateBanner** — Overview tab shows release status with "update available", "ahead of release", or "up to date" badges and links to release pages
- **ETag caching** — conditional requests avoid counting against GitHub's rate limit when nothing has changed
- **Version detection** — reads llama-server build info from `/props` (running) or `--version` (stopped)
- **Optional `github_token` config** — for higher API rate limits (5000/hr vs 60/hr unauthenticated)
- **Release cache persistence** — saved to `~/.local/state/rookery/releases.json`

## 0.4.0 — 2026-03-21

Phase 7: Production hardening.

### Added
- **OOM protection** — sets `oom_score_adj=-900` on llama-server after spawn, protecting the 20GB+ model from the OOM killer
- **systemd unit file** — `rookery.service` with journal output, `AmbientCapabilities=CAP_SYS_RESOURCE`, auto-restart on failure
- **Agent state persistence** — agent PIDs saved to `~/.local/state/rookery/agents.json`, reconciled and adopted on daemon restart (mirrors server state persistence pattern)
- **Agent auto-start** — agents with `auto_start = true` are started on daemon boot (config field existed but was never checked)
- **Swap drain** — 5s grace period before killing old server during hot-swap; new chat requests get 503 during drain

### Security
- **Config API redaction** — `GET /api/config` now replaces agent env vars with `"[N vars redacted]"` instead of exposing API keys and tokens

## 0.3.0 — 2026-03-21

Phases 5b + 6: Dashboard v2 and reliability sprint.

### Added
- **Dashboard v2** — replaced vanilla JS with Leptos WASM app: tabbed layout (Overview, Settings, Chat, Bench, Logs), streaming chat playground, live profile settings editor, model info panel, server stats, dark/light theme with localStorage, keyboard shortcuts (1-5 tabs, s/x start/stop, t theme), toast notifications
- **Dashboard API** — `GET /api/config`, `PUT /api/config/profile/:name`, `GET /api/model-info`, `GET /api/server-stats`, `POST /api/chat` (streaming SSE proxy)
- **SSE onopen handler** — dashboard reconnects automatically after daemon restart

### Fixed
- **Operation mutex** — `tokio::sync::Mutex<()>` serializes start/stop/swap, preventing concurrent state-changing operations from racing
- **Atomic saves** — config and state persistence use write-to-tmpfile + `rename()` to prevent corruption on crash
- **RwLock guard lifetime** — config read lock dropped before long `.await`s in start/swap handlers
- **LogBuffer poison recovery** — `unwrap_or_else(|e| e.into_inner())` instead of panicking on poisoned lock
- **Chat payload ordering** — message list built before empty assistant placeholder, preventing empty messages in API request
- **Stats polling accumulation** — single polling loop at App level instead of per-component (prevented unbounded request accumulation on tab switch)
- **Chat partial failure** — incomplete assistant messages marked with `[incomplete]` and filtered from subsequent API payloads
- **CSS variable** — `var(--text-muted)` replaced with `var(--muted)` (was undefined)

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

# Rookery Roadmap

## Phase 1: MVP (Done)
- [x] Cargo workspace with 4 crates (core, engine, daemon, cli)
- [x] Config parsing (TOML, models + profiles, command line resolution)
- [x] State machine (Stopped/Starting/Running/Stopping/Failed, persistence, reconciliation)
- [x] ProcessManager (spawn, stop, PID tracking, stdout/stderr capture)
- [x] HealthChecker (exponential backoff, 120s timeout)
- [x] GpuMonitor (NVML — VRAM, temp, utilization, power)
- [x] LogBuffer (10K line ring buffer, broadcast channel)
- [x] Daemon REST API (status, gpu, start, stop, health)
- [x] CLI (start, stop, status, gpu, config validate)
- [x] Seed config with 3 profiles (qwen_fast, qwen_thinking, qwen_dense)

## Phase 2: Hermes Agent Integration (Done)
- [x] Config: `[agents.hermes]` section — command, args, auto-start, restart_on_swap
- [x] Engine: AgentManager — spawn/stop agents, PID tracking, stdout/stderr capture
- [x] Daemon: `/api/agents`, `/api/agents/start`, `/api/agents/stop` endpoints
- [x] CLI: `rookery agent start hermes`, `rookery agent stop hermes`, `rookery agent status`
- [x] Agent restart_on_swap config flag (auto-restart agents after model swap)

## Phase 3: Hot-swap + Profiles (Done)
- [x] Hot-swap: `rookery swap <profile>` — stop current, start new, health check
- [x] `/api/swap`, `/api/profiles` endpoints
- [x] CLI: `rookery swap`, `rookery profiles`
- [x] Agents with restart_on_swap=true auto-restart after swap

## Phase 4: Web Dashboard + SSE + Logs (Done)
- [x] `/api/events` SSE stream (GPU stats every 2s, state changes, log lines)
- [x] `/api/logs?n=100` endpoint
- [x] Embedded HTML dashboard at `http://127.0.0.1:3000/` (vanilla JS, no framework)
- [x] Live GPU gauges, status card, profile switcher, agent controls, log viewer
- [x] State change broadcasting via tokio broadcast channel
- [x] CLI: `rookery logs` and `rookery logs -f` (follow mode via SSE)

## Phase 5: Polish (Done)
- [x] `rookery bench` — quick PP + gen speed benchmark via `/api/bench`
- [x] Graceful daemon shutdown (stop llama-server + agents on SIGTERM/SIGINT)
- [x] Shell completions (`rookery completions <shell>` via clap_complete)
- [x] Idempotent start (no-op if already running with same profile)
- [x] Capacity gate — check free VRAM before starting (uses model's estimated_vram_mb)
- [x] Orphan process cleanup (find stale llama-servers via NVML GPU process list, SIGTERM/SIGKILL)
- [x] Orphan process adoption (daemon restart discovers running server, adopts PID for stop/swap)
- [x] GPU process visibility (per-GPU compute process list with PID, name, VRAM in stats)

## Phase 6: Hardening
- [ ] systemd unit (auto-start on boot, restart on crash, journalctl logs)
- [ ] Agent state persistence (survive daemon restarts)
- [ ] Auto-sleep (unload model after idle timeout, reload on request)
- [ ] Better dashboard (SVG gauges, better CSS, eventually Leptos)

## Future
- Leptos WASM dashboard (replace vanilla HTML with proper Rust frontend)
- Multi-GPU support (data model ready, engine picks GPU 0 for now)
- Model downloads (`rookery models pull` to prefetch GGUF files)
- Reverse proxy drain (axum proxies to llama-server, 503 during swap)
- Custom agent framework (build/test agents against local models)
- `--json` flag on all remaining commands

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

## Phase 5b: Dashboard v2 (Done)
- [x] Leptos WASM dashboard (replaced vanilla JS)
- [x] Tabbed layout (Overview, Settings, Chat, Bench, Logs)
- [x] Settings panel — edit profile sampling params, save to config.toml
- [x] Model info panel — live model ID, context window, chat template from llama-server
- [x] Server stats panel — request count, processing status from /slots
- [x] Chat playground — streaming chat tab with SSE proxy
- [x] Dark/light theme toggle with localStorage persistence
- [x] Keyboard shortcuts (1-5 tabs, s/x start/stop, t theme)
- [x] Toast notifications (success/error on all actions)
- [x] Config API (GET /api/config, PUT /api/config/profile/:name)

## Phase 6: Reliability Sprint (Done)
### Backend — Concurrency & Atomicity
- [x] Operation mutex: add `tokio::sync::Mutex<()>` to serialize start/stop/swap (prevent concurrent state mutations)
- [x] Drop RwLock read guard before long `.await`s in post_start/post_swap (clone config data, release lock)
- [x] Atomic config save: write to tempfile + `rename()` instead of direct `std::fs::write()`
- [x] Atomic state persistence: same tempfile + rename pattern for state.json
- [x] LogBuffer: handle poison recovery (`unwrap_or_else(|e| e.into_inner())`)

### Backend — Watchdog & Health
- [ ] Inference canary: periodic minimal completion request (timeout 10s) to detect CUDA zombie state
- [ ] Restart backoff: exponential delay (1s→2s→4s...60s cap) on repeated crashes, reset on successful health
- [ ] Enable `--metrics` flag on llama-server, parse KV cache usage ratio + throughput
- [ ] Stderr pattern matching: detect `CUDA error` / `ggml_cuda_error` lines, trigger immediate canary
- [ ] Canary after orphan adoption: verify adopted process can actually serve inference
- [ ] Startup readiness gate: poll /health until 200 before reporting "running" (already exists, verify robustness)

### Backend — Chat Proxy Hardening
- [ ] Stream timeout: kill proxy connection if llama-server hangs mid-stream
- [ ] Request body size limit: cap message payload size
- [ ] SSE connection limit: bound max concurrent /api/events connections

### Frontend — Leak Fixes
- [x] Fix polling loop accumulation: move ServerStats polling to App level (single loop, passed as prop)
- [x] Fix chat payload: build message list before pushing empty assistant message
- [x] SSE onopen handler: set connected=true on successful reconnection
- [ ] Chat abort controller: allow canceling in-flight stream requests
- [x] Partial stream failure: mark incomplete assistant messages with " [incomplete]", filter from API payloads

### Frontend — UX Reliability
- [ ] Settings input validation: range checks before save, show errors for invalid values
- [ ] Loading/error states for initial data fetch (profiles, agents, logs)
- [ ] Bench panel: show error toast on failure instead of silent swallow
- [x] Fix CSS variable: --text-muted → --muted in header connection status

## Phase 7: Production Hardening
- [ ] systemd unit file (auto-start, restart on crash, journalctl)
- [ ] OOM killer protection: set oom_score_adj on llama-server process
- [ ] Proactive restart: schedule periodic restarts to counter performance degradation over time
- [ ] Agent state persistence (survive daemon restarts)
- [ ] Model swap drain: stop accepting new requests during swap, wait for in-flight to complete
- [ ] Redact sensitive fields from GET /api/config (agent env vars, paths)

## Future
- Multi-GPU support (data model ready, engine picks GPU 0 for now)
- Model downloads (`rookery models pull` to prefetch GGUF files)
- Reverse proxy drain (axum proxies to llama-server, 503 during swap)
- Custom agent framework (build/test agents against local models)
- `--json` flag on all remaining commands
- KV cache usage gauge in dashboard (requires --metrics)
- Auto-sleep (unload model after idle timeout, reload on request)

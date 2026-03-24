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
- [x] Inference canary: periodic minimal completion request (60s interval, 10s timeout, retry-once) with auto-restart
- [ ] Restart backoff: exponential delay (1s→2s→4s...60s cap) on repeated crashes, reset on successful health
- [ ] Enable `--metrics` flag on llama-server, parse KV cache usage ratio + throughput
- [x] Stderr pattern matching: detect `CUDA error` / `ggml_cuda_error` lines, trigger immediate canary
- [x] Canary after orphan adoption: verify adopted process can actually serve inference
- [ ] Startup readiness gate: poll /health until 200 before reporting "running" (already exists, verify robustness)

### Backend — Chat Proxy Hardening
- [x] Stream timeout: 60s per-chunk timeout on chat proxy SSE stream
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

## Phase 7: Production Hardening (Done)
- [x] Redact sensitive fields from GET /api/config (agent env vars replaced with count)
- [x] OOM killer protection: set oom_score_adj=-900 on llama-server after spawn
- [x] systemd unit file (journal output, AmbientCapabilities for OOM adj, auto-start on boot)
- [x] Model swap drain: 5s grace period before stop, 503 on new chat requests during drain
- [x] Agent state persistence: agents.json, reconcile+adopt on daemon restart, auto_start support
- [ ] Proactive restart: schedule periodic restarts (skipped — llama-server is stable)

## Phase 8: Model Discovery & Management
### CLI — `rookery models`
- [ ] `rookery models search <query>` — search HuggingFace for GGUF repos
- [ ] `rookery models quants <repo>` — list available quants (extract labels, group split shards, show sizes, highlight recommended)
- [ ] `rookery models recommend <repo>` — VRAM-aware auto-selection (best quant that fits in free VRAM)
- [ ] `rookery models list` — scan HF cache for already-downloaded GGUFs
- [ ] `rookery models pull <repo> [quant]` — download a specific quant (or auto-pick best fit)
- [ ] Quant preference ordering: UD variants first (UD-Q4_K_XL > UD-Q4_K_L > ...), then standard (Q4_K_M > Q5_K_M > ...)
- [ ] Auto-prefix shorthand: bare names without `/` get `unsloth/` prepended

### Dashboard — Model Browser
- [ ] Model search panel in Settings or new Models tab
- [ ] Quant selector with sizes, download status, VRAM fit indicator
- [ ] One-click download + add to config
- [ ] Show already-downloaded models from HF cache
- [ ] VRAM recommendation badge per quant

### Backend — Engine
- [ ] HuggingFace HTTP API client in rookery-engine (repo metadata, file listing, search)
- [ ] Quant label extraction from GGUF filenames (regex: Q4_K_M, IQ4_XS, UD-Q4_K_XL, etc.)
- [ ] Split shard grouping (multiple files per quant, sum sizes)
- [ ] HF cache scanner (`~/.cache/huggingface/hub/models--*` or llama.cpp cache)
- [ ] `/api/models/search`, `/api/models/quants/:repo`, `/api/models/recommend/:repo`, `/api/models/pull` endpoints

## Agent Reliability
- [x] Agent health check: `depends_on_port` config — watchdog detects server restart (down→up) and bounces dependent agents
- [x] Auto-restart on crash: `restart_on_crash = true` config flag with exponential backoff
- [x] Agent watchdog loop: background task polling agent liveness every 30s, kill+restart stuck agents
- [x] Restart agents on daemon restart: adopted agents with restart_on_swap bounced for fresh connections

## Future
- Multi-GPU support (data model ready, engine picks GPU 0 for now)
- Reverse proxy drain (axum proxies to llama-server, 503 during swap)
- Custom agent framework (build/test agents against local models)
- ~~`--json` flag on all remaining commands~~ (done)
- KV cache usage gauge in dashboard (requires --metrics)
- Auto-sleep (unload model after idle timeout, reload on request)

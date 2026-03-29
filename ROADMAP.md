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
- [x] Keyboard shortcuts (1-7 tabs, s/x start/stop, t theme)
- [x] Dedicated Agents tab with agent cards, filtered agent logs, watchdog state display
- [x] Compact agent summary pills on Overview tab
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
- [x] Restart backoff: exponential delay (1s→2s→4s...60s cap) on repeated crashes, reset after 5min healthy uptime
- [ ] Enable `--metrics` flag on llama-server, parse KV cache usage ratio + throughput
- [x] Stderr pattern matching: detect `CUDA error` / `ggml_cuda_error` lines, trigger immediate canary
- [x] Canary after orphan adoption: verify adopted process can actually serve inference
- [x] Startup readiness gate: `wait_for_health()` with exponential backoff, 120s timeout

### Backend — Chat Proxy Hardening
- [x] Stream timeout: 60s per-chunk timeout on chat proxy SSE stream
- [x] Request body size limit: 1MB via axum DefaultBodyLimit
- [x] SSE connection limit: max 16 concurrent /api/events connections (429 on overflow)

### Frontend — Leak Fixes
- [x] Fix polling loop accumulation: move ServerStats polling to App level (single loop, passed as prop)
- [x] Fix chat payload: build message list before pushing empty assistant message
- [x] SSE onopen handler: set connected=true on successful reconnection
- [x] Chat abort controller: allow canceling in-flight stream requests
- [x] Partial stream failure: mark incomplete assistant messages with " [incomplete]", filter from API payloads

### Frontend — UX Reliability
- [x] Settings input validation: range checks on sampling params, error toasts for invalid values
- [x] Loading/error states for initial data fetch (profiles, agents, logs)
- [x] Bench panel: show error toast on failure
- [x] Fix CSS variable: --text-muted → --muted in header connection status

### Frontend — Mobile Responsiveness
- [ ] Responsive layout for tabbed UI (Overview, Settings, Chat, Bench, Logs)
- [ ] Touch-friendly controls for GPU gauges, profile switcher, agent controls
- [ ] Mobile-optimized log viewer and chat playground
- [ ] Hamburger menu for smaller screens
- [ ] Stack GPU gauges vertically on mobile
- [ ] Ensure keyboard shortcuts don't conflict with mobile gestures

## Phase 7: Production Hardening (Done)
- [x] Redact sensitive fields from GET /api/config (agent env vars replaced with count)
- [x] OOM killer protection: set oom_score_adj=-900 on llama-server after spawn
- [x] systemd unit file (journal output, AmbientCapabilities for OOM adj, auto-start on boot)
- [x] Model swap drain: 5s grace period before stop, 503 on new chat requests during drain
- [x] Agent state persistence: agents.json, reconcile+adopt on daemon restart, auto_start support
- [ ] Proactive restart: schedule periodic restarts (skipped — llama-server is stable)

## Phase 8: Model Discovery & Management (Done)
### CLI — `rookery models`
- [x] `rookery models search <query>` — search HuggingFace for GGUF repos
- [x] `rookery models quants <repo>` — list available quants (extract labels, group split shards, show sizes, highlight recommended)
- [x] `rookery models recommend <repo>` — VRAM-aware auto-selection (best quant that fits in free VRAM)
- [x] `rookery models list` — scan HF cache for already-downloaded GGUFs
- [x] `rookery models pull <repo> [quant]` — download a specific quant (or auto-pick best fit)
- [x] Quant preference ordering: UD variants first (UD-Q4_K_XL > UD-Q4_K_L > ...), then standard (Q4_K_M > Q5_K_M > ...)
- [x] Auto-prefix shorthand: bare names without `/` get `unsloth/` prepended
- [x] `rookery models hardware` — show GPU/CPU/RAM hardware profile

### Dashboard — Model Browser
- [x] Models tab with search, quants, recommendations
- [x] Quant selector with sizes, download status, VRAM fit indicator
- [x] One-click download + add to config
- [x] Show already-downloaded models from HF cache
- [x] VRAM recommendation badge per quant

### Backend — Engine
- [x] HuggingFace HTTP API client in rookery-engine (repo metadata, file listing, search)
- [x] Quant label extraction from GGUF filenames (regex: Q4_K_M, IQ4_XS, UD-Q4_K_XL, etc.)
- [x] Split shard grouping (multiple files per quant, sum sizes)
- [x] HF cache scanner (llama.cpp cache at `~/.cache/llama.cpp/`)
- [x] `/api/models/search`, `/api/models/quants`, `/api/models/recommend`, `/api/models/cached`, `/api/models/pull` endpoints
- [x] Hardware profiling: GPU (NVML), CPU, RAM with bandwidth lookup + performance estimation

## Agent Reliability
- [x] Agent health check: `depends_on_port` config — watchdog detects server restart (down→up) and bounces dependent agents
- [x] Auto-restart on crash: `restart_on_crash = true` config flag with exponential backoff
- [x] Agent watchdog loop: background task polling agent liveness every 30s, kill+restart stuck agents
- [x] Restart agents on daemon restart: adopted agents with restart_on_swap bounced for fresh connections

## Agent Observability
- [x] Agent health endpoint: `/api/agents/{name}/health` — uptime, restart count/reason, error count, version
- [x] Agent metrics in dashboard: uptime, restart count, error count with color indicators
- [x] Agent stderr error counting: atomic counter shared with stderr capture task
- [x] Restart reason tracking: "crash", "swap", "port_recovery", "daemon_restart"
- [x] Enriched `/api/agents` list: includes health metrics for all running agents (no N+1 calls needed)
- [x] Enriched `/api/agents/{name}/health`: watchdog state, backoff, dependency port status, last restart timestamp
- [x] Error count reset on restart: `error_count` resets per session, `lifetime_errors` accumulates
- [ ] Agent chat timeout config: kill hung requests after configurable timeout
- [x] Agent restart on error patterns: `restart_on_error_patterns` config, watch channel triggers immediate restart via watchdog select!

## Phase 9: Hermes Management Plane (Kubernetes-Style Control)
### Vision
Rookery as the control plane for Hermes: Hermes manages itself (self-update, self-heal), Rookery ensures desired state matches actual state.

### Desired State Declaration
- [ ] Config: `desired_version` field (e.g., "1.3.0" or "latest")
- [ ] Config: `desired_state` field ("running" | "stopped")
- [ ] Config: `restart_policy` ("on-failure" | "on-config-change" | "never")

### Reconciliation Loop (Enhanced Watchdog)
- [ ] Version drift detection: compare actual vs desired version, trigger update if mismatch
- [ ] State reconciliation: ensure running/stopped matches desired state
- [ ] Config sync: detect external config changes, reload if needed
- [ ] Error pattern analysis: classify crashes (OOM, timeout, connection) vs normal restarts

### CLI — Update Coordination
- [x] `rookery agent update hermes` — run update_command, stop/restart, report version diff
- [ ] `rookery agent update hermes --to <version>` — version-targeted update (deferred)
- [ ] `rookery agent version hermes` — show desired vs actual version, update available
- [x] `rookery agent describe hermes` — full status (PID, uptime, version, restart count, health, errors)

### Observability — "kubectl describe" for Hermes
- [ ] `/api/agents/{name}/describe` — detailed agent status endpoint
- [ ] Track: restart count with reasons, last update timestamp, uptime history
- [ ] Health depth: not just "alive" but "telegram connected", "port listening", "no recent errors"
- [ ] Error aggregation: count errors by type in last N hours

### Dashboard — Hermes Control Panel
- [ ] Hermes status card: version, uptime, health indicators (telegram, port, errors)
- [ ] One-click update button with version selector
- [ ] Restart history timeline (when, why)
- [ ] Quick actions: "flush state", "reconnect telegram", "view errors"

## Code Quality & Testing
### Linting
- [x] `cargo fmt` — enforced across workspace
- [x] `cargo clippy` — zero warnings (27 fixed)
- [x] CI check: `cargo fmt --check && cargo clippy -- -D warnings` in GitHub Actions
- [x] Pre-commit hook: `.githooks/pre-commit` runs fmt + clippy (configured via `core.hooksPath`)

### Test Coverage (337 tests total)
- [x] `rookery-core`: config parsing, state serialization, reconciliation, validation, backend type serde (29 tests)
- [x] `rookery-engine`: MockLlamaServer test infrastructure (shared mock HTTP server)
- [x] `rookery-engine`: health checks — wait_for_health, check_health, check_inference (14 tests)
- [x] `rookery-engine`: ProcessManager lifecycle — start, stop, adopt, is_running, log capture (18 tests)
- [x] `rookery-engine`: AgentManager — start/stop/health/remove_tracking/record_restart/fatal patterns (13 tests)
- [x] `rookery-engine`: Backend interaction — LlamaServer/Vllm trait implementations (7 tests)
- [x] `rookery-engine`: compose generation, model utils, log buffer, hardware (38 tests)
- [x] `rookery-daemon`: canary extraction + behavior tests with mock backend (11 tests)
- [x] `rookery-daemon`: route integration tests — status, gpu, start, stop, swap, bench, agents, chat, dashboard (27 tests)
- [x] `rookery-daemon`: SSE — initial state, connection limit, event format (7 tests)
- [x] `rookery-daemon`: config save isolation (tests write to temp dir, not production config)
- [x] `rookery-cli`: argument parsing, output formatting for all subcommands (15 tests)
- [x] Edge cases: logs, models, config, state, GPU, compose (23 tests)
- [ ] End-to-end: daemon startup → start → swap → agent lifecycle (manual only)
- [ ] vLLM integration tests (gated behind `ROOKERY_INTEGRATION=1`, needs Docker + free GPU)

### CI Pipeline
- [x] GitHub Actions workflow: build + test + clippy + fmt on push/PR
- [x] Cache cargo registry + build artifacts via `rust-cache` action
- [ ] Test matrix: stable + nightly Rust
- [ ] Dashboard CI job: trunk build + wasm32 validation in GitHub Actions

## vLLM Backend Support (Done — Core)

### Architecture (Done)
- [x] Backend trait: `InferenceBackend` trait in `backend.rs` (start, stop, health, swap, subscribe_errors)
- [x] `LlamaServerBackend`: wraps ProcessManager, zero behavior change
- [x] `VllmBackend`: Docker-based backend (docker compose up/down, log capture, CUDA detection)
- [x] Config: profile sub-tables `[profiles.name.llama_server]` / `[profiles.name.vllm]`
- [x] Backend selection at startup based on profile config
- [x] Daemon holds `Box<dyn InferenceBackend>` — all routes polymorphic

### vLLM-Specific (Done)
- [x] Profile params: `docker_image`, `gpu_memory_utilization`, `max_num_seqs`, `max_num_batched_tokens`, `quantization`, `tool_call_parser`, `kv_cache_dtype`, `extra_args`
- [x] Docker compose template generation (`compose.rs`) with NVIDIA GPU reservation
- [x] Health check: same `/health` endpoint, works for both backends
- [x] Swap: docker compose down + up with drain period
- [x] CUDA 13.0 inside container — doesn't conflict with host CUDA 12.8
- [x] Log capture: docker compose logs -f piped into LogBuffer with `[vllm]` prefix
- [x] CUDA error detection in docker logs
- [x] Compose --model validation (repo field required for vLLM profiles)
- [x] Canary re-subscribes to backend error channel after swap (stale receiver fix)
- [x] Capacity gate bypass for vLLM (manages own memory)
- [x] Integration tests gated behind `ROOKERY_INTEGRATION=1`

### Dashboard — Multi-Backend (Done)
- [x] Status card: backend badge ("llama.cpp" / "vLLM")
- [x] Profile switcher: backend type indicator per profile
- [x] ServerStats: shows "N/A" for vLLM (no /slots endpoint)
- [ ] Backend-specific stats: vLLM batch utilization display

### CLI (Done)
- [x] `rookery status` shows backend type
- [x] `rookery profiles` shows `[llama-server]` / `[vllm]` prefix
- [x] `rookery bench` works against any OpenAI-compatible endpoint
- [ ] `rookery start --backend vllm` CLI override (uses profile config currently)

### Quantization Profiles & Testing (Not Yet Tested)
- [ ] NVFP4 profile: Qwen3.5-27B-NVFP4 at ~80 tok/s gen, 229K context
- [ ] TurboQuant KV profile: AWQ-4bit weights + turboquant35 KV cache at 262K context
- [ ] Profile comparison: `rookery bench --profile qwen_dense --profile qwen_nvfp4` side-by-side

### A/B Testing & Blog
- [ ] Dual-port mode: llama.cpp on 8081 + vLLM on 8000, swap hermes between them
- [ ] Tool calling quality benchmark: BFCL-V4 on Q6_K (llama.cpp) vs NVFP4 (vLLM)
- [ ] Blog post: "Tool Calling Quality Across Quantization Formats on RTX 5090"
- [ ] Hermes real-world comparison: same conversations on both backends, measure response quality

## Observability & Metrics (Inspired by llama-swap, GPUStack)
- [x] Prometheus metrics endpoint (`/metrics`) — GPU, server, canary, agent, chat, SSE metrics
- [ ] Grafana dashboard template (JSON import) — GPU gauges, request throughput, error rates
- [ ] OpenTelemetry trace export for inference requests
- [ ] KV cache usage gauge in dashboard (requires `--metrics` flag on llama-server)

## Quality of Life (Inspired by Competitive Research)
- [x] Model aliasing: `aliases` field on profiles, resolved in CLI and API (e.g., "fast" → qwen_fast)
- [x] CLI auto-detects daemon address from config (no hardcoded port)
- [x] Auto-sleep: `idle_timeout` config, `Sleeping` state, wake-on-request, manual `sleep`/`wake` CLI + API
- [x] API key auth: optional bearer token for dashboard and API access
- [x] Auto-start default profile on daemon boot (`auto_start` config flag)
- [x] Multi-cache model scanner (HF hub + llama.cpp + custom `model_dirs`)
- [ ] Swagger/OpenAPI spec generation for the REST API

## Production Deployment
- [ ] Production systemd setup: install binaries to `/usr/local/bin` (not `target/release/`), separate build and deploy steps
- [ ] Version-tagged releases: `cargo build --release` → `cp` to `/usr/local/bin/` on explicit deploy, not on every build
- [ ] Systemd unit points to `/usr/local/bin/rookeryd` (stable path, survives `cargo clean`)
- [x] Deploy script or `make install` target for clean build → install → restart cycle
- [x] `install.sh` curl-to-shell installer for GitHub releases
- [x] GitHub release workflow: multi-arch builds, checksums, shell completions

## Open Source Launch Prep
- [x] README polish: feature list, comparison table, real-world use cases, install methods, full reference
- [x] LICENSE file (dual MIT/Apache-2.0)
- [x] CONTRIBUTING.md (dev setup, code style, project structure, PR guidelines)
- [ ] CONTRIBUTING.md (build instructions, PR guidelines, code style)
- [x] Review all docs for accuracy and completeness
- [x] Remove any hardcoded paths or lancebox-specific references from code
- [ ] Publish to crates.io (optional — evaluate if workspace structure allows it)

## Future
- Multi-GPU support (data model ready, engine picks GPU 0 for now)
- Reverse proxy drain (axum proxies to llama-server, 503 during swap)
- Custom agent framework (build/test agents against local models)
- ~~`--json` flag on all remaining commands~~ (done)
- Request rewriting / filtering (proxy layer for API requests)
- Multi-model concurrent serving (multiple profiles on different ports simultaneously)
- Multi-GPU support (data model ready, engine picks GPU 0 for now)
- Reverse proxy drain (axum proxies to llama-server, 503 during swap)
- Custom agent framework (build/test agents against local models)
- ~~`--json` flag on all remaining commands~~ (done)
- Request rewriting / filtering (proxy layer for API requests)
- Multi-model concurrent serving (multiple profiles on different ports simultaneously)

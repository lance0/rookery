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

## Phase 2: Hermes Agent Integration
- [ ] Config: `[agents.hermes]` section — install path, model assignment, auto-start
- [ ] Engine: AgentManager — spawn/stop Hermes Agent, point at running llama-server
- [ ] Daemon: `/api/agents` endpoint — list, start, stop, status
- [ ] CLI: `rookery agent start hermes`, `rookery agent stop`, `rookery agent status`
- [ ] Agent restarts on model swap (picks up new model automatically)

## Phase 3: Profiles + Hot-swap
- [ ] CapacityGate — check free VRAM vs model estimate before starting
- [ ] Swapping state in state machine (drain → stop → start → health check)
- [ ] `/api/swap`, `/api/profiles`, `/api/models` endpoints
- [ ] CLI: `rookery swap`, `rookery profiles`, `rookery models`, `rookery profile show`

## Phase 4: Leptos Dashboard + SSE
- [ ] `/api/events` SSE stream (GPU stats every 2s, state changes, log lines)
- [ ] `/api/logs?n=100` endpoint
- [ ] Leptos WASM dashboard (status card, GPU gauges, profile switcher, log viewer)
- [ ] SSE-driven live updates
- [ ] CLI: `rookery logs [-f]` (follow mode via SSE)

## Phase 5: Bench + Polish
- [ ] `rookery bench` — quick PP + gen speed benchmark
- [ ] Graceful daemon shutdown (stop llama-server on SIGTERM)
- [ ] Shell completions (clap_complete)
- [ ] Idempotent start (no-op if already running with same profile)
- [ ] `--json` flag on all commands

## Future
- Multi-GPU support (data model ready, engine picks GPU 0 for now)
- Model downloads (`rookery models pull` to prefetch GGUF files)
- Reverse proxy drain (axum proxies to llama-server, 503 during swap)
- systemd unit (auto-start on boot, restart on crash)
- Custom agent framework (build/test agents against local models)

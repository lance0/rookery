# Changelog

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

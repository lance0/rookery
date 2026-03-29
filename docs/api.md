# API Reference

All endpoints are served by the rookeryd daemon.

## Server Management

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/health` | GET | Daemon health check |
| `/api/status` | GET | Server state, profile, PID, uptime |
| `/api/start` | POST | Start server `{"profile": "name"}` |
| `/api/stop` | POST | Stop server |
| `/api/sleep` | POST | Put the running server into `sleeping` state |
| `/api/wake` | POST | Wake the sleeping server using its last profile |
| `/api/swap` | POST | Hot-swap profile `{"profile": "name"}` |
| `/api/profiles` | GET | List available profiles |
| `/api/bench` | GET | Run PP + gen speed benchmark |

## GPU & Hardware

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/gpu` | GET | GPU stats (VRAM, temp, utilization, power, processes) |
| `/api/hardware` | GET | Hardware profile (GPU, CPU, RAM with bandwidth) |

## Agent Management

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/agents` | GET | List agents with health metrics |
| `/api/agents/start` | POST | Start agent `{"name": "hermes"}` |
| `/api/agents/stop` | POST | Stop agent `{"name": "hermes"}` |
| `/api/agents/{name}/health` | GET | Detailed agent health |

## Model Discovery

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/models/search?q=query` | GET | Search HuggingFace for GGUF repos |
| `/api/models/quants?repo=name` | GET | List available quants for a repo |
| `/api/models/recommend?repo=name` | GET | VRAM-aware quant recommendation |
| `/api/models/cached` | GET | List locally cached models |
| `/api/models/pull` | POST | Download a model `{"repo": "...", "quant": "..."}` |

## Configuration

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/config` | GET | Full config (agent env vars redacted) |
| `/api/config/profile/{name}` | PUT | Update profile sampling params |
| `/api/model-info` | GET | Model ID, context window from llama-server |
| `/api/server-stats` | GET | Slot status, request count |

## Streaming

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/events` | GET | SSE stream (gpu stats, state changes, log lines) |
| `/api/chat` | POST | Streaming chat proxy to llama-server (auto-wakes sleeping backends, 60s per-chunk timeout) |
| `/api/logs?n=50` | GET | Fetch last N log lines |
| `/metrics` | GET | Prometheus/OpenMetrics text exposition |

`/api/status` may return `state: "sleeping"` with the last active `profile` and no PID/port. `POST /api/wake` or the next `/api/chat` request transitions that profile back to `running`.

## Metrics

`GET /metrics` returns Prometheus-compatible text generated from live daemon state plus in-process runtime counters.

Metric families:

| Metric | Labels | Notes |
|----------|--------|-------------|
| `rookery_gpu_vram_used_bytes` | `gpu` | NVML scrape-time gauge |
| `rookery_gpu_vram_total_bytes` | `gpu` | NVML scrape-time gauge |
| `rookery_gpu_temperature_celsius` | `gpu` | NVML scrape-time gauge |
| `rookery_gpu_utilization_percent` | `gpu` | NVML scrape-time gauge |
| `rookery_gpu_power_watts` | `gpu` | NVML scrape-time gauge |
| `rookery_server_up` | `profile`, `backend` | `1` when backend is running, else `0` |
| `rookery_server_uptime_seconds` | `profile` | Present only while running |
| `rookery_server_restarts_total` | none | Runtime counter, resets on daemon restart |
| `rookery_canary_checks_total` | none | Incremented on each canary run |
| `rookery_canary_failures_total` | none | Incremented when a failed check enters retry flow |
| `rookery_canary_restarts_total` | none | Incremented when canary initiates a restart |
| `rookery_canary_last_check_timestamp` | none | Unix timestamp of the last canary run |
| `rookery_agent_up` | `name` | `1` when agent is running, else `0` |
| `rookery_agent_uptime_seconds` | `name` | Present while an agent is running |
| `rookery_agent_restarts_total` | `name` | Agent restart counter from agent manager state |
| `rookery_agent_errors_total` | `name` | Current tracked error count |
| `rookery_agent_lifetime_errors_total` | `name` | Lifetime tracked error count |
| `rookery_chat_requests_total` | none | Chat proxy requests accepted for forwarding |
| `rookery_chat_errors_total` | none | Chat proxy setup or upstream errors |
| `rookery_chat_stream_timeouts_total` | none | Per-chunk 60s stream timeouts |
| `rookery_sse_connections_current` | none | Current active SSE clients |
| `rookery_sse_connections_total` | none | Lifetime SSE connections since daemon start |

Notes:

- GPU metrics are refreshed on each scrape; there is no background polling task.
- Server and agent gauges are derived from current `AppState` and engine health data.
- Canary, chat, and SSE counters are daemon-runtime metrics and reset when `rookeryd` restarts.

## Dashboard

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Embedded Leptos WASM dashboard |

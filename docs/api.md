# API Reference

All endpoints are served by the rookeryd daemon.

## Server Management

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/health` | GET | Daemon health check |
| `/api/status` | GET | Server state, profile, PID, uptime |
| `/api/start` | POST | Start server `{"profile": "name"}` |
| `/api/stop` | POST | Stop server |
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
| `/api/chat` | POST | Streaming chat proxy to llama-server (60s per-chunk timeout) |
| `/api/logs?n=50` | GET | Fetch last N log lines |

## Dashboard

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Embedded Leptos WASM dashboard |

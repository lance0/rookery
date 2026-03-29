# Dashboard

The Rookery dashboard is a Leptos WASM application embedded in the daemon binary. Access it at your configured `listen` address.

## Tabs

| Tab | Key | Description |
|-----|-----|-------------|
| **Overview** | `1` | GPU gauges, server status with backend badge, model info, server stats, agent panel with health metrics |
| **Settings** | `2` | Profile switcher with backend indicators, sampling param editor, agent start/stop |
| **Chat** | `3` | Streaming chat playground (SSE proxy to inference server) |
| **Bench** | `4` | PP + gen speed benchmark with error toasts |
| **Logs** | `5` | Live log viewer with auto-scroll |
| **Models** | `6` | HuggingFace model search, quant browser, VRAM-aware recommendations, download |

## Keyboard Shortcuts

- `1`-`6` — switch tabs
- `s` — start server
- `x` — stop server
- `t` — toggle dark/light theme

## Features

- **Backend badge** — shows "llama.cpp" or "vLLM" on the status card
- **Sleep / Wake controls** — status card exposes manual sleep and wake without leaving the dashboard
- **Sleeping state** — status card shows `sleeping` with the remembered profile and no stale PID/port
- **Agent panel** — green/gray dot, version, uptime, restart count (yellow), error count (red)
- **GPU gauges** — live VRAM, temperature, utilization, power from NVML
- **Mobile responsive** — tabs scroll horizontally, cards stack vertically on small screens
- **SSE auto-reconnect** — dashboard reconnects automatically if the daemon restarts
- **Settings validation** — range checks on sampling params with error toasts

## Building

```bash
cd crates/rookery-dashboard
trunk build --release

# Re-embed into daemon binary
cd ../..
touch crates/rookery-daemon/src/routes.rs
cargo build --release -p rookery-daemon
```

The dashboard is embedded via `include_dir!` at compile time. After rebuilding the WASM, you must touch a daemon source file and rebuild to re-embed the new assets.

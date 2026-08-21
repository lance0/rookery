# Dashboard

The Rookery dashboard is a Leptos WASM application embedded in the daemon binary. Access it at your configured `listen` address.

When `api_key` is enabled, the SPA shell still loads normally, then prompts for the key after the first `401` from the daemon. The key is stored in browser `localStorage`, attached to API requests as `Authorization: Bearer ...`, and attached to the SSE stream as `?token=...`.

## Tabs

| Tab | Key | Description |
|-----|-----|-------------|
| **Overview** | `1` | GPU gauges, server status with backend badge, model info, server stats, compact agent summary, upstream release banner |
| **Settings** | `2` | Profile switcher with backend indicators and sampling param editor |
| **Agents** | `3` | Agent cards, controls, filtered logs, watchdog and health detail |
| **Chat** | `4` | Streaming chat playground (SSE proxy to inference server) |
| **Bench** | `5` | PP + gen speed benchmark; failures persist on the card with reasons |
| **Logs** | `6` | Live log viewer with auto-scroll |
| **Models** | `7` | HuggingFace model search, quant browser, VRAM-aware recommendations, download |

## Keyboard Shortcuts

- `1`-`7` — switch tabs
- `s` — start server
- `x` — stop server
- `t` — toggle dark/light theme

Shortcuts are ignored while a text input is focused, and ignored when combined with
`Ctrl`/`Cmd`/`Alt` — so `Ctrl+S` reaches the browser instead of starting the server.
Held keys do not repeat.

## Features

- **Backend badge** — shows "llama.cpp" or "vLLM" on the status card
- **Sleep / Wake controls** — status card exposes manual sleep and wake without leaving the dashboard
- **Sleeping state** — status card shows `sleeping` with the remembered profile and no stale PID/port
- **Agent panel** — green/gray dot, version, uptime, restart count (yellow), error count (red)
- **Agent updates** — each agent row exposes an Update button backed by the daemon API
- **GPU gauges** — live VRAM, temperature, utilization, power from NVML
- **Mobile responsive** — tabs scroll horizontally, cards stack vertically on small screens
- **Connection status chip** — a persistent indicator driven by a four-state machine, so
  the page always says whether what you are reading is current:

  | State | Chip reads | Meaning |
  |-------|-----------|---------|
  | `Live` | `live` | An event arrived within 3s. Numbers are current. |
  | `Stale` | `stale` | Socket open but silent for 3–10s. Numbers are dimmed. |
  | `Reconnecting` | `Disconnected — reconnecting…` | Socket closed, or open but silent past 10s. A retry is scheduled. Dot pulses. |
  | `Disconnected` | `disconnected` | Stopped retrying — reached only on a confirmed `401`. |

  **A silently-stale stream no longer reads as live data.** A socket that stays open
  while the daemon has stopped sending is invisible to `EventSource` — no error, no
  close event — so a 1s watchdog ages the last event rather than trusting the socket.
  The daemon sends a named `ping` event every 2s to keep that clock fed.

  Freshness drives the whole page, not just the chip: the app root carries `data-stale`
  (stat values, badges, gauges and process rows drop to 45% opacity) or `data-dead`
  (30% opacity **plus** greyscale, so colour-coded status stops reading as meaningful).
  The dot's pulse is disabled under `prefers-reduced-motion`.

- **SSE reconnect with jittered backoff** — the dashboard drives reconnection itself
  rather than relying on the browser's native retry, which recovers from transport
  drops but gives up permanently on a non-2xx handshake. Backoff doubles 1s → 2s → 4s
  → 8s → 16s → 30s (capped) with ±25% jitter. The jitter matters more than the curve:
  the daemon returns 429 past `MAX_SSE_CONNECTIONS` (16), so every tab it rejects is by
  definition in lockstep and would otherwise retry in unison forever. **A 429 now
  recovers on its own** — no page reload. Only a `401` stops the loop, and that raises
  the unlock prompt rather than failing silently.
- **API key prompt** — when auth is enabled, unauthorized API responses trigger an in-app unlock prompt
- **Settings validation** — range checks on sampling params with error toasts
- **Swapping state** — a model swap takes 30s+, so the daemon broadcasts it rather than
  leaving the card on its old value. The status card shows an amber `swapping` badge
  with the **target** profile name and `—` for PID, port and uptime. Every profile card
  is disabled for the duration, as is **Stop**; the click handler also refuses
  re-entry, so a double-click cannot queue a second swap before the state event lands.
- **Ticking uptime** — uptime counts up once per second from a monotonic
  `performance.now()` anchor, re-anchored whenever a status event arrives, instead of
  freezing at whatever the last event said. `performance.now()` rather than wall-clock
  deliberately: NTP steps, DST and resume-from-sleep do not distort it. Renders
  `2h 14m 7s`, or `—` when the server is not running.
- **Tab state survives switching** — all seven panels stay mounted and are shown or
  hidden with `display`. Chat history, bench results and the HuggingFace model search
  are no longer wiped by visiting another tab. The Agents tab's per-agent health polling
  is gated on that tab being visible, so keeping panels alive does not cost background
  requests.
- **"Unavailable" is never rendered as zero** — a measured `0` and a failed
  measurement are different readings and now look different:
  - **Server stats** — when the backend does not expose `/slots`, Status reads
    `N/A — stats unavailable` (or `N/A — vLLM does not expose /slots` on a vLLM
    profile) and the remaining fields read `N/A`, rather than showing zero requests
    served. When the server is stopped the card reads `server not running`.
  - **GPU panel** — when NVML is present but the query fails, the panel replaces the
    gauges with **`GPU monitor unavailable`** plus the NVML error verbatim. A machine
    with genuinely no GPU still renders normally, and a real zero is still shown as
    zero. (This covers the Overview gauges; the Models tab's free-VRAM figure does not
    yet distinguish the two.)
- **Bench failures persist on the card** — a failed run no longer falls back to
  `no results yet`. A total failure shows `bench failed — no tests completed` in red
  followed by `name: reason` per failure; a partial failure shows the results table
  plus `N of M tests failed` and the same list. A request that never reached the daemon
  is reported the same way rather than looking like a bench that was never run.
- **Contrast and legibility** — `--muted` and `--dim` meet WCAG AA against the worse of
  the two backdrops in both themes. This matters more than it sounds: those tokens
  colour every `.empty`, `.loading` and stat label in the app, and dark-mode `--muted`
  had been sitting at 1.70:1, hiding error text. Numbers use `tabular-nums` so live
  values stop jittering as digits change, and log lines use `content-visibility` so a
  long scrollback stays responsive while remaining findable by find-in-page.
- **Accessibility** — chat and both log viewers auto-scroll only when you are already
  within 50px of the bottom, so scrolling back to read is not yanked away. Agent cards
  expose a real `details →` button rather than a click-only div. Toasts announce via
  `role="status"` (polite, so a screen reader is not cut off mid-sentence) and errors
  linger 15s against a success's 3s. Chat streaming announces state only —
  `Response started.` / `Response complete.` / `Response failed.` / `Response stopped.` —
  rather than queueing one announcement per streamed token.

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

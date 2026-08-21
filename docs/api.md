# API Reference

All endpoints are served by the rookeryd daemon.

## Authentication

When `api_key` is configured in `config.toml`, all API endpoints require `Authorization: Bearer <key>` except `/api/health` and `/metrics`.

- Dashboard fetches use the bearer header automatically after the user enters the key.
- `GET /api/events` accepts `?token=<key>` for browser `EventSource` connections.
- Dashboard HTML/assets remain publicly servable so the SPA can show the auth prompt.

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

### Start, wake and swap status codes

- `POST /api/swap` with an unknown profile returns **404** and a JSON body naming
  the profile and the valid names: `{"error": "no such profile: typo", "profiles": [...]}`.
  The name is validated **before any teardown**, so a typo no longer drains and
  stops the running backend on its way to failing.
- `POST /api/start`, `POST /api/wake` and `POST /api/swap` return **503** once
  daemon shutdown has begun, rather than spawning a backend the daemon will not
  live to supervise. `/api/swap` carries `{"error": "daemon is shutting down"}`
  and leaves the state machine on `stopped`, never stuck on `swapping`.

### `GET /api/bench`

Runs three prompts (`short`, `medium`, `long`) against the live backend and
returns both what measured and what did not:

```json
{
  "tests":  [{"name": "short", "prompt_tokens": 0, "completion_tokens": 0,
              "pp_tok_s": 0.0, "gen_tok_s": 0.0}],
  "errors": [{"name": "long", "error": "HTTP 404 Not Found: ..."}]
}
```

`errors` has one entry per prompt that produced no measurement — a transport
failure, a non-2xx upstream, a malformed body, or a response with no `timings`
block (which is what a vLLM backend gives). The response is **200 even when
every prompt failed**: partial results are real and must render, and a non-2xx
would lose the reason. An empty `tests` with a populated `errors` is a bench
that ran and failed; empty *both* is a bench that never ran. Only a
non-`running` backend short-circuits this, with **503**.

## GPU & Hardware

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/gpu` | GET | GPU stats (VRAM, temp, utilization, power, processes) |
| `/api/hardware` | GET | Hardware profile (GPU, CPU, RAM with bandwidth) |

`/api/hardware` adds two live fields to the static profile: `gpu.vram_free_mb`
and `cpu.ram_free_mb`. **`vram_free_mb` is `null` when the NVML query failed** —
distinct from `0`, which means a genuinely full GPU. Clients must not render an
unread value as zero.

## Agent Management

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/agents` | GET | List agents with health metrics |
| `/api/agents/start` | POST | Start agent `{"name": "hermes"}` |
| `/api/agents/stop` | POST | Stop agent `{"name": "hermes"}` |
| `/api/agents/{name}/update` | POST | Stop, update, and restart an agent |
| `/api/agents/{name}/health` | GET | Detailed agent health |

## Model Discovery

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/models/search?q=query` | GET | Search HuggingFace for GGUF repos |
| `/api/models/quants?repo=name` | GET | List available quants for a repo |
| `/api/models/recommend?repo=name` | GET | VRAM-aware quant recommendation |
| `/api/models/cached` | GET | List locally cached models |
| `/api/models/pull` | POST | Download a model `{"repo": "...", "quant": "..."}` |

Anything that sizes a quant against VRAM carries **`vram_known: bool`**, the same
NVML distinction as `/api/hardware`:

- `/api/models/quants` — `{"repo": ..., "quants": [...], "vram_known": false}`.
  The per-quant fit estimates are still attached when VRAM is unknown, computed
  against `0`; `vram_known: false` marks them as guesses, not measurements.
- `/api/models/recommend` — same flag. When nothing fits, `recommendation` is
  `null` and `message` says which case it is.
- `/api/models/pull` with no `quant` picks one by recommendation. If none fits it
  returns 200 with `{"started": false, "vram_known": ..., "message": ...}`.

That `message` distinguishes the two causes rather than blaming the GPU:

| `vram_known` | `message` |
|----------|--------|
| `true` | `no quant fits in available memory` |
| `false` | `could not read GPU VRAM (NVML query failed)` |

## Configuration

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/config` | GET | Full config, with `api_key`, `github_token` and agent env vars redacted |
| `/api/config/profile/{name}` | PUT | Update profile sampling params |
| `/api/reload` | POST | Re-read the config file from disk — see below |
| `/api/model-info` | GET | Model ID, context window from llama-server |
| `/api/server-stats` | GET | Slot status, request count |

### `POST /api/reload`

Re-reads the config file into the running daemon. Nothing is restarted: the
live backend keeps its profile, port, PID and binary, and no agent is started,
stopped or bounced. CLI equivalent: `rookery reload`.

A reload changes what *future* operations see:

| | |
|---|---|
| **Applied immediately** | `api_key` (checked per request), `idle_timeout` (re-read each 30s poll), `default_profile` |
| **Applied on the next start/swap** | `profiles`, `models`, the `llama_server` binary path |
| **Needs a daemon restart** | `listen` (socket already bound), `agents` (the watchdog holds the definitions it booted with), `auto_start`, `release_check_interval` |

The response body repeats those three lists, plus `warnings` for anything this
particular reload could not honour — a changed `listen`, edited `[agents]`, a
port change on the live profile, or the live profile having been deleted from
the file (which is allowed and stops nothing).

Responses:

- `200` — applied. Keys: `success`, `message`, `path`, `profiles`, and the three
  lists above as `applied_now`, `unchanged` and `needs_daemon_restart`, plus
  `warnings`. (`profiles` and `models` are reported inside `applied_now`,
  annotated as taking effect on the next start or swap.)
- `400` — the file is missing, unparseable, or fails the same validation the
  daemon applies at boot. **The old config is kept and the daemon keeps
  serving**; the error names the problem. A typo can never take the daemon
  down, unlike at boot where an invalid config is a hard exit.
- `409` — a start/stop/swap held `op_lock` for longer than 5s. Nothing changed;
  retry once the operation finishes.

## Upstream Releases

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/releases` | GET | Cached release status for llama.cpp: latest version, current version, update availability |

Returns cached data from periodic GitHub polling (interval configurable via `release_check_interval`). Includes `update_available` and `ahead_of_release` flags, current binary version, latest release tag/URL, and `checked_at` timestamp.

## Streaming

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/events` | GET | SSE stream (gpu stats, state changes, log lines; use `?token=` when auth is enabled) |
| `/api/chat` | POST | Streaming chat proxy to llama-server (auto-wakes sleeping backends, 60s per-chunk timeout) |
| `/api/logs?n=50` | GET | Fetch last N log lines |
| `/metrics` | GET | Prometheus/OpenMetrics text exposition |

`/api/status` may return `state: "sleeping"` with the last active `profile` and no PID/port. `POST /api/wake` or the next `/api/chat` request transitions that profile back to `running`.

`/api/status` may also return `state: "swapping"` with `profile` set to the
**target** profile and `pid`, `port`, `uptime_secs` and `backend` all `null` —
nothing is serving yet, and reporting the target's port would be an optimistic
lie. A swap takes 30s+, and this state is broadcast before the drain begins, so
clients should treat `swapping` as intentional downtime rather than showing the
old profile as still running.

`/api/chat` returns **502** when the upstream backend answers with a non-2xx,
instead of laundering the failure into a 200 SSE stream that carries no content.
This is always 502 and never the upstream's own status: an upstream 400 can be
proxy-side fault (the `model` name is resolved by the daemon, not the caller), so
forwarding it would blame the caller. A transport-level failure is also 502; a
stopped, draining or unwakeable backend is 503.

### `GET /api/events`

Event types on the stream: `state` (one sent immediately on connect, then on
every start/stop/swap), `gpu` (every 2s), `log` (per line), and `ping`.

**Two different keep-alives, and they are not interchangeable:**

| | Sent | Visible to JavaScript | Purpose |
|----------|--------|-------------|-------------|
| `event: ping` | every 2s | yes, via `addEventListener("ping", …)` | feeds the client's staleness clock |
| `: ping` comment | after 15s idle | **no** | stops intermediaries timing the socket out |

The heartbeat is a *named* event, so `EventSource.onmessage` — which fires only
for the default `message` type — never sees it and can never mistake it for
data. Its payload is the server clock in epoch milliseconds, present only
because a browser never dispatches an event with an empty data buffer. It must
stay comfortably under the dashboard's 3s freshness threshold. The `: ping`
comment cannot serve this role: browsers do not surface comments to JavaScript
at all, so it can keep a proxy from closing the connection but cannot tell a
client anything.

The `gpu` payload distinguishes a broken GPU query from a machine that has none:

| payload | meaning |
|----------|--------|
| `{"gpus": [...]}` | normal reading |
| `{"gpus": [], "error": "<nvml error>"}` | **NVML query failed** — stats are unknown |
| `{"gpus": []}`, no `error` | no GPU present, or NVML found no devices |

Both failure and absence used to collapse into a bare `{"gpus": []}`, which
arrives perfectly on schedule and so is invisible to a staleness watchdog by
construction. Treat the `error` field as the only signal of a degraded GPU
read — an empty `gpus` alone is not one, and marking it as such would make
every GPU-less host cry wolf permanently.

The daemon accepts at most 16 concurrent SSE connections; beyond that
`/api/events` returns **429** and the connection is not counted.

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
| `rookery_agent_db_corrupt_total` | `name` | Agent SQLite databases found corrupt by `PRAGMA quick_check` |
| `rookery_agent_db_unchecked_total` | `name` | Agent SQLite databases the integrity check could not read — not a clean bill of health |
| `rookery_agent_db_last_check_timestamp` | `name` | Unix timestamp of the last integrity sweep for that agent |
| `rookery_chat_requests_total` | none | Chat proxy requests accepted for forwarding |
| `rookery_chat_errors_total` | none | Chat proxy setup or upstream errors |
| `rookery_chat_stream_timeouts_total` | none | Per-chunk 60s stream timeouts |
| `rookery_sse_connections_current` | none | Current active SSE clients |
| `rookery_sse_connections_total` | none | Lifetime SSE connections since daemon start |

Notes:

- GPU metrics are refreshed on each scrape; there is no background polling task.
- Server and agent gauges are derived from current `AppState` and engine health data.
- Canary, chat, and SSE counters are daemon-runtime metrics and reset when `rookeryd` restarts.
- `rookery_server_up` reports **0 while swapping or sleeping, labelled with the
  profile** (the swap *target*, with `backend=""`). Every other down state —
  stopped, starting, stopping, failed — reports 0 with `profile=""`. So
  `rookery_server_up{profile!=""} == 0` is intentional downtime and
  `rookery_server_up{profile=""} == 0` is not, which is what lets an alert rule
  stay quiet through a 30s+ model swap without going blind to a crash.
- The three `rookery_agent_db_*` families are only emitted for agents whose
  database has actually been swept. Staleness in
  `rookery_agent_db_last_check_timestamp` means the check stopped running, which
  otherwise looks identical to healthy — alert on its age, not just on
  `rookery_agent_db_corrupt_total`.

## Dashboard

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Embedded Leptos WASM dashboard shell |

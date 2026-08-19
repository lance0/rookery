# Changelog

> Version numbering note: releases ran 0.1.0 → 0.4.0 in March 2026, then renumbered to
> the 0.1.x line when the crates moved to a shared workspace version. Entries below 0.1.3
> predate that change and are left as originally published.

## 0.1.8 — 2026-08-19

Dependency maintenance, a documentation sweep, and one config field that was
silently doing nothing.

### Fixed
- **`gpu_index` now actually selects a GPU.** It was parsed, serialized and
  documented as the multi-GPU knob, but emitted no flag at all — a multi-GPU user
  who set it silently got GPU 0. Now emits `--main-gpu`. Covered by a test in both
  directions, since the failure mode was silence.
- **Migrated to rand 0.10.** `Fill::fill` was renamed to `Fill::fill_slice`, so
  `rand::Fill::fill(...)` no longer resolved (`E0782`). Now uses the free function
  `rand::fill`, which still draws from a CSPRNG — this generates API keys.

### Changed
- **Dashboard clippy runs with `-D warnings`.** It was advisory when the job was
  added in 0.1.7, which is a gate that gets ignored.
- 15 dependency updates, including tokio 1.51→1.53, axum 0.8.8→0.8.9,
  tower-http 0.6.8→0.7.0, prometheus-client 0.24.1→0.25.0, clap 4.6.0→4.6.6.

### Documentation
Several claims were checked against the code and found wrong:
- **The release monitor polls llama.cpp only.** README (×2), `docs/api.md`,
  `docs/cli.md`, `docs/configuration.md` and the CLI `--help` all claimed vLLM was
  tracked too, and the rate-limit budget was documented as "2 requests per
  interval (one per tracked repo)" when it is 1. vLLM is deliberately not polled:
  the comparison parses llama.cpp's `bNNNNN` build numbers, and vLLM's `vX.Y.Z`
  tags would fail that parse and report "up to date" forever.
- **State does not live in `~/.config/rookery/`.** `docs/architecture.md` said it
  did; it is `dirs::state_dir()`, i.e. `~/.local/state/rookery/`.
- **The default KV cache type is `q8_0`, not `f16`.** The table in
  `docs/configuration.md` marked the wrong row as default, so anyone omitting
  `cache_type_*` expecting f16 quality was silently getting q8_0.
- **SSE auto-reconnect is narrower than advertised.** The browser retries
  transport-level drops, so a daemon restart recovers — but a non-2xx handshake
  (429 past 16 connections, or 401) kills the stream permanently.
- **`/api/config` redacts `api_key` and `github_token` as well as agent env vars.**
- **`stop_timeout_secs` is documented**, along with a warning about being
  conservative with `restart_on_error_patterns`.
- **CONTRIBUTING now tells you to enable the git hooks.** It claimed fmt and
  clippy were "enforced by pre-commit hooks", but `core.hooksPath` is local config
  that cannot be committed, so a fresh clone had no hooks at all.

## 0.1.7 — 2026-08-19

Durability and lifecycle correctness in the daemon, plus the first dashboard
release in four months. Findings came from a systematic review of the workspace.

### Security
- **`Ctrl+S` no longer starts the inference server.** The dashboard's keydown
  handler read `e.key()` with no modifier or repeat check, and `e.key()` for
  `Ctrl+S` is plain `"s"` — so the browser's save-page shortcut fired
  `POST /api/start`, and `Ctrl+X` fired stop. Holding a key also repeated the
  action at key-repeat rate.

### Fixed
- **State writes are now durable.** `write` + `rename` is atomic against a process
  crash but not against power loss: the rename can reach disk before the temp
  file's contents do, leaving a zero-length file. All four persistence sites
  (server state, agent state, release cache, config) now go through
  `write_atomic` — write, fsync, rename, fsync parent directory.
- **A failed `state.json` load no longer silently kills a healthy server.** The
  error was discarded, yielding `Stopped`, which set `tracked_pid` to `None`,
  which made the orphan reaper treat every running llama-server as an orphan and
  SIGTERM then SIGKILL it — mid-request, with nothing in the log explaining why.
- **Agent stop grace is configurable** via `stop_timeout_secs`, default raised
  5s → 30s. Five seconds is thin for an agent checkpointing a large SQLite WAL,
  and hard-killing mid-checkpoint is how torn pages happen. Taking the SIGKILL
  path is now logged at error, naming the agent and the risk.
- **Automated restarts no longer erase crash backoff.** `stop()` unconditionally
  cleared the crash counter, and the fatal-error restart, dependency-port bounce
  and profile swap all called it — wiping the exponential backoff the crash path
  had just built. Split into `stop()` and `stop_automated()`.
- **The fatal-error restart path has real backoff.** It previously slept a flat 2s
  with no counter, so an agent matching a pattern on every startup restarted every
  ~2.3s indefinitely. systemd's `StartLimit` does not cover this, because rookeryd
  itself never exits.
- **Agent adoption verifies identity.** `adopt()` stored the persisted PID
  unchecked, and everything downstream SIGKILLs that bare number. The upstream
  filter only tested that `/proc/<pid>` exists — true for a zombie and true for a
  recycled PID. Adoption now requires the PID to be alive and
  `/proc/<pid>/cmdline` to still reference the configured command.
- **Zombie-aware liveness in the stop path**, which previously waited out the full
  grace period before SIGKILLing an already-dead process.
- **Start button after a failed start.** `can_start` compared the whole status
  string to `"failed"`, but the daemon reports `"failed: <error>"` — so Start was
  disabled in exactly the state where it is needed.
- **Unmatched `/api/*` paths return 404 with a JSON body** instead of 200 with the
  dashboard HTML, which made a typo'd route look like success to any caller
  checking `response.ok`. The `index.html` unwrap in the SPA fallback is gone too.

### Changed
- **Dashboard assets are cacheable.** Content-hashed assets are served
  `immutable`; `index.html` gets a content ETag with 304 handling. Beyond
  bandwidth, V8 caches *compiled* wasm keyed on URL, and a fresh `200`
  invalidates that cache — so every dashboard reload was recompiling ~900 KB.
- **Dashboard wasm is 39% smaller** — 904 KB → 556 KB, via a release profile
  (`opt-level="z"`, fat LTO, one codegen unit, `panic="abort"`, strip) that the
  crate never had, since being excluded from the workspace left it on cargo
  defaults.
- **The dashboard is built in CI and in the release pipeline.** It was excluded
  from the workspace, so fmt/clippy/test/MSRV/audit and `cargo build --release`
  all skipped it — meaning `dist/` (a committed artifact embedded via
  `include_dir!`) went four months stale, and dashboard fixes existed in source
  but in no released binary. `make install` now depends on `make dashboard`.

### Known issues
- `cargo clippy` on the dashboard still reports 8 warnings (redundant rebindings
  and one upstream future-incompat note). The CI job builds and lints it but does
  not yet gate on `-D warnings`.

## 0.1.6 — 2026-08-19

Correctness pass over the daemon, plus deployment and log-correlation fixes. Findings came
from a systematic review of the whole workspace; three independent reviews converged on the
drain-flag leak, which is the most severe item here.

### Security
- **`github_token` is now redacted by `GET /api/config`** — it was returned in cleartext.
  The handler redacted `api_key` and agent env vars but not this one, and the endpoint is
  unauthenticated unless `api_key` is set.
- **`api_key` no longer reports `"[redacted]"` when no key is configured.** It was
  unconditionally overwritten, so the one endpoint an operator would use to check whether a
  daemon is authenticated answered "yes" when the answer was no.
- **Bumped `rustls-webpki` 0.103.10 → 0.103.14 and `quinn-proto` 0.11.14 → 0.11.17**,
  clearing RUSTSEC-2026-0104 (reachable panic in CRL parsing), -0098 and -0099 (name-constraint
  bypasses), and -0185. Lockfile-only; the repo's own `cargo audit` CI gate was failing.

### Fixed
- **Drain flag leaked on two CUDA-error canary exit paths**, leaving `POST /api/chat`
  returning 503 for every request indefinitely and disabling all subsequent canary checks.
  The flag is an `AtomicBool` that survives `stop()` and `start()`, so nothing short of a
  swap or a daemon restart cleared it. Reachable whenever llama-server emits a CUDA line
  while the server is not in `Running` — teardown during a normal `rookery stop`, for one.
- **Watchdog restarted an agent twice per fatal-error burst**, SIGTERMing the replacement
  microseconds after spawning it. The notification is consumed before a ~2.2s
  stop/sleep/start, so any send arriving in that window — one traceback matches the patterns
  on several lines, and lines drain off the dying process's pipe too — re-armed `changed()`
  and fired again immediately. Observed 32 times in production journals since April, on an
  agent that opens a 385 MB SQLite database at startup.
- **`PUT /api/config/profile/{name}` was a silent no-op for any profile using a
  `[profiles.X.llama_server]` sub-table** — i.e. every non-legacy profile. It wrote the
  legacy flat fields, which `resolve_llama_server_command_line` ignores whenever the
  sub-table exists, and still returned `success: true`. Now writes through to the sub-table,
  normalizing legacy profiles onto it, and returns 409 for vLLM profiles instead of
  reporting success. 404 now carries a body explaining that config is read at daemon start.
- **`Config::save_to` now keeps a `.toml.bak`** before rewriting. It serializes from the
  struct, so it discards all comments and key ordering in a hand-maintained config; the
  previous contents are at least recoverable now.
- **`detect_llama_version` parsed stdout, but llama-server writes its build banner to
  stderr** — which was piped to `/dev/null`. The function always failed, so with the server
  stopped `rookery releases` reported "up to date" regardless of the installed build. Now
  scans both streams.
- **`sleep_server` leaked `dependency_bounce_suppressed`** when `stop()` failed, silently
  disabling dependency-port agent bouncing until the next start/stop/swap.
- **Log in local time** — journald prefixes each line in local time while tracing's default
  timer emitted UTC, so every `rookeryd` line carried two clocks four hours apart, and across
  midnight the *dates* disagreed. Adds a `LocalTimer` `FormatTime` impl backed by `chrono::Local`.
  chrono deliberately, not `tracing_subscriber::fmt::time::LocalTime`: the latter is backed by
  the `time` crate, which refuses to read the local UTC offset from a multithreaded process, and
  `#[tokio::main]` has already started the runtime by the time the subscriber initializes.
- **`make install` no longer writes over a live binary in place** — installs to a `.new`
  temp and `mv -f`s it into place. `mv` within a filesystem is an atomic `rename(2)`, so the
  path is never absent or partial and a running daemon keeps its old inode. The previous
  in-place `install` would fail with `ETXTBSY` against a running daemon.

### Changed
- **`rookery.service` caps restart thrashing** — `StartLimitIntervalSec=300` /
  `StartLimitBurst=5`. Previously inherited systemd's defaults (10s/5). Every restart cycle
  churns managed agents, so a unit that stops loudly beats one that loops.

## 0.1.5 — 2026-08-11

Dependency maintenance.

### Changed
- Bumped the `rust-deps` group (6 updates)
- Migrated to the rand 0.9 API — `rand::Fill` replaces `RngCore`

## 0.1.4 — 2026-08-04

CUDA crash-loop recovery. A CUDA fault used to leave the daemon restarting a backend into the
same broken GPU state; this release makes it drain, cool down, and verify identity first.

### Fixed
- **CUDA crash loop** — drain on error and enforce a GPU cooldown before restart
- **Canary draining** — distinguish a genuine CUDA error from an in-progress swap drain, and
  skip the canary inference check entirely while server slots are busy
- **CUDA error races** — PID verification before acting on an error signal, and an atomic
  drain check; error signals raised during an active swap or drain are ignored

### Changed
- CUDA error channel is now daemon-scoped, with structured backend identity checks

## 0.1.3 — 2026-04-03

Upstream release monitoring.

### Added
- **Upstream release monitor** — background task polls GitHub releases for `ggml-org/llama.cpp` and `vllm-project/vllm` every 30 minutes (configurable via `release_check_interval`, set to 0 to disable)
- **`/api/releases` endpoint** — returns cached release state with version comparison, update availability, and check timestamp
- **`rookery releases` CLI command** — shows current vs latest version with color-coded status; `--json` for scripting
- **Dashboard UpdateBanner** — Overview tab shows release status with "update available", "ahead of release", or "up to date" badges and links to release pages
- **ETag caching** — conditional requests avoid counting against GitHub's rate limit when nothing has changed
- **Version detection** — reads llama-server build info from `/props` (running) or `--version` (stopped)
- **Optional `github_token` config** — for higher API rate limits (5000/hr vs 60/hr unauthenticated)
- **Release cache persistence** — saved to `~/.local/state/rookery/releases.json`

## 0.4.0 — 2026-03-21

Phase 7: Production hardening.

### Added
- **OOM protection** — sets `oom_score_adj=-900` on llama-server after spawn, protecting the 20GB+ model from the OOM killer
- **systemd unit file** — `rookery.service` with journal output, `AmbientCapabilities=CAP_SYS_RESOURCE`, auto-restart on failure
- **Agent state persistence** — agent PIDs saved to `~/.local/state/rookery/agents.json`, reconciled and adopted on daemon restart (mirrors server state persistence pattern)
- **Agent auto-start** — agents with `auto_start = true` are started on daemon boot (config field existed but was never checked)
- **Swap drain** — 5s grace period before killing old server during hot-swap; new chat requests get 503 during drain

### Security
- **Config API redaction** — `GET /api/config` now replaces agent env vars with `"[N vars redacted]"` instead of exposing API keys and tokens

## 0.3.0 — 2026-03-21

Phases 5b + 6: Dashboard v2 and reliability sprint.

### Added
- **Dashboard v2** — replaced vanilla JS with Leptos WASM app: tabbed layout (Overview, Settings, Chat, Bench, Logs), streaming chat playground, live profile settings editor, model info panel, server stats, dark/light theme with localStorage, keyboard shortcuts (1-5 tabs, s/x start/stop, t theme), toast notifications
- **Dashboard API** — `GET /api/config`, `PUT /api/config/profile/:name`, `GET /api/model-info`, `GET /api/server-stats`, `POST /api/chat` (streaming SSE proxy)
- **SSE onopen handler** — dashboard reconnects automatically after daemon restart

### Fixed
- **Operation mutex** — `tokio::sync::Mutex<()>` serializes start/stop/swap, preventing concurrent state-changing operations from racing
- **Atomic saves** — config and state persistence use write-to-tmpfile + `rename()` to prevent corruption on crash
- **RwLock guard lifetime** — config read lock dropped before long `.await`s in start/swap handlers
- **LogBuffer poison recovery** — `unwrap_or_else(|e| e.into_inner())` instead of panicking on poisoned lock
- **Chat payload ordering** — message list built before empty assistant placeholder, preventing empty messages in API request
- **Stats polling accumulation** — single polling loop at App level instead of per-component (prevented unbounded request accumulation on tab switch)
- **Chat partial failure** — incomplete assistant messages marked with `[incomplete]` and filtered from subsequent API payloads
- **CSS variable** — `var(--text-muted)` replaced with `var(--muted)` (was undefined)

## 0.2.0 — 2026-03-20

Phases 2–5 complete. Agent management, hot-swap, dashboard, and polish.

### Added
- **Phase 2: Agent management** — `[agents.*]` config section, AgentManager engine, `/api/agents` endpoints, `rookery agent start|stop|status` CLI commands, `restart_on_swap` flag to auto-restart agents after model swap
- **Phase 3: Hot-swap + profiles** — `rookery swap <profile>` for zero-downtime model switching, `/api/swap` and `/api/profiles` endpoints, `rookery profiles` to list available profiles with model/context/VRAM info
- **Phase 4: Dashboard + SSE + logs** — embedded HTML dashboard at `http://127.0.0.1:3000/` with live GPU gauges, status card, profile switcher, agent controls, and log viewer; `/api/events` SSE stream merging GPU stats (2s interval), state changes, and log lines; `/api/logs?n=N` endpoint; `rookery logs` and `rookery logs -f` (follow mode via SSE); state change broadcasting via tokio broadcast channel
- **Phase 5: Polish**
  - `rookery bench` — quick benchmark hitting llama-server's `/v1/chat/completions` with short/medium prompts, reports PP and gen tok/s
  - Graceful daemon shutdown — SIGTERM/SIGINT handler stops all agents and llama-server, persists Stopped state
  - Shell completions — `rookery completions bash|zsh|fish` via clap_complete
  - Idempotent start — `rookery start` is a no-op if already running with the same profile, returns error with hint to use `swap` if a different profile is active
  - Capacity gate — checks free VRAM against model's `estimated_vram_mb` before starting, rejects with clear error if insufficient
  - Orphan process cleanup — on daemon startup, scans NVML GPU process list for untracked llama-server processes, SIGTERM then SIGKILL
  - Orphan process adoption — on daemon startup, reconciles persisted state and adopts the running llama-server PID so stop/swap work across daemon restarts
  - GPU process visibility — `GpuStats` includes per-GPU compute process list (PID, name, VRAM) from NVML

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

# Changelog

> Version numbering note: releases ran 0.1.0 → 0.4.0 in March 2026, then renumbered to
> the 0.1.x line when the crates moved to a shared workspace version. Entries below 0.1.3
> predate that change and are left as originally published.





## 0.1.14

### Fixed

- **`make install` no longer generates a systemd unit that runs the daemon as
  root.** `install` needs root to write `/usr/local/bin` and
  `/etc/systemd/system`, so it is normally run under `sudo` — where `whoami` is
  `root` and `$(HOME)` is `/root`. Both were substituted into the unit, so the
  daemon started as root, failed to find config at
  `/root/.config/rookery/config.toml`, and crash-looped until systemd gave up
  with "start request repeated too quickly". `SERVICE_USER` now prefers
  `SUDO_USER`, and `HF_HOME` resolves against the invoking user's home. A
  non-sudo install still falls back to `whoami`.
- **`make install` now prints the `User` and `HF_HOME` it baked into the unit.**
  Both were substituted silently, and a wrong `HF_HOME` is not merely cosmetic:
  container backends bind-mount it, so pointing it at root's home hides every
  model on the machine. Both are overridable:
  `sudo make install SERVICE_USER=you HF_HOME=/path/to/models`.
- **The releases view shows only the running backend's upstream.** The cache is
  keyed by repo and entries persist, so serving it whole left a row for whatever
  engine was running at each past check. A backend not yet polled returns an
  explicit not-yet-checked row rather than the previous engine's numbers.

### Notes

- `make install` has always advised `systemctl daemon-reload` afterwards; it now
  says the unit changed and the reload is required. Skipping it leaves systemd
  running a cached unit that has silently diverged from the installed one.

## 0.1.13

### Fixed

- **`hf_cache` is now optional and resolved from the environment.** It
  previously carried a hardcoded absolute path as its default, so SGLang would
  bind-mount a directory that does not exist on most machines. Unset, it now
  resolves to `$HF_HOME`, else `~/.cache/huggingface`, and reports a clear error
  if neither is available.
- **The committed dashboard bundle no longer embeds absolute build paths.**
  `dist/` is checked in because rookeryd embeds it via `include_dir!`, and rustc
  bakes source paths into dependency panic messages. The `dashboard` target now
  passes `--remap-path-prefix` for the cargo registry, the rustup toolchain and
  the workspace root. Release binaries were unaffected — CI rebuilds `dist/` on
  its own runners.
- Example configuration and docs use placeholder paths and hostnames.

## 0.1.12

SGLang becomes a first-class backend, and the release checker stops
recommending downgrades.

### Added

- **SGLang backend.** A third implementation behind the existing
  `InferenceBackend` trait, selected by a `[profiles.<name>.sglang]` sub-table.
  `rookery swap` crosses engines on the same port: a local llama-server process
  stops and a container starts, or the reverse, with clients seeing one endpoint
  throughout. Driven by plain `docker run` rather than compose, because the whole
  invocation is described by the config and a compose file would be a second
  source of truth. Liveness is `docker inspect`, identity is the container ID,
  and adoption verifies the running container against the recorded one with
  prefix matching for short-vs-full IDs.
- **SGLang telemetry in the dashboard.** SGLang exposes no `/slots`, but its
  Prometheus scrape carries strictly more than llama-server's slot payload, so
  the stats card renders KV used against total, KV usage, accept length and rate,
  prefix cache hit rate, request counts, and the GDN state pool. That last one
  has its own tile deliberately: on a 32GB card serving a hybrid GatedDeltaNet
  model it is the resource that runs out first, and `/slots` cannot show it.
- **Backend-aware release tracking.** The monitor polls the running backend's
  upstream — `ggml-org/llama.cpp`, `sgl-project/sglang`, or `vllm-project/vllm` —
  falling back to the default profile's backend when nothing is running.
  Previously it always polled llama.cpp, so a box serving SGLang was told about
  builds it does not run.

### Fixed

- **`rookery releases` no longer advertises a stale stable tag as "latest".**
  It read `/releases/latest`, which returns only the newest NON-prerelease.
  ggml-org publishes semver tags as stable *and* `bNNNNN` build tags as
  prereleases, so every build tag was invisible. On 2026-09-03 this reported
  `v0.3.0` (Aug 25) as latest to a box running a build 154 commits ahead of it —
  and taking that "upgrade" would have lost a feature the newer build was
  installed for. Now reads `/releases` and takes the newest non-draft entry;
  drafts are the only kind worth skipping, since prereleases are the point.
- **Nightly container builds are compared by date, not semver.** SGLang
  nightlies report `0.0.0.dev1+g<hash>.d<YYYYMMDD>`, where the semver carries no
  information. The build date is compared against the release's publish date, so
  a nightly newer than the latest release reads as *ahead* rather than prompting
  a downgrade. When neither a usable semver nor a date is available the status
  stays **unknown** — never "up to date".
- **`rookery profiles` shows a context length for container backends.** It read
  `ctx_size` from `llama_server_config()`, which is `None` for them, so the
  SGLang row rendered with no context and read like a broken profile.
- **The "no slot data" explanation names the backend.** Previously a boolean
  that only knew about vLLM, so SGLang fell through to llama-server's generic
  "stats unavailable", which implies a fault rather than an endpoint that does
  not exist.

### Notes

- A profile declaring two backend sub-tables is rejected rather than resolved by
  precedence. Starting the wrong engine on the same port is the kind of thing you
  notice an hour later via a confusing throughput number.
- `rookery-dashboard` is **not** a workspace member, so `cargo build/clippy/test
  --workspace` does not cover it. It needs `trunk build` and its own `cargo
  clippy`/`cargo test`. Three clippy errors in it were missed this way during
  development.

## 0.1.11 — 2026-08-21

Finishes what 0.1.10 started. 0.1.10 stopped the *lie* — reporting "up to date"
when the versions could not be compared. Deploying it revealed that the honest
answer was then **"unknown" forever**, which for a tool whose whole job is to
tell you when to upgrade is barely an improvement.

### Fixed

- **`rookery releases` now resolves llama.cpp again instead of reporting
  "unknown" permanently.** When a server is running, the daemon reads its
  version from `/props`, which exposes only `build_info: "b10566-bb4caa754"` —
  a build number and a commit, no semver. Since llama.cpp's release tags are now
  semver, the two sides had no shared scheme and every check came back
  incomparable. The binary's own `--version` banner carries both halves
  (`version: 0.2.0-dev (build 10566, commit bb4caa754)`), so the daemon now
  borrows the semver from there.
- **The borrow only happens when both sources agree on the build number.** A
  binary that has been rebuilt but not yet restarted into is a *different* build
  from the one actually serving; lending it its semver would describe the
  running server as a version it is not. On a mismatch the comparison stays
  honestly unknown rather than becoming confidently wrong — this is not
  hypothetical, it is the exact state this box was in between building v0.2.0
  and swapping it in.

Three new tests covering the live case, the build-mismatch refusal, and the
no-op paths. 466 tests total. (LAN-1153)

## 0.1.10 — 2026-08-21

A single fix, cut same-day as 0.1.9 because it is the exact failure class 0.1.9
was about — and because it was actively lying on the maintainer's own box.

### Fixed

- **`rookery releases` reported "✓ up to date" for llama.cpp while 193 commits
  behind.** On 2026-08-21 ggml-org moved from `bNNNNN` release tags to semver
  (`v0.2.0`), and **both** halves of the comparison stopped parsing at once. The
  binary's own banner changed from `version: 10380 (0b1bad14f)` to
  `version: 0.2.0-dev (build 10566, commit bb4caa754)`, so `build_number` came
  back `None`; and `parse_tag_build_number("v0.2.0")` returned `None` because it
  only ever stripped a leading `b`. Either failure alone made
  `compare_llama_versions` return `(false, false)` — indistinguishable, to every
  caller, from a genuine match. The CLI's final `else` then printed
  "✓ up to date".
- **The root cause was the signature, not the parsers.** `(bool, bool)` had no
  way to say "could not compare", so a parse failure had to borrow the encoding
  meaning "current". `compare_llama_versions` now returns
  `Option<(bool, bool)>`, `RepoReleaseState` carries `version_comparable`, and
  the CLI renders the unknown case as **`? unknown (version schemes differ)`**.
  A future upstream format change now surfaces as unknown instead of going
  quiet again.
- **Semver support fixes vLLM's row too.** vLLM has always tagged `vX.Y.Z`, so
  its comparison never worked — 0.1.9's changelog documented that as a known
  limitation rather than a bug. Both repos now compare correctly.
- Prerelease suffixes are **discarded rather than ordered**: llama.cpp stamps
  `-dev` even on a build made from the release tag itself, so treating
  `0.2.0-dev < 0.2.0` would have reported an update forever while sitting
  exactly on the release. The trade-off is noted in the source.
- `version_comparable` defaults to `true` when absent, so a release cache
  written by an older daemon does not turn every row into "unknown" on upgrade.

Six new tests, including the exact banner and tag strings that produced the
false negative, and one asserting an old `bNNNNN` binary against a semver tag
reports unknown rather than either answer. (LAN-1152)

## 0.1.9 — 2026-08-21

The largest release on the 0.1.x line: 44 tickets over 86 commits. One theme
dominates it. Rookery had five separate places where "we could not measure this"
was rendered as "we measured zero" — empty GPU gauges presented as live, a
silently-stale SSE stream trusted as current, missing slot data drawn as
`idle / 0 / 0 / 0`, a failed benchmark byte-identical to one that never ran, and
an NVML failure read as 0 MB free VRAM that then blocked a start and told the
user to go hunting a leak that did not exist. All five are fixed. Alongside that:
two durability features that would have changed the outcome of the 2026-08-15
Hermes `state.db` corruption, three things that were wholly broken and unreported
(model downloads over 30s, `/api/chat` and `/api/bench` on vLLM, and the vLLM
backend itself), and a CLI exit-code contract that scripts can finally trust.

### Behaviour changes

These change what existing commands, configs and metrics do. Read them before
upgrading.

- **`rookery status` exits 1 when the daemon is offline** (was 0), in both plain
  and `--json` mode. The JSON body is still printed, and its offline shape gains
  an `error` field alongside the existing `state` and `daemon_url`, so `jq`
  pipelines keep working.
- **`rookery sleep` and `rookery wake` exit non-zero on daemon-reported failure.**
  Both previously printed the message and exited 0, so `rookery wake && rookery
  bench` benched a server that never woke. The idempotent cases — sleeping an
  already-sleeping server, waking an already-running one — still exit 0, because
  the daemon reports those as `success: true` and the CLI honours its verdict.
- **Six more commands exit 1 on `success: false`**: `start`, `swap`, `agent
  start`, `agent stop`, `agent update`, `models pull`. `--json` mode now matches
  plain mode everywhere — previously `rookery start` exited 1 and `rookery start
  --json` exited 0 on the identical body.
- **The daemon refuses to boot on an invalid config.** `Config::validate()` was
  only ever called by the CLI, so the daemon used whatever it loaded: a
  `default_profile` that does not exist fell through to HashMap iteration order
  and silently started some other profile's backend, and an empty `[profiles]`
  table hit a raw panic. Invalid config is now `exit(1)` at boot, which includes
  a missing `llama_server` binary. `StartLimitIntervalSec=300` /
  `StartLimitBurst=5` in `rookery.service` bounds the resulting restart loop.
- **`rookery_server_up` reports 0 while swapping**, labelled with the *target*
  profile. It previously reported 1 against the *old* profile's label for the up
  to two minutes a swap takes, which is a lie in both halves. Labelled 0 lets an
  alert rule tell "intentionally out of service" from a crash, which reports
  `profile=""`.
- **An agent error-pattern match now needs 3 occurrences within 10 minutes to
  trigger a restart**, not one. A single-shot fatal pattern therefore no longer
  restarts an agent. Threshold and window are hardcoded; the known ceiling is
  that matches sparser than one per five minutes are treated as transient.
- **A manual `rookery agent stop` now takes a database backup first**, adding a
  few seconds to the stop. Daemon shutdown and watchdog crash/port bounces
  deliberately opt out.
- **`vram_free_mb` can serialize as `null`.** `GET /api/hardware` reports null
  rather than 0 when the NVML query fails; consumers that assumed a number must
  handle it.

### Added

- **A daily read-only SQLite `quick_check` sweep over agent databases.** Hermes'
  `state.db` was corrupted on 2026-08-15, sat undetected for weeks, and cost 153
  unrecoverable messages; nothing on the box would ever have reported it. The
  watchdog now runs `PRAGMA quick_check` once per local day shortly after 04:00
  over `*.db` in the agent's `data_dir` (falling back to `workdir`; skipped if
  neither is set) and its immediate subdirectories. It reports and never acts —
  the first thing a restart does is reopen and write to the damaged file.
  Corruption is logged at error under the agent's own `[agent:name]` prefix and
  exported as `rookery_agent_db_corrupt`, with `rookery_agent_db_unchecked` and a
  last-check timestamp beside it. `count(*)` is not an integrity signal and the
  code says so: on the real corrupt `state.db` it happily returned 25,654 rows,
  answered from an index that never touched the damaged leaf pages, while
  `max(id)` failed outright. Shells out to `sqlite3` rather than linking
  rusqlite; a missing binary reports *unchecked*, never healthy. (LAN-1070)
- **Pre-change `VACUUM INTO` backups of agent databases.** `hermes update`
  migrates config in place, and on 2026-08-15 there was no pre-change copy to
  restore from. Every deliberate bounce — the update route's stop, the swap
  route's automated stop, and a manual stop — now copies each database to
  `<data_dir>/db-backups/<UTC timestamp>/<name>.db.bak` first, keeping three
  generations. `sqlite3 -readonly` is load-bearing, not hygiene: measured against
  a database with 2,867,552 bytes of uncheckpointed `-wal`, a read-write
  `VACUUM INTO` checkpointed and deleted the `-wal`/`-shm` sidecars, while the
  read-only copy left all three byte-identical and still recovered all 20,000
  rows. Fails open — a backup failure logs at error but never blocks the stop.
  (LAN-1088, LAN-1125)
- **`POST /api/reload` and `rookery reload`** re-read the config file in place.
  `Config::load()` previously ran exactly once at boot, so adding a profile or
  repointing a `[models.*]` path required `systemctl restart rookery` — on this
  box a ~24 GB model reload and a Hermes bounce to add a config entry, i.e. a
  standing incentive to restart the daemon during routine work. Nothing is
  restarted: the live backend keeps its profile, port, PID and binary, and no
  agent is touched. The file is parsed and validated into a *local* `Config`
  before `state.config` is written, so a rejected reload leaves the daemon on the
  config it was already serving — deliberately the inverse of the boot behaviour
  above, where invalid config is `exit(1)`. Anything a reload cannot honour comes
  back in `warnings` (a changed `listen`, edited `[agents]`, a port change on the
  live profile, the live profile having been deleted), and the response names
  which fields apply now, which apply on the next start/swap, and which still
  need a restart. Takes `op_lock` bounded at 5s, returning 409 rather than
  blocking behind a ~135s worst-case swap. (LAN-1090)
- **`ServerState::Swapping { from, to, since }`.** `post_swap` broadcast nothing
  until it finished — drain 5s, stop, start, then up to a 120s health wait — so
  for up to two minutes every client reported the OLD profile as `running` while
  its process was already gone, and three impatient clicks serialised on
  `op_lock` into ~5 minutes of model loading. The CLI, `/api/status` and the SSE
  `state` event all pick it up; the dashboard's profile cards disable while it is
  set. (LAN-1081)

### Fixed

#### "We don't know" rendered as "we measured zero"

- **A failed NVML query no longer renders as fresh zeroes.** The GPU stream did
  `.and_then(|m| m.stats().ok()).unwrap_or_default()`, so an NVML failure kept
  emitting on schedule with `gpus: []` and the dashboard drew empty gauges,
  labelled `Live`, as current truth. Fresh-but-wrong cannot be caught by a
  staleness watchdog, so the payload now carries an `error` marker and the panel
  renders "GPU monitor unavailable" plus the NVML reason. Set only when NVML
  initialised and the *query* failed — a machine with no GPU renders exactly as
  before. (LAN-1121)
- **A silently-stale SSE stream is now detected instead of trusted.**
  `EventSource` only notices that the socket died; a wedged llama-server or a
  stalled stat task leaves `readyState` at OPEN with no error, so the VRAM gauge
  presented a four-minute-old number as live. The daemon now emits a named `ping`
  event every 2s — a *named* event, because browsers never surface SSE comments
  to JavaScript and never route named events to `onmessage`, so the heartbeat
  resets staleness without ever being read as data. A 1s client watchdog derives
  live/stale/reconnecting from the age of the last event plus `readyState`, and
  stale or dead dims every number on the page via a class on the app root.
  (LAN-1082)
- **Missing slot data is now missing on every backend.** The daemon returns
  `{"available": true, "slots": null}` whenever the `/slots` proxy fails (404,
  `--no-slots`, port gone mid-swap). The dashboard special-cased that for vLLM
  only, so llama-server — the daily driver — fell through to `unwrap_or(0)` and
  rendered `idle / 0 / 0 / 0` styled identically to real data. (LAN-1145)
- **A failed bench no longer renders as one that never ran.** All three failure
  modes — the 60s client timeout, a non-2xx upstream, and a 200 whose body
  carries no `timings` block — returned `200 {"tests": []}`, which the panel drew
  as "no results yet": byte-identical to the initial never-run state. The button
  flipped back and that was the entire signal. `BenchResult` grows an `errors`
  array, one entry per prompt that produced no measurement, carrying up to 200
  chars of the upstream body (which is where "model not found" lives). The panel
  now has three states, with an "n of m tests failed" note for the partial case.
  (LAN-1094)
- **An NVML failure no longer reads as 0 MB free VRAM.** `live_vram_free_mb`
  collapsed the error to zero, which is indistinguishable from a genuinely full
  GPU: the fit check then blocked the start and told the user to go hunting a
  leak that was not there. A new `try_live_vram_free_mb()` returns `Option<u64>`;
  `get_hardware` serializes null, the quant endpoints add `vram_known` so the UI
  can say the estimates are guesses, and `models pull` and `models recommend` now
  say "could not read GPU VRAM (NVML query failed)" instead of "no quant fits in
  available memory". (LAN-1092, LAN-1127)

#### Wholly broken, and nobody knew

- **`rookery models pull` aborted every download longer than 30 seconds.**
  `HfClient::new` built one reqwest client with `.timeout(30s)`, and reqwest's
  total timeout covers the streamed response body — so a 25 GB GGUF, which needs
  ~250s at 100 MB/s, could never have completed; it failed with "download stream
  error". The download path now has its own client with a 30s *read* timeout and
  no total deadline, which bounds "no bytes arrived recently" without bounding
  "this file is large". The API client keeps its 30s total deadline, correct for
  small JSON. (LAN-1146)
- **`/api/chat` and `/api/bench` 404'd on every vLLM profile.** Both hardcoded
  `"model": "test"`, and vLLM validates `request.model` against its served name.
  Chat failed every request; bench silently returned zero tests. Both now reuse
  the same `served_model_id()` lookup, deliberately uncached — a stale name is
  exactly the 404 this prevents. (LAN-1130)
- **The vLLM backend could not work as generated.** Four compounding defects: the
  inference canary hardcoded the same `"model": "test"`, so `check_inference()`
  returned false unconditionally and restart-looped every vLLM profile forever
  (and broke daemon-restart reconciliation, which gates adoption on the same
  call); the generated compose file had no volumes at all while `stop()` runs
  `docker compose down`, so every start re-downloaded the full model and a slow
  first pull was torn down by the health timeout; that health timeout was 120s,
  copied from the llama-server path which mmaps a local GGUF, against a vLLM cold
  start that is routinely 3-5 minutes for a 27B (now a 300s constant scoped to
  `VllmBackend`, deliberately not shared); and `stop()` aborted log capture
  *before* `docker compose down`, so a transient down failure left a container
  serving with no log stream and a permanently dead CUDA-error channel.
  `spawn_log_capture()` also leaked a `docker compose logs -f` child on every
  replacement, because dropping a `JoinHandle` detaches rather than cancels.
  (LAN-1086)

#### Shutdown, state and lifecycle correctness

- **Shutdown now takes `op_lock`, bounded to fit `TimeoutStopSec=45`.**
  `stop_all()` and `backend.stop()` ran without it, while `post_swap` holds the
  lock across drain → stop → start → 120s health wait, and aborting `axum::serve`
  does not cancel already-spawned connection handlers. A SIGTERM landing in that
  window could stop the old backend and exit while the swap went on to spawn its
  llama-server — ~30 GB of VRAM held by an unsupervised process, which is why
  `main()` has an orphan reaper. The wait is bounded at 20s rather than unbounded
  because a worst-case swap holds the lock ~135s and blocking longer earns a
  SIGKILL that skips teardown entirely — strictly worse than the race. (LAN-1074)
- **A racing swap or start can no longer spawn an orphan after the daemon exits.**
  This is the root cause the bounded wait above could only mitigate. The guard
  sits in `start_profile()` rather than on the routes, so every start path is
  covered — swap, start, wake, `post_chat`'s wake, and the canary restart — and
  it lands `Stopped`, matching what the shutdown path writes anyway. It is placed
  after the unconditional `clear_drain()` so the new early return cannot leak
  `draining = true` and wedge `post_chat` on a permanent 503. (LAN-1120,
  LAN-1128)
- **Failure paths land a terminal state instead of sticking on a transient one.**
  `post_stop` broadcast `Stopping` up front and then returned 500 without landing
  anything, so the dashboard badge, CLI status and `rookery_server_up` sat on
  `Stopping` until the daemon restarted. It now reports `Running` when the
  process demonstrably survived — a failed stop often means it did — and
  `Failed { last_error }` otherwise. The swap error path likewise now lands
  `Failed` instead of a stale `Running{old}`. (LAN-1123, LAN-1081)
- **A typo'd profile name no longer takes the running model down.** `post_swap`
  drained and stopped the backend *before* looking up the requested profile, then
  failed with a 500. The lookup moved first, returning 404 naming the unknown
  profile and the valid ones. (LAN-1072)
- **The canary re-checks the busy-slot guard before retrying and before
  restarting.** The guard ran once, before the first check; with `--parallel 1` a
  user request landing after that poll holds the only slot for the whole
  generation, so both canary requests timed out queueing behind it and the canary
  SIGTERMed a server that was working correctly. (LAN-1073)
- **Agent liveness is derived from the OS, not from `info.status`.** That field is
  set to `Running` at start/adopt and never mutated, so `get_health` reported
  every tracked agent as running with a growing uptime regardless of whether the
  process was alive — permanently, not for a poll interval, since the watchdog
  only evicts dead agents that have `restart_on_crash` set. `rookery_agent_up` is
  derived from this field, so the gauge could never read 0 and any alerting on it
  was watching a constant. Five inline copies of the liveness check are now one
  `agent_is_alive()`. (LAN-1071)

#### CLI contract and outbound timeouts

- **The exit-code contract is documented and honoured.** `docs/cli.md` now
  specifies 0 success, 1 runtime failure (daemon unreachable or daemon-reported),
  2 usage error — clap already owns 2, and scripts needing more can read `.error`.
  The daemon answers HTTP 200 with `success: false` for genuine failures (profile
  mismatch, insufficient VRAM, "server failed to start"), and six commands
  printed that and returned `Ok(())`, so `rookery start && rookery agent start
  hermes` started the agent against a dead inference server. See the behaviour
  changes above for the full list. (LAN-1084, LAN-1102, LAN-1148)
- **`--json` mode has one error contract.** On a client error, non-status `--json`
  commands printed nothing at all, so `rookery gpu --json | jq .` handed jq an
  empty stdin whenever the daemon was down. All `--json` paths now emit
  `{"error": ..., "daemon_url": ...}` and exit 1, routed through one exhaustive
  `wants_json` match so a new subcommand cannot silently miss it. (LAN-1102)
- **Upstream chat errors surface as 502 instead of being laundered into 200 OK.**
  `.send()` only returns `Err` on transport failure, so a non-2xx from llama.cpp
  arrived as `Ok(resp)` and was streamed through under a hardcoded
  `200 text/event-stream`. The body was not even SSE-framed, so an `EventSource`
  client saw a clean successful stream that yielded nothing, while
  `rookery_chat_errors_total` stayed at 0 and `requests_total` incremented.
  (LAN-1075)
- **Chat errors on the stopped-server path are counted.** The two trailing arms of
  `post_chat`'s port-resolution match returned 503 after `inc_chat_request()`
  without ever calling `inc_chat_error()` — so the two counters disagreed on the
  most common failure there is. (LAN-1101)
- **Outbound HTTP clients bound the connect phase.** `releases.rs`, `models.rs`
  and `DaemonClient` had a total `.timeout()` but no `.connect_timeout()`, and a
  total timeout does not usefully bound a TCP connect against a peer that
  blackholes SYNs — the kernel retry loop runs ~130s. All now use a 2s connect
  timeout (measured 30s before, 2.0s after). The CLI's SSE follower gets connect
  timeout only, so `logs -f` still streams indefinitely. (LAN-1084, LAN-1103)
- **A config file that exists but fails to parse now warns** with its path and the
  fallback URL, instead of being discarded by `.ok()`. A missing file stays
  silent — that is the normal pre-install state. (LAN-1084)

#### Config, cache and model resolution

- **`Config::validate()` now checks models.** It did not look at them at all, and
  `resolve_llama_server_command_line()` emits no model argument for a `source` it
  does not recognise — so `source = "HF"` validated clean and produced a
  llama-server command line with no model, failing later with llama.cpp's own
  opaque error. `source` must now be `hf` or `local`, local needs `path`, hf
  needs `repo`, and hf needs `file` for a llama-server profile. Fields only, no
  `path.exists()`: an HF cache populates on first start and model dirs can be
  lazily mounted, and validate() is now a boot-time hard failure. (LAN-1076)
- **The cache match is scoped to a repo.** `mark_downloaded` matched on quant
  label alone, so a `UD-Q6_K_XL` cached for one repo marked every other repo's
  `UD-Q6_K_XL` as already downloaded — the user skips a multi-GB download that
  never happened. (LAN-1092)
- **Snapshot selection is deterministic.** `scan_hf_hub_cache` walked snapshot
  directories in readdir order, so a repo with two snapshots — this box has two,
  for Qwen3.6-27B and 3.8-27B — could report the stale one's path. It now picks
  the snapshot `refs/main` names, falling back to newest mtime then greatest
  name. The same block recorded only the first shard's size for a sharded quant
  while `extract_quants` summed them, so the two views disagreed; and it scanned
  only the snapshot's top level, missing nested quants such as Unsloth's
  `mtp-*.gguf`. (LAN-1092)
- **VRAM subtractions use `saturating_sub`.** In release builds an unsigned
  underflow on `used > total` wraps to ~1.8e19, which silently passes the
  capacity gate. (LAN-1092)
- **The "no cached models" message named a directory nothing uses.** It cited
  `~/.cache/llama.cpp/` alone, though `scan_cache` also covers `$HF_HOME/hub/`
  and configured `model_dirs` — on this box every cached model is under
  `$HF_HOME/hub/`. (LAN-1092)
- **The settings panel can no longer write one profile's values onto another.**
  Since 0.1.7 `PUT /api/config/profile/{name}` really writes to disk, so every
  way this panel could show the wrong numbers under the wrong name became a way
  to corrupt a live config. Name and values are now committed together, the form
  is unrendered until real values arrive, and the inputs bind `prop:value` rather
  than `value=` — an attribute write is ignored once the DOM's dirty value flag
  is set, so a field the user had typed into never picked up a newly fetched
  profile's value again. It also reads the `[llama_server]` sub-table when
  present: on the shipped example config the panel showed `ctx_size 262144` for a
  profile configured at 131072, and a Save would have written that back. Clearing
  a field is now an error rather than a silent no-op that returned a "saved"
  toast followed by the old value on reopen. (LAN-1095)

#### Dashboard

- **Tab state survives switching.** The tab body was one reactive closure
  returning a differently-typed view per arm, so each switch unmounted the
  previous subtree and disposed its reactive owner — taking the chat
  conversation, the bench results and the models search with it. Run a bench,
  check the logs, come back: empty table. All seven panels now render once and
  toggle `display`, which also stops ModelsPanel re-issuing its hardware and
  cached-model fetches on every visit. (LAN-1078)
- **Uptime actually ticks.** `uptime_secs` only reaches the dashboard on an SSE
  `state` event, and those fire on transition, never on a timer — so the Overview
  card showed `0h 0m 0s` for a server that had just started, until the next
  start/stop/swap. On a box where the server runs 12+ hours at a stretch, that is
  "always". The value is now anchored against `performance.now()` at the moment
  it lands and derived from a 1s tick; wall time was rejected because it steps on
  an NTP correction, a DST change or a resume from sleep. (LAN-1077)
- **`--muted` and `--dim` now clear WCAG AA.** Dark `--muted` measured 1.70:1 on
  `--surface` — the colour of every status message in the app, including the
  failure ones. Dark muted 1.70 → 4.65:1, dark dim 3.67 → 6.91:1, light muted
  2.33 → 4.87:1. Values stay on the zinc ramp's hue and keep three distinct
  tiers, and the stale/dead dimming still leaves stale numbers below the dimmest
  live tier, verified by compositing the real opacities in a browser. (LAN-1080)
- **The chat stream no longer corrupts split UTF-8.** `TextDecoder.decode()`
  without `{ stream: true }` treats every call as complete input, so a sequence
  split across a network chunk boundary decoded to U+FFFD — box-drawing, arrows,
  CJK and emoji came back as replacement characters, and a mangled `data:` line
  could fail `serde_json::from_str` and drop the token with no error at all. Also
  drops the `js_sys::eval("new TextDecoder()")` that was the one construct in the
  dashboard a Content-Security-Policy would break. (LAN-1079)
- **A permanently dead SSE stream now reconnects.** Per spec a non-2xx response
  closes an `EventSource` for good, and the daemon returns 429 past
  `MAX_SSE_CONNECTIONS` and 401 on a bad key — so a 17th tab or a stale key got a
  stream that never came back while status sat at the default. Retry is
  1/2/4/8/16s capped at 30s with ±25% jitter (the tabs hitting the connection cap
  are by definition all retrying at once), single-flight guarded, and a probe
  distinguishes 429 from 401 so a rejected key stops rather than burning
  requests. (LAN-1082)
- **SSE reconnects no longer leak closures.** `connect_sse` registered its six
  listeners with `Closure::forget()` — harmless when the connection was made once
  at startup, ~120 leaked closures an hour at the 30s backoff cap once LAN-1082
  added explicit reconnects. Teardown order is load-bearing and now lives in
  `Drop`: close first, then detach every listener, then free. Inverting it as a
  negative control produces 12,520 "closure invoked recursively or after being
  dropped" errors and leaks the connection count to the cap. (LAN-1122)
- **User values are percent-encoded in dashboard URLs.** A search containing `&`
  split into a second parameter, `#` truncated the rest as a fragment, and `+`
  decoded server-side as a space. Applied to all six call sites that interpolate
  a user-controlled value, not just the two reported. (LAN-1097)
- **The models panel no longer shows one repo's quants under another's name.**
  `select_repo` never cleared `quants`, so clicking repo A then repo B showed
  "Quants — B" over A's rows for the duration of B's fetch, or permanently if
  that fetch errored. Each row binds the new repo with the old label, so "pull"
  asked the daemon for one of A's quant labels under repo B — a multi-GB download
  of the wrong file. A successful fetch returning no quants, and a failed fetch,
  now each get their own message instead of falling through to an empty div.
  (LAN-1093)
- **Start/Stop decides the verb at click time.** Nothing pushes agent state to the
  dashboard, so `agents` only moves on a 10s poll — a watchdog crash-restart, the
  CLI or another dashboard left the button sending Stop to a process that had
  just come back under a new PID. The handler now re-reads `/api/agents` first
  and feeds the response back into the signal. The double-send guard is promoted
  from `disabled` (applied on a microtask, so a synchronous burst still reached
  the handler — three clicks sent three stops) to a synchronous check at the top
  of the handler. (LAN-1124, LAN-1093)
- **Agent health polling is gated on tab visibility.** Since panels stay mounted,
  the health effect re-ran on every 10s `agents` poll — one health GET per running
  agent per 10s, forever, whatever tab you were on. (LAN-1116)
- **Accessibility: five scoped fixes, from zero `aria-` attributes.** Chat
  auto-scrolls (following the stream only when already within 50px of the
  bottom); the agent card's details affordance is a real `<button>` rather than a
  keyboard-unreachable `<div>`; toasts get `role="status"`, errors live 15s
  instead of 3s and any toast dismisses on click — toasts are the only channel
  for swap/start/stop/save/bench failures; and chat state is announced through a
  separate visually-hidden live region carrying state strings only, because a
  live message container queues one announcement per token (measured: 402
  mutations in the container, 2 in the live region). (LAN-1096)
- **`tabular-nums` on body and `content-visibility: auto` on log lines.**
  (LAN-1083)

### Changed

- **Dead code removed.** `subscribe_errors()` and both `watch<bool>` CUDA
  channels were superseded when error handling became daemon-scoped; they had no
  receivers outside `#[cfg(test)]`, so every `send(true)` pushed into nothing, and
  the trait method forced every future backend to implement it.
  `ProcessManager::swap` had no callers and `start_and_wait` had only `swap` —
  both carried traps, since `start_and_wait` returned `Ok(ServerState::Failed)`
  on health failure (so `is_ok()` reads as success) and `swap` still had the
  drain-leak bug fixed in 0.1.6. Also `ReleaseCache::has_updates` and the never
  referenced `CANARY_HEALTH_TIMEOUT`. Separately, `ProcessManager::adopt` now
  clears `self.child` — it only ever set `self.info`, so a stale `Child` left
  `is_running()`/`stop()` acting on the old process. Test count 410 → 402.
  (LAN-1087)
- **`Error::ProfileNotFound` carries a hint** that config is read at daemon start,
  since "profile not found: qwen39" is otherwise indistinguishable from a typo
  when the real cause is a profile the running daemon never loaded. (LAN-1090)
- **`config --json` pretty-prints** like every other `--json` path. (LAN-1084)
- **Two flaky tests de-raced.** `test_cache_roundtrip` wrote to a fixed shared
  path in `temp_dir()`, so concurrent runs across worktrees clobbered each other
  — measured 39,371 of 73,577 runs failing under six concurrent copies, 0 of
  74,978 after. `test_agent_crash_detected_on_list` raced a 100ms self-exit timer
  against `start()`'s two fsyncs, which stall 150-200ms on a busy disk; the child
  is now killed explicitly, which also models a crash more honestly. The
  `LAN-1088` WAL fixture had the same shape and is fixed in that ticket.
  (LAN-1151, LAN-1147)

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

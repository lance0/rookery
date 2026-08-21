# Rookery Test Gap Analysis

**Date:** 2026-08-21
**Current test count:** 457 (59 CLI + 50 core + 116 daemon + 232 engine)
**At the 0.1.8 tag (`f9ee1e7`):** 358

This document was first written on 2026-03-28 against a 171-test suite. Most of
the gaps it catalogued have since been filled. Where a section below cites a
prior count, it is the **0.1.8** figure, not the original 2026-03-28 one — the
suite had already grown substantially between those two points.

> The dashboard crate is **excluded from the cargo workspace**, so its own tests are
> not part of the 457 and do not run under `cargo test --workspace`. See
> [testing.md](testing.md) for how to run them.

---

## 1. Roadmap Test Plan (from ROADMAP.md § Code Quality & Testing)

| Status | Item |
|--------|------|
| ✅ Done | `rookery-core`: config parsing, state serialization, reconciliation (now 50 tests) |
| ✅ Done | `rookery-engine`: log buffer, model utils, is_pid_alive, version parsing, AgentManager (now 232 tests) |
| ✅ Done | `rookery-engine`: **ProcessManager start/stop** — mock llama-server landed as `test_utils.rs` |
| ✅ Done | `rookery-daemon`: **route handler integration tests** — `routes::tests::route_integration`, 59 tests via axum `oneshot` |
| ✅ Done | `rookery-daemon`: **SSE event stream tests** (`sse.rs`, 9 tests) |
| ✅ Done | `rookery-cli`: **CLI argument parsing, output formatting** (31 unit) + **exit codes** (`tests/exit_codes.rs`, 28 integration) |
| ⚠️ Partial | `rookery-engine`: **watchdog behavior** — crash detection and error-pattern restart are covered; backoff timing and `depends_on_port` bounce are not |
| ❌ TODO | **End-to-end**: daemon startup → start → swap → agent lifecycle as one flow |

`ProcessManager::swap` and `start_and_wait` no longer appear here because the
functions were deleted (LAN-1087); swap is orchestrated by the daemon route
against the `InferenceBackend` trait.

---

## 2. Per-Module Analysis

### 2.1 `rookery-engine::process` (process.rs)

**Current tests: 20** (0.1.8: 22 — the drop is deleted code, not lost coverage:
`ProcessManager::swap` and `start_and_wait` went away with LAN-1087 and took
their tests with them.)

Now covered: `start()` return value and stdout capture, owned-child stop,
adopted kill-by-PID, `adopt()`, `is_running()` for both paths,
`to_server_state()` both directions, draining flag, CUDA and GGML CUDA error
detection in stderr with no-false-positive controls, and double-start rejection.

**Still untested:**
1. OOM score adj write failure (non-root) — the failure path only logs a warning
2. Empty command line returns an error before spawn

**Estimated new tests: 2**

### 2.2 `rookery-engine::backend` (backend.rs)

**Current tests: 71** (0.1.8: 73) — still the largest test module. The two lost
tests covered `subscribe_errors()`, which was removed from the trait.

Every gap previously listed here is closed:
- `LlamaServerBackend::start()` against a real health endpoint, success and
  failure, including process cleanup when health never comes up
  (`test_llama_backend_start_then_health_succeeds_with_mock`,
  `test_llama_backend_start_failing_health_cleans_up_process`)
- `VllmBackend` full lifecycle, gated behind `ROOKERY_INTEGRATION=1`
  (`test_integration_vllm_start_and_health`, `_stop_removes_container`,
  `_is_running_lifecycle`, `_orphan_adoption`)
- `VllmBackend::adopt()` container-id requirement and orphan adoption
- `VllmBackend::stop()` error propagation on `docker compose down` failure
- `is_cuda_error()` — 8 tests including five distinct false-positive controls
- Swap drain lifecycle for both backends, plus backend replacement during swap

**Untested behaviors:** none material. This module is the coverage benchmark for
the rest of the codebase.

### 2.3 `rookery-engine::health` (health.rs)

**Current tests: 17** (0.1.8: 15)

All six previously listed gaps are closed: `wait_for_health` success, timeout,
exponential backoff, and recovery after initial failures; `check_health` on 200,
500, connection refused, and timeout; `check_inference` on the same four; and
`HealthError` display for both variants.

Two tests cover behaviour that did not exist when this document was written:
`test_check_inference_uses_served_model_name` and
`test_check_inference_falls_back_when_models_endpoint_missing`. The canary now
discovers the served model via `GET /v1/models` rather than sending a hardcoded
`"test"`, which is what made every vLLM profile 404 and restart-loop.

**Untested behaviors:** none material.

### 2.4 `rookery-engine::agent` (agent.rs)

**Current tests: 37** (0.1.8: 29)

Now covered: `adopt()` and its three refusal paths (dead PID, mismatched
cmdline, missing config), adopted kill-by-PID, `stop_all()`, `list()` with dead
agent cleanup, agent persistence roundtrip and reconcile, crash detection,
error-count tracking and reset across restart, below-threshold non-restart,
`is_running()` for adopted vs owned, env var passing, workdir setting,
`get_health` reporting a dead agent as stopped, and pre-change database backups
on the update and swap-bounce paths.

**Still untested:**
1. Watchdog `depends_on_port` bounce logic (down→up transition)
2. Watchdog healthy-agent backoff reset after 5 min — the timing, specifically

**Estimated new tests: 3–4** (both need a time-controlled runtime)

### 2.5 `rookery-engine::gpu` (gpu.rs)

**Current tests: 2** (0.1.8: 2)

`process_name()` is covered for both the live-process and nonexistent-PID cases.

**Still untested:**
1. `find_orphan_llama_servers()` — needs a mockable `stats()`; NVML-dependent
2. `GpuMonitor::new()` / `stats()` — require a real GPU

Note that failed NVML queries are now distinguished from a machine with no GPU;
that distinction is tested in `hardware.rs`
(`test_try_live_vram_free_reports_unknown_not_zero`) and at the route layer
(`test_route_hardware_vram_free_is_null_when_nvml_unavailable`), not here.

**Estimated new tests: 1–2** (integration-gated)

### 2.6 `rookery-engine::logs` (logs.rs)

**Current tests: 5** (0.1.8: 5)

`subscribe()`, `len()`/`is_empty()`, and concurrent push from multiple tasks are
all covered. The mutex-poison recovery entry previously listed here no longer
applies — the `unwrap_or_else(|e| e.into_inner())` pattern is gone from the
codebase.

**Untested behaviors:** none material.

### 2.7 `rookery-engine::compose` (compose.rs)

**Current tests: 20** (0.1.8: 18)

Both previously listed edge cases are closed
(`test_compose_with_max_model_len_field`,
`test_compose_all_optional_vllm_params_set_simultaneously`).

Two new tests cover the HuggingFace cache bind mount
(`test_compose_mounts_hf_cache`, `test_hf_cache_volume_honours_hf_home`).
Without that mount, `docker compose down` discarded the weights with the
container's writable layer and every start re-downloaded the model.

**Untested behaviors:** none material.

### 2.8 `rookery-engine::models` (models.rs)

**Current tests: 20** (0.1.8: 13)

Now covered: `scan_cache()` on an empty directory, HF hub scanning
(deterministic snapshot selection via `refs/main`, newest-mtime fallback,
shard summing, subdirectory-packed quants), `recommend_quant()` across
fits/partial-offload/nothing-fits, quant preference ordering, repo normalization
edge cases, and `mark_downloaded` no longer leaking a cached quant across repos.

Network behaviour is covered at the boundary rather than end-to-end:
`test_hf_client_bounds_connect_to_blackhole` pins the connect timeout, and
`test_download_survives_body_longer_than_api_timeout` pins that downloads use a
separate client with a read timeout and no total deadline.

**Still untested:**
1. `attach_estimates()` — performance estimation is pure logic and very testable

**Estimated new tests: 2–3**

### 2.9 `rookery-daemon::routes` (routes.rs)

**Current tests: 83** (0.1.8: 60), of which 59 are in the
`routes::tests::route_integration` module using an axum test client.

Every route previously listed as needing integration tests now has them:
status, start (including idempotent same-profile and shutdown abort), stop
(including both failure landings), swap (drain, unknown profile, shutdown
abort), sleep/wake, profiles, health, config redaction, put_profile (update and
404), model info, chat (running, draining 503, sleeping-wake, upstream error,
stopped-path error counting, served model name), bench (timings, no timings,
partial failure, failure surfacing, served model name), logs, agents
(list, start/stop lifecycle, update success/failure/backup paths), dashboard
(static asset and SPA fallback), hardware, metrics, reload, and the 1 MB body
size limit returning 413.

SSE connection limits and the initial state event moved to `sse.rs` and are
covered there.

**Still untested:**
1. `get_models_search`, `get_models_quants`, `get_models_recommend`,
   `get_models_cached`, `post_models_pull` — the only route family without
   integration coverage. `get_models_quants` is the one with real logic, since
   it is what wires the repo-aware `mark_downloaded`.

**Estimated new tests: 5–8**

### 2.10 `rookery-daemon::canary` (canary.rs) and `main.rs`

**Current tests: 10 in `canary.rs`, 4 in `main.rs`** (0.1.8: 10 and 3)

The canary was extracted from `main()` into its own module, exactly as this
document proposed, and is now tested through the `InferenceBackend` trait.
Covered: restart after two inference failures, healthy-backend no-restart,
skip when draining, skip when not running, op_lock acquisition during restart,
skip if stopped while waiting on the lock, restart state transitions
(Running → stop → start → Running), restart failure landing Failed, start
failure, operating through the trait interface, and the **busy-slot guard**
(`test_canary_skips_restart_when_slots_busy`) — the canary no longer restarts a
server that is merely busy serving a long request.

`main.rs` covers reconciliation liveness checks (PID check for llama-server,
backend check for vLLM) and bounded shutdown op_lock acquisition.

The "canary re-subscribe after swap" entry is obsolete: `subscribe_errors()` was
removed from the `InferenceBackend` trait (LAN-1087), so there is no per-backend
error channel to re-subscribe to. CUDA error propagation is still tested inside
`backend.rs`, and `test_cuda_error_skips_restart_for_stale_backend_event` covers
the stale-event case.

**Still untested:**
1. Orphan cleanup: `find_orphan_llama_servers` → SIGTERM → wait → SIGKILL
2. Auto-start agents on daemon init
3. Full graceful shutdown sequence (SIGTERM → stop agents → stop server →
   persist Stopped) — only the op_lock portion is covered

**Estimated new tests: 4–6**

### 2.11 `rookery-daemon::sse` (sse.rs)

**Current tests: 9** (0.1.8: 7)

All five previously listed gaps are closed: connection limit rejection at max,
connection count increment, no counter leak on a rejected connection, initial
state event on connect for both running and stopped, the keep-alive as a named
`ping` event, the state event field set, and GPU events staying quiet without
NVML.

**Untested behaviors:** none material.

### 2.12 `rookery-core::config` (config.rs)

**Current tests: 32** (0.1.8: 27)

All five previously listed gaps are closed: `Config::load()`/`save()` via a real
filesystem roundtrip, `validate()` rejecting a missing default_profile,
`resolve_profile_name()` returning the default when None, and model source
validation across local/HF and both backends.

Atomic write is covered separately in `rookery-core::atomic` (3 tests):
contents replaced, parent directories created, no temp file left behind.

`test_config_example_toml_parses` pins `config.example.toml` against the real
schema, so a config-key rename cannot land without breaking a test.

**Untested behaviors:** none material.

### 2.13 `rookery-core::state` (state.rs)

**Current tests: 15** (0.1.8: 14)

Now covered: `StatePersistence::load()` on a missing file returning Stopped,
`profile_name()` and `is_running()` across all variants, `AgentPersistence`
save/load/reconcile plus missing-file, the Swapping state, and vLLM
reconciliation with and without a container id.

**Still untested:**
1. `is_process_alive()` with the `expected_exe` check via `/proc/pid/exe` — the
   PID-reuse guard. Dead-process reconciliation is tested, but not the case
   where the PID is alive and belongs to a *different* binary.

**Estimated new tests: 1–2**

### 2.14 `rookery-cli` (main.rs + client.rs + tests/exit_codes.rs)

**Current tests: 59** (0.1.8: 31) — 31 unit plus 28 integration in
`tests/exit_codes.rs`.

Now covered: clap parsing (all subcommands, global daemon flag, JSON flag,
logs follow and line count, invalid subcommand rejection), output formatting
(`format_count`, GPU, bench, agent status, profiles with backend prefixes and
ctx sizes), daemon-offline error handling including naming the probed URL, and
the full exit-code contract.

The exit-code contract is the significant new surface: usage errors exit 2 so
runtime failures can own 1; start/stop/swap/sleep/wake/agent-update/models-pull
all exit non-zero when the daemon reports failure rather than reporting success;
`--json` keeps printing parseable JSON on the error path and carries a shared
`error` key on every error path; and failures go to stderr rather than stdout.

**Still untested:**
1. Logs follow mode — SSE stream parsing on the client side

**Estimated new tests: 2–3**

---

## 3. Existing Test Dependencies

| Crate | Dev Dependencies |
|-------|-----------------|
| rookery-core | `tempfile` |
| rookery-engine | `tempfile`, `axum`, `tokio` |
| rookery-daemon | `tempfile`, `async-trait`, `tokio` (`test-util`), `tower` (`util`), `http-body-util` |
| rookery-cli | (none) |

The dependencies this document previously listed as missing are all in place.
The key insight held: no external test framework was needed. Route tests use
`tower::ServiceExt::oneshot()` against the real `Router`, with `http-body-util`
to build and read bodies, and `tokio`'s `test-util` feature supplies time
control.

`rookery-cli` still has no dev-dependencies — `tests/exit_codes.rs` drives the
built binary through `std::process::Command`.

---

## 4. Mock llama-server

**Built.** It lives at `crates/rookery-engine/src/test_utils.rs` — in-crate
rather than the `tests/common/` helper originally sketched, so both the engine's
own unit tests and the daemon's route tests can use it.

It has 9 tests of its own covering `/health`, `/v1/models`, `/props`, `/slots`,
`/v1/chat/completions`, configurable health delay, configurable
failure-after-N-requests, clean shutdown, and cleanup on drop.

This was the blocker for ProcessManager, backend start/health, and route
integration tests. Unblocking it is most of why the suite grew.

---

## 5. Flaky Test Assessment

### `test_is_pid_alive_parses_stat`

Fixed in commit `623ee92` — accepts R, S, and D states. Stable since.

### `test_cache_roundtrip`

Fixed in LAN-1151 — the test shared a temp directory with another test and could
observe the other's cache file. It now gets its own unique tempdir.

### Crash-detection test

Fixed in LAN-1147 — the test raced `start()`'s fsync latency against a timer. It
now kills the child explicitly rather than waiting a fixed interval.

Two of the three known flakes were fixed this release, both by removing a
shared resource or a timing assumption rather than by widening a tolerance.

---

## 6. Remaining Work, Ranked

### Tier 1: Real gaps worth closing
1. **Models route family** (5–8) — the only route group with no integration
   coverage, and `get_models_quants` carries real logic
2. **Daemon lifecycle** (4–6) — orphan cleanup, agent auto-start, full graceful
   shutdown
3. **End-to-end flow** — daemon startup → start → swap → agent lifecycle as a
   single test, the one roadmap item still fully open

### Tier 2: Narrow and cheap
4. **Watchdog timing** (3–4) — `depends_on_port` bounce, backoff reset; both
   need `tokio::time` control
5. **`attach_estimates`** (2–3) — pure logic
6. **CLI logs follow** (2–3) — SSE parsing
7. **`is_process_alive` PID-reuse guard** (1–2)

### Tier 3: Low value
8. **process.rs edge cases** (2) — OOM adj write failure, empty command line
9. **gpu.rs orphan detection** (1–2) — integration-gated, needs a GPU

---

## 7. Test Count by Area

Counts below are `#[test]` / `#[tokio::test]` attributes per file, comparing the
0.1.8 tag against current. They sum slightly under the harness totals (356 vs
358, 453 vs 457) because a few tests are generated rather than attributed
one-per-function; use the harness numbers for the headline figure.

| Area | 0.1.8 | Current | Notes |
|------|-------|---------|-------|
| backend.rs | 73 | 71 | −2: `subscribe_errors` tests removed with the trait method |
| routes.rs | 60 | 83 | +23, and 59 of the total are axum integration tests |
| agent.rs | 29 | 37 | adopt refusals, dead-agent health, pre-change backups |
| config.rs | 27 | 32 | validation matrix across both backends |
| cli (unit) | 31 | 31 | unchanged |
| cli (exit codes) | 0 | 28 | **new file** — `tests/exit_codes.rs` |
| process.rs | 22 | 20 | −2: `swap` / `start_and_wait` deleted with LAN-1087 |
| models.rs | 13 | 20 | hub scanning, recommend, repo scoping, download client |
| compose.rs | 18 | 20 | HF cache mount, optional param matrix |
| health.rs | 15 | 17 | served-model-name discovery |
| state.rs | 14 | 15 | vLLM reconcile |
| backup.rs | 0 | 11 | **new module** |
| releases.rs | 9 | 10 | connect-timeout bound |
| canary.rs | 10 | 10 | unchanged count; busy-slot guard replaced an older test |
| sse.rs | 7 | 9 | heartbeat naming, counter-leak guard |
| test_utils.rs | 9 | 9 | unchanged — the mock llama-server |
| integrity.rs | 0 | 8 | **new module** |
| auth.rs | 6 | 6 | unchanged |
| logs.rs | 5 | 5 | unchanged |
| main.rs (daemon) | 3 | 4 | reconcile liveness, shutdown lock |
| atomic.rs | 3 | 3 | unchanged |
| gpu.rs | 2 | 2 | unchanged |
| hardware.rs | 0 | 2 | **new module** — NVML unknown vs zero |
| **TOTAL (harness)** | **358** | **457** | |

The three genuinely new modules this release are `backup.rs`, `integrity.rs`,
and `hardware.rs`, plus the new `tests/exit_codes.rs` integration file. Those
four account for 49 of the 99 added tests; the rest is deepening on existing
modules, chiefly `routes.rs`.

---

## 8. Architectural Notes

### AppState for route tests

Route handlers take `State<Arc<AppState>>`. The `route_integration` module
builds one with a mock backend, a real `LogBuffer`, a `StatePersistence` on a
tempdir, a `broadcast::channel` for `state_tx`, and a `Mutex<()>` op_lock.
`GpuMonitor` is `None`, which is what
`test_route_hardware_vram_free_is_null_when_nvml_unavailable` asserts against.

### Canary extraction

Done. `crates/rookery-daemon/src/canary.rs` holds the loop, and its tests drive
it through `Box<dyn InferenceBackend>` without starting a daemon.

### Test organization

- `rookery-engine/src/test_utils.rs` — mock llama-server, shared across crates
- `rookery-daemon/src/routes.rs` → `tests::route_integration` — axum `oneshot`
- `rookery-cli/tests/exit_codes.rs` — spawns the built binary, asserts exit codes
- Everything else in each module's `#[cfg(test)] mod tests {}`

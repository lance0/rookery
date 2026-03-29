# Rookery Test Gap Analysis

**Date:** 2026-03-28
**Current test count:** 171 (14 CLI + 30 core + 23 daemon + 104 engine)
**Target:** 50–100 additional tests

---

## 1. Roadmap Test Plan (from ROADMAP.md § Code Quality & Testing)

The roadmap explicitly calls out these test gaps:

| Status | Item |
|--------|------|
| ✅ Done | `rookery-core`: config parsing, state serialization, reconciliation (now 30 tests) |
| ✅ Done | `rookery-engine`: log buffer, model utils, is_pid_alive, version parsing, AgentManager (now 104 tests) |
| ❌ TODO | `rookery-engine`: **ProcessManager start/stop/swap** (needs mock llama-server) |
| ❌ TODO | `rookery-engine`: **watchdog behavior** (crash restart, backoff, port recovery bounce) |
| ❌ TODO | `rookery-daemon`: **route handler integration tests** (axum test client) |
| ❌ TODO | `rookery-daemon`: **SSE event stream tests** |
| ❌ TODO | `rookery-cli`: **CLI argument parsing, output formatting** |
| ❌ TODO | **End-to-end**: daemon startup → start → swap → agent lifecycle |

---

## 2. Per-Module Analysis

### 2.1 `rookery-engine::process` (process.rs)

**Current tests: 4**
- `test_is_pid_alive_current_process` — verifies current PID is alive
- `test_is_pid_alive_nonexistent` — dead PID returns false
- `test_is_pid_alive_pid_1` — init process is alive
- `test_is_pid_alive_parses_stat` — parses /proc/self/stat correctly

**Untested behaviors:**
1. `ProcessManager::start()` — spawns child, sets OOM adj, captures stdout/stderr, returns ProcessInfo
2. `ProcessManager::stop()` — SIGTERM → 10s wait → SIGKILL for owned child
3. `ProcessManager::stop()` orphan path — kill-by-PID when no child handle (SIGTERM → poll → SIGKILL)
4. `ProcessManager::adopt()` — stores info, no child handle
5. `ProcessManager::is_running()` — try_wait on child, or is_pid_alive for adopted
6. `ProcessManager::to_server_state()` — maps process state to ServerState
7. `ProcessManager::swap()` — drain → stop → start_and_wait
8. `ProcessManager::start_and_wait()` — start + health check, transitions to Failed on timeout
9. CUDA error detection in stderr → triggers watch channel
10. OOM score adj write failure (non-root)
11. `start()` when already running returns error
12. Empty command line returns error

**Test types needed:** Mock-based unit tests using a **mock llama-server** (see §4).

**Estimated new tests: 12–15**

### 2.2 `rookery-engine::backend` (backend.rs)

**Current tests: 60** (largest test module)

Comprehensive coverage of:
- `InferenceBackend` trait object safety, Send+Sync
- `BackendInfo` serde roundtrips (llama-server + vllm)
- `LlamaServerBackend`: idle state, stop no-op, adopt, is_running, draining, subscribe_errors, start with real process, stop after adopt
- `VllmBackend`: idle state, container checks, compose file writing, docker compose commands, log capture, CUDA error detection
- `create_backend()` factory function
- Conversion helpers: `process_info_to_backend_info`, `backend_info_to_process_info`

**Untested behaviors:**
1. `LlamaServerBackend::start()` with health check success/failure (current test uses `/bin/sleep` which doesn't have health endpoint)
2. `VllmBackend::start()` full lifecycle (gated behind `ROOKERY_INTEGRATION=1`)
3. `VllmBackend::adopt()` with running container
4. `VllmBackend::stop()` error propagation on docker compose down failure (partially tested)
5. `is_cuda_error()` edge cases (partially covered — 7 tests exist)
6. Swap drain lifecycle across backend replacement

**Estimated new tests: 5–8** (mostly mock-based start/health interaction)

### 2.3 `rookery-engine::health` (health.rs)

**Current tests: 0**

Three public functions, all untested:
1. `wait_for_health(port, timeout)` — exponential backoff polling until 200 or timeout
2. `check_health(port, timeout)` — single-shot health check, returns bool
3. `check_inference(port, timeout)` — sends minimal completion request, returns bool

**Untested behaviors:**
1. `wait_for_health` succeeds when server responds 200
2. `wait_for_health` times out and returns `HealthError::Timeout`
3. `wait_for_health` exponential backoff timing (100ms → 200ms → ... → 5s cap)
4. `check_health` returns true on 200, false on non-200, false on connection refused
5. `check_inference` returns true on 200, false on non-200, false on timeout
6. `HealthError` display messages

**Test types needed:** Mock HTTP server (use `axum` itself or `wiremock` or inline `hyper` server).

**Estimated new tests: 8–10**

### 2.4 `rookery-engine::agent` (agent.rs)

**Current tests: 11**
- `test_read_version_pyproject`, `_poetry`, `_cargo`, `_missing_file`, `_no_version_field` (5 version parsing tests)
- `test_agent_manager_start_stop` — start + stop lifecycle
- `test_agent_manager_already_running` — duplicate start error
- `test_agent_manager_get_health` — health metrics
- `test_agent_manager_remove_tracking` — untrack without signal
- `test_agent_manager_record_restart` — restart counter tracking
- `test_agent_fatal_error_pattern_detection` — stderr pattern triggers watch
- `test_agent_no_false_fatal_trigger` — non-matching pattern doesn't trigger

**Untested behaviors:**
1. `adopt()` — registers existing PID, no child handle
2. `stop()` orphan path — kill-by-PID for adopted agents
3. `stop_all()` — stops all agents
4. `list()` — returns info with dead agent cleanup
5. `persist_state()` — writes agents.json
6. Crash count / exponential backoff in `spawn_watchdog()`
7. Watchdog `depends_on_port` bounce logic (down→up transition)
8. Watchdog healthy agent backoff reset after 5min
9. `is_running()` for adopted (PID check) vs owned (try_wait)
10. Agent env var passing
11. Agent workdir setting

**Estimated new tests: 8–12**

### 2.5 `rookery-engine::gpu` (gpu.rs)

**Current tests: 0**

NVML-dependent, difficult to unit test without GPU:
- `GpuMonitor::new()` — requires NVML
- `GpuMonitor::stats()` — reads GPU stats
- `GpuMonitor::find_orphan_llama_servers()` — finds untracked processes
- `process_name()` — reads /proc/pid/comm

**Testable without NVML:**
1. `process_name()` — can test with current process PID
2. `find_orphan_llama_servers()` logic (would need to mock stats())

**Estimated new tests: 1–2** (process_name, integration-gated GPU tests)

### 2.6 `rookery-engine::logs` (logs.rs)

**Current tests: 2**
- `test_ring_buffer` — eviction at capacity
- `test_last_n` — last N lines retrieval

**Untested behaviors:**
1. `subscribe()` — returns broadcast receiver
2. `len()` / `is_empty()` — size queries
3. Concurrent push from multiple tasks
4. Poison recovery (`unwrap_or_else(|e| e.into_inner())`)
5. Broadcast receiver receives pushed messages

**Estimated new tests: 3–4**

### 2.7 `rookery-engine::compose` (compose.rs)

**Current tests: ~17** (I count 17 compose::tests:: in the output)
- Compose file path, YAML generation, GPU reservation, port mapping, model args, extra args, full config, error cases

**Good coverage. Untested:**
1. Edge case: compose with `max_model_len` field
2. Compose with all optional vLLM params set simultaneously

**Estimated new tests: 1–2**

### 2.8 `rookery-engine::models` (models.rs)

**Current tests: 4**
- `test_cache_path`, `test_extract_quant_label`, `test_normalize_repo`, `test_extract_quants`

**Untested behaviors:**
1. `HfClient::search()`, `list_files()`, `download_file()` — network-dependent
2. `scan_cache()` — filesystem scan
3. `recommend_quant()` — VRAM-aware selection logic
4. `mark_downloaded()` — checks HF cache
5. `attach_estimates()` — performance estimation
6. Quant preference ordering (UD variants first)

**Estimated new tests: 5–8** (recommend_quant is pure logic, very testable)

### 2.9 `rookery-daemon::routes` (routes.rs)

**Current tests: 23**
- Status response formatting (stopped/running/starting/stopping/failed) with backend field
- Status JSON always has backend key
- Profiles response includes backend field
- Capacity gate skips vLLM
- Swap drain flag cleanup on failure/success
- Compose generation failure returns error
- Model info response with/without props
- Server stats response with/without slots
- Start failure transitions to Failed state
- HTTP status check logic

**Untested behaviors (need axum test client):**
1. `get_status` route handler — full request/response cycle
2. `post_start` — with real AppState, config, backend mock
3. `post_stop` — with real AppState
4. `post_swap` — drain → stop → new backend → start → health
5. `get_profiles` — JSON response structure
6. `get_health` — always returns 200
7. `get_config` — redaction of agent env vars
8. `put_profile` — sampling param updates, 404 on missing profile
9. `get_model_info` — proxy to llama-server /v1/models and /props
10. `post_chat` — rejects during drain (503), proxy passthrough, stream timeout
11. `get_bench` — proxy to llama-server, result formatting
12. `get_logs` — returns last N lines
13. `get_agents`, `post_agent_start`, `post_agent_stop`, `get_agent_health`
14. `get_dashboard` — serves embedded HTML, SPA fallback
15. `get_models_search`, `get_models_quants`, `get_models_recommend`, `get_models_cached`, `post_models_pull`
16. `get_hardware` — hardware profile with live data
17. SSE connection limit (429 on overflow)
18. SSE initial state event
19. Body size limit enforcement (1MB)

**Estimated new tests: 15–25** (route integration tests with axum test client)

### 2.10 `rookery-daemon::main` (main.rs) — Canary/Watchdog

**Current tests: 0** (all logic is in `main()`, no extractable test functions)

**Testable behaviors (would need extraction):**
1. **Inference canary loop**: periodic check, retry-once, restart on double failure
2. **Canary CUDA error trigger**: cuda_error_rx.changed() → immediate canary
3. **Canary skip during drain**: draining=true → continue without checking
4. **Canary op_lock serialization**: canary restart acquires op_lock
5. **Canary re-subscribe**: after swap, canary re-subscribes to new backend error channel
6. **Orphan cleanup**: find_orphan_llama_servers → SIGTERM → wait → SIGKILL
7. **State reconciliation**: load → reconcile → adopt or mark stopped
8. **Agent reconciliation**: load → reconcile → adopt → bounce restart_on_swap agents
9. **Auto-start agents**: agents with auto_start=true start on daemon init
10. **Graceful shutdown**: SIGTERM → stop agents → stop server → persist Stopped

**Test types needed:** Extract canary logic into a testable function, or test via integration tests.

**Estimated new tests: 8–12** (after refactoring canary into testable unit)

### 2.11 `rookery-daemon::sse` (sse.rs)

**Current tests: 0**

**Untested behaviors:**
1. SSE merges GPU, state, and log streams
2. SSE connection limit (MAX_SSE_CONNECTIONS = 16)
3. SSE sends initial state event on connect
4. SSE keep-alive interval (15s)
5. Connection count decrement on disconnect

**Estimated new tests: 3–5** (with axum test client)

### 2.12 `rookery-core::config` (config.rs)

**Current tests: 22**

Excellent coverage of TOML parsing, backend types, validation, serialization roundtrips, command line resolution. 

**Untested:**
1. `Config::load()` — reads from filesystem (integration)
2. `Config::save()` — atomic write (tempfile + rename)
3. `Config::validate()` — missing default_profile error
4. Edge case: model with both `path` and `repo` fields
5. `resolve_profile_name()` — returns default when None

**Estimated new tests: 3–5**

### 2.13 `rookery-core::state` (state.rs)

**Current tests: 8**

Good coverage of serde roundtrip, reconciliation, backend fields, backward compat.

**Untested:**
1. `StatePersistence::load()` when file doesn't exist → returns Stopped
2. `ServerState::profile_name()` for all variants
3. `ServerState::is_running()` — false for non-Running variants
4. `AgentPersistence` save/load/reconcile
5. `is_process_alive()` with expected_exe check via /proc/pid/exe

**Estimated new tests: 4–6**

### 2.14 `rookery-cli` (main.rs + client.rs)

**Current tests: 14**

All tests are StatusResponse/profile display formatting and JSON backward compat. No tests for:
1. **CLI argument parsing** — clap derive correctness
2. **DaemonClient** — HTTP request/response handling
3. **Output formatting** — `format_count()`, GPU display, bench display, agent status display
4. **Error handling** — daemon offline detection, HTTP errors
5. **Logs follow mode** — SSE parsing

**Estimated new tests: 8–12**

---

## 3. Existing Test Dependencies

| Crate | Dev Dependencies |
|-------|-----------------|
| rookery-core | `tempfile = "3"` |
| rookery-engine | `tempfile = "3"` |
| rookery-daemon | (none) |
| rookery-cli | (none) |

### Missing Dependencies Needed

| Dependency | Purpose | Crate |
|-----------|---------|-------|
| `axum::test` (built-in) | Route handler testing via `axum::Router::oneshot()` | rookery-daemon |
| `tower::ServiceExt` | `.oneshot()` helper for axum testing | rookery-daemon |
| `hyper` | Building test requests for axum | rookery-daemon |
| `tokio-test` (optional) | Async test utilities | rookery-engine |
| (none needed) | Mock llama-server can be built with axum itself | rookery-engine |

**Key insight:** Axum has built-in testing support via `Router::into_service()` + `tower::ServiceExt::oneshot()`. No external test framework like `axum-test` is needed. The daemon already depends on `axum` and `tower-http`.

---

## 4. Mock llama-server Design

A mock llama-server is the key enabler for ProcessManager and route integration tests.

### What it needs to implement:
```
GET  /health          → 200 OK (or configurable delay/failure)
GET  /v1/models       → {"data":[{"id":"test-model","owned_by":"test"}]}
GET  /props           → {"total_slots":1,"chat_template":"..."}
GET  /slots           → [{"id":0,"state":"idle"}]
POST /v1/chat/completions → {"choices":[...],"timings":{...}}
```

### Implementation approach:
- Small axum server that binds to a random available port
- Configurable behaviors: health delay, failure after N requests, slow responses
- Returns realistic JSON responses matching llama-server's format
- Can be a shared test utility in `rookery-engine/src/test_utils.rs` or a `tests/` helper

### Shared test utilities:
```rust
// rookery-engine/tests/common/mock_server.rs
pub struct MockLlamaServer {
    port: u16,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    join_handle: tokio::task::JoinHandle<()>,
}

impl MockLlamaServer {
    pub async fn start() -> Self { ... }
    pub fn port(&self) -> u16 { ... }
    pub async fn shutdown(self) { ... }
}
```

---

## 5. Flaky Test Assessment

### `test_is_pid_alive_parses_stat`

**Status:** Fixed in commit `623ee92` — now accepts R, S, and D states.

```rust
assert!(
    matches!(state, 'R' | 'S' | 'D'),
    "test process should be alive (R/S/D), got '{state}'"
);
```

This is now stable. The fix was correct — the test process can be in Running, Sleeping, or Disk sleep states, all of which are valid "alive" states.

---

## 6. Priority Ranking for New Tests

### Tier 1: High Impact, Moderate Effort (should do first)
1. **Health check tests** (8–10) — zero tests today, used everywhere
2. **Route integration tests** (15–25) — zero real handler tests, high risk surface
3. **ProcessManager lifecycle** (12–15) — needs mock server but core functionality

### Tier 2: Medium Impact, Lower Effort
4. **AgentManager gaps** (8–12) — adopt, watchdog, crash backoff
5. **SSE tests** (3–5) — connection limits, stream merging
6. **CLI argument/output tests** (8–12) — clap parsing, display formatting
7. **Canary behavior** (8–12) — needs extraction from main.rs

### Tier 3: Incremental Value
8. **LogBuffer subscribe/concurrent** (3–4)
9. **Config edge cases** (3–5)
10. **State persistence edge cases** (4–6)
11. **Models recommend logic** (5–8)
12. **GPU process_name** (1–2)

---

## 7. Estimated Test Count by Area

| Area | Current | New Tests | Notes |
|------|---------|-----------|-------|
| health.rs | 0 | 8–10 | Mock HTTP server |
| routes.rs (integration) | 23 | 15–25 | axum oneshot + AppState mock |
| process.rs | 4 | 12–15 | Mock llama-server |
| agent.rs | 11 | 8–12 | Adopt, watchdog, backoff |
| sse.rs | 0 | 3–5 | axum test client |
| canary (main.rs) | 0 | 8–12 | Extract to testable fn |
| CLI | 14 | 8–12 | Clap parsing, formatting |
| backend.rs | 60 | 5–8 | Start+health interaction |
| logs.rs | 2 | 3–4 | Subscribe, concurrent |
| config.rs | 22 | 3–5 | Edge cases |
| state.rs | 8 | 4–6 | Agent persistence |
| models.rs | 4 | 5–8 | recommend_quant logic |
| compose.rs | 17 | 1–2 | Edge cases |
| gpu.rs | 0 | 1–2 | process_name |
| **TOTAL** | **171** | **84–126** | |

---

## 8. Architectural Notes

### AppState for Route Tests
Route handlers take `State<Arc<AppState>>`. Testing them requires constructing an AppState with:
- A mock backend (`Box<dyn InferenceBackend>`)
- A real `LogBuffer`
- A `StatePersistence` pointed at a tempdir
- A `broadcast::channel` for state_tx
- A `Mutex<()>` for op_lock
- Optional: mock GpuMonitor (None is fine for most tests)

### Canary Extraction
The canary logic in `main.rs` is a ~60-line `tokio::spawn` block. It should be extracted to:
```rust
pub async fn run_canary(
    backend: Arc<Mutex<Box<dyn InferenceBackend>>>,
    state_persistence: &StatePersistence,
    config: Arc<RwLock<Config>>,
    op_lock: &Mutex<()>,
) { ... }
```
This makes it testable without starting the full daemon.

### Test Organization
- `rookery-engine/tests/common/mod.rs` — shared mock server, test config builders
- `rookery-daemon/tests/routes_test.rs` — integration tests with axum oneshot
- Each module's `#[cfg(test)] mod tests {}` for unit tests

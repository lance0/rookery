---
name: rust-backend
description: Implements Rust backend features across rookery crates (core, engine, daemon, cli)
---

# Rust Backend Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

Use this skill when the task involves:
- Adding or modifying Rust code in any of the four workspace crates (`rookery-core`, `rookery-engine`, `rookery-daemon`, `rookery-cli`)
- Implementing new features that span config types, engine logic, daemon routes, or CLI commands
- Refactoring existing Rust code (state machine, config parsing, backend trait, routes)
- Adding or updating serde serialization/deserialization
- Modifying the `InferenceBackend` trait or its implementations
- Adding new daemon API endpoints or CLI subcommands

Do **not** use this skill for:
- Dashboard/frontend-only changes (HTML, CSS, JS, Leptos/WASM)
- Documentation-only changes
- Config file changes that don't touch Rust code
- Shell scripts or CI/CD pipeline work

## Required Skills

None

## Work Procedure

### 1. Understand the Feature

Read the feature description carefully. Identify which crates need changes:

| Change type | Crate |
|---|---|
| New config fields, types, enums, error variants | `rookery-core` (`crates/rookery-core/src/`) |
| Backend logic, process/container management, GPU, agents, health, logs | `rookery-engine` (`crates/rookery-engine/src/`) |
| HTTP routes, SSE, AppState, orchestration, dashboard embedding | `rookery-daemon` (`crates/rookery-daemon/src/`) |
| CLI commands, daemon HTTP client | `rookery-cli` (`crates/rookery-cli/src/`) |

Map out which files will be touched before writing any code.

### 2. Read Project Constraints

- Read `CLAUDE.md` for build commands, crate layout, and key patterns.
- Read `.factory/library/architecture.md` for system design, component relationships, data flows, and invariants.

Pay special attention to:
- **State machine**: `Stopped → Starting → Running → Stopping → Failed` — transitions must be correct.
- **Op lock**: start/stop/swap are serialized via `op_lock`.
- **Atomic saves**: state persistence uses write-to-tmp + rename.
- **One backend at a time**: the daemon holds a single `Box<dyn InferenceBackend>`.
- **Config determines backend type**: profile sub-tables (`llama_server` or `vllm`) are the source of truth.

### 3. Read Relevant Source Files

Before writing any code, read the source files that will be modified. Understand:
- Existing struct fields and their serde attributes
- How the module's tests are structured (inline `#[cfg(test)] mod tests`)
- Import patterns and error handling conventions (`use crate::error::{Error, Result}`)
- Whether async code uses `#[tokio::test]` or sync `#[test]`

### 4. Write Tests (TDD where applicable)

For **new features**: Write failing tests first (TDD red-green). Add tests in the appropriate crate's inline test module (`#[cfg(test)] mod tests { ... }`), confirm they fail, then implement to make them pass.

For **refactoring or fix features** (behavior-preserving changes): Existing tests serve as the primary regression guard. Add targeted tests for the specific behavior being fixed or changed. The strict "fail first" requirement is relaxed — you may implement the fix and add tests together when the change is behavior-preserving and existing tests already cover the happy path.

Conventions:
- Test function names: `test_<feature>_<scenario>` (e.g., `test_config_parse_vllm_profile`)
- Async tests use `#[tokio::test]`
- Sync tests use `#[test]`
- `rookery-engine` has `tempfile = "3"` as a dev dependency for filesystem tests
- Construct test data inline — do not rely on external fixture files
- Test both success paths and error paths
- **When changing API response shapes**: add tests verifying JSON structure for all states (Running, Stopped, Failed, Starting, Stopping)

Run `cargo test --workspace` to confirm tests pass after implementation.

### 5. Implement the Feature

Write the minimal code to make all tests pass. Follow existing patterns:

- **rookery-core**: `#[derive(Debug, Clone, Serialize, Deserialize)]` on all types. Use `#[serde(default)]` for optional fields. Validation logic lives in `Config::validate()`.
- **rookery-engine**: Backend implementations use `#[async_trait]` for the `InferenceBackend` trait. Process management uses `nix` for signals. Log capture uses `LogBuffer` broadcast channels.
- **rookery-daemon**: Routes are async functions taking `State<AppState>` and return `impl IntoResponse`. Use the `op_lock` for state-mutating operations. Broadcast state changes via the state channel.
- **rookery-cli**: Commands use clap derive macros. The `DaemonClient` makes HTTP requests to the daemon.

### 6. Run Tests

```bash
cargo test --workspace
```

All tests must pass — both new and existing. If any test fails, fix the implementation before proceeding.

### 7. Run Clippy

```bash
cargo clippy --workspace
```

Zero warnings required. Fix any lints before proceeding. Common issues:
- Unused imports or variables
- Missing `pub` or unnecessary `pub`
- `clone()` on `Copy` types
- Needless `return` statements

### 8. Run Release Build

```bash
cargo build --release
```

Must compile cleanly. This catches issues that debug builds may miss (e.g., unused code behind `#[cfg]` gates).

### 9. Manual Verification

Describe what was verified beyond automated tests:
- For config changes: show a sample TOML snippet that parses correctly
- For API changes: describe the request/response shape
- For state machine changes: trace the state transitions that occur
- For CLI changes: show the help output or example invocation

### 10. Commit

Write a descriptive commit message:
```
<what changed>: <concise summary>

- Detail 1
- Detail 2
```

Example: `config: add vllm profile sub-table parsing`

## Example Handoff

**Feature**: Refactor config validation to reject profiles with both `llama_server` and `vllm` sub-tables.

**Crates affected**: `rookery-core`

**Tests written** (in `crates/rookery-core/src/config.rs`, inline test module):
- `test_config_validate_rejects_dual_backend_profile` — profile with both sub-tables returns `Error::ConfigValidation`
- `test_config_validate_rejects_no_backend_profile` — profile with neither sub-table returns `Error::ConfigValidation`
- `test_config_validate_accepts_llama_server_only` — profile with only `llama_server` sub-table passes
- `test_config_validate_accepts_vllm_only` — profile with only `vllm` sub-table passes

**Commands run**:
```
$ cargo test --workspace
   Compiling rookery-core v0.1.0
   ...
   running 4 tests
   test config::tests::test_config_validate_rejects_dual_backend_profile ... FAILED  (expected — no validation yet)
   ...

(implemented validation in Config::validate())

$ cargo test --workspace
   running 26 tests
   ...
   test result: ok. 26 passed; 0 failed

$ cargo clippy --workspace
   Checking rookery-core v0.1.0
   Checking rookery-engine v0.1.0
   Checking rookery-daemon v0.1.0
   Checking rookery-cli v0.1.0
   Finished

$ cargo build --release
   Compiling rookery-core v0.1.0
   ...
   Finished `release` profile
```

**Manual verification**: Tested with a hand-crafted TOML containing a profile with both `[profiles.bad.llama_server]` and `[profiles.bad.vllm]` sub-tables. Confirmed the daemon logs `Config validation error: profile 'bad' has both llama_server and vllm sub-tables` and exits with code 1.

**Commit**: `config: reject profiles with both or neither backend sub-table`

## When to Return to Orchestrator

Return when:
- All tests pass (`cargo test --workspace`)
- Clippy is clean (`cargo clippy --workspace`)
- Release build succeeds (`cargo build --release`)
- Changes are committed with a descriptive message
- A summary of what was implemented, tested, and verified is ready

Return early (with explanation) if:
- The feature requires changes to crates or files outside the Rust workspace
- The feature description is ambiguous and multiple interpretations lead to different implementations
- A dependency is missing and needs to be added to `Cargo.toml` (report which dependency and why)
- An existing test fails in a way unrelated to the current feature

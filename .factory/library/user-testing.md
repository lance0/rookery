# User Testing

Testing surface, required testing skills/tools, and resource cost classification.

## Validation Surface

**Primary surface:** CLI binary output + cargo test suite
- Tool: `cargo test --workspace`, `cargo build --release`
- All behavioral assertions testable via unit tests
- No running daemon required for automated validation

**Secondary surface:** Dashboard compilation
- Tool: `trunk build --release` (in crates/rookery-dashboard/)
- Verifies WASM compiles, not visual correctness
- Visual verification is manual (deferred to user)

**Manual surface:** End-to-end daemon testing
- Requires stopping running daemon, starting new binary
- Not part of automated validation
- User performs after mission completion

## Validation Concurrency

**cargo test:** Single process, parallel test execution managed by cargo. Max concurrent: unlimited (tests are lightweight). Machine has 64 cores and 125GB RAM — no resource concern.

**trunk build:** Single build, not parallelizable. ~30s build time.

## Testing Tools
- cargo test (Rust unit tests)
- cargo clippy (linting)
- trunk build (WASM compilation)
- No browser testing tools needed for automated validation

## Flow Validator Guidance: cargo-cli-validation

- Isolation boundary: shared repository checkout at `/home/lance/projects/rookery`; no separate data dirs needed.
- Do not start/stop `rookeryd` or `llama-server`; validation is compile/test-only.
- Run validators serially across subagents (`max concurrent = 1`) to avoid `cargo` target-dir lock contention.
- Allowed commands: `cargo test --workspace`, `cargo clippy --workspace`, `cargo build --release`, and dashboard `trunk build --release`.
- Write flow reports to `.factory/validation/backend-abstraction/user-testing/flows/*.json`.
- When filtering tests, prefer substring filters unless the full test path is known; `--exact` with partial names can execute zero tests.

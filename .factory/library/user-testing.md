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

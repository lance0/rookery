# Contributing to Rookery

## Development Setup

### Prerequisites

- Rust 1.88+ (stable)
- NVIDIA GPU with CUDA drivers (for NVML)
- [llama.cpp](https://github.com/ggml-org/llama.cpp) built with CUDA
- [Trunk](https://trunkrs.dev/) for dashboard: `cargo install trunk`
- wasm32 target: `rustup target add wasm32-unknown-unknown`

### Enable the git hooks

The pre-commit hook lives in `.githooks/` but git will not use it until you point
at that directory. This is local config and cannot be committed, so a fresh clone
has no hooks until you run:

```bash
git config core.hooksPath .githooks
```

### Build

```bash
# Backend (daemon + CLI)
cargo build --release

# Dashboard (optional — only needed if modifying frontend)
cd crates/rookery-dashboard && trunk build --release && cd ../..

# Re-embed dashboard into daemon
touch crates/rookery-daemon/src/routes.rs
cargo build --release -p rookery-daemon
```

### Run Tests

```bash
make test                      # canonical command — runs cargo test --workspace
cargo clippy --all-targets     # zero warnings enforced
cargo fmt --all --check        # formatting enforced
```

Tests use mock backends and temp directories — they never touch your real config or running daemon.

### Run Locally

```bash
mkdir -p ~/.config/rookery
cp config.example.toml ~/.config/rookery/config.toml
# Edit config: set llama_server path, configure a model + profile

./target/release/rookeryd       # start daemon
./target/release/rookery status # use CLI
```

## Code Style

- `cargo fmt` and `cargo clippy` are enforced by pre-commit hooks and CI
- No warnings allowed — `clippy --all-targets` must be clean
- Prefer `Edit` tool over `sed` for file modifications (this is in our muscle memory after some incidents)

**`rookery-dashboard` is excluded from the workspace**, so none of the commands
above touch it — the root-level `cargo fmt --all` and `cargo clippy --all-targets`
silently skip that crate (this is how it went unformatted for months). Format
and lint it explicitly, targeting `wasm32-unknown-unknown`:

```bash
cargo fmt --manifest-path crates/rookery-dashboard/Cargo.toml --check
cargo clippy --manifest-path crates/rookery-dashboard/Cargo.toml \
  --target wasm32-unknown-unknown --all-targets -- -D warnings
cd crates/rookery-dashboard && trunk build --release
```

## Project Structure

```
crates/
  rookery-core/       # Config parsing, state machine, shared types
  rookery-engine/     # Process manager, GPU monitor, health checker, agent manager
  rookery-daemon/     # Axum REST API, SSE, auth middleware, embedded dashboard
  rookery-dashboard/  # Leptos WASM frontend (separate build via trunk)
  rookery-cli/        # Clap CLI client
```

- `rookery-core` has no async dependencies — pure config + state
- `rookery-engine` owns all process and hardware interaction
- `rookery-daemon` wires everything together with axum routes
- `rookery-dashboard` is excluded from the cargo workspace (WASM target)
- `rookery-cli` is a thin HTTP client

## Adding Features

### New API endpoint
1. Add the handler in `crates/rookery-daemon/src/routes.rs`
2. Register the route in `crates/rookery-daemon/src/main.rs`
3. Add a test in the `route_integration` test module
4. Add CLI support in `crates/rookery-cli/src/main.rs`
5. Add dashboard API call in `crates/rookery-dashboard/src/api.rs`

### New config field
1. Add to the struct in `crates/rookery-core/src/config.rs` with `#[serde(default)]`
2. Add to `config.example.toml`
3. Add `field: value` to ALL test Config constructors (search for `agents: HashMap::new()`)
4. Document in `docs/configuration.md`

### New dashboard component
1. Create `crates/rookery-dashboard/src/components/my_component.rs`
2. Register in `crates/rookery-dashboard/src/components/mod.rs`
3. Wire into the appropriate tab in `crates/rookery-dashboard/src/main.rs`
4. Rebuild: `cd crates/rookery-dashboard && trunk build --release`

## Pull Requests

1. Fork the repo and create a feature branch
2. Make your changes with tests
3. Ensure `cargo test --workspace && cargo clippy --all-targets && cargo fmt --all --check` pass
4. Submit a PR with a clear description of what and why

## Questions?

Open an issue on [GitHub](https://github.com/lance0/rookery/issues).

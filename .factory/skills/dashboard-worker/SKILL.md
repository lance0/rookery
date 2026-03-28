---
name: dashboard-worker
description: Implements Leptos WASM dashboard changes for the Rookery web UI
---

# Dashboard Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

Use this skill when implementing UI changes to the Rookery dashboard — a Leptos 0.7 WASM single-page app that lives in `crates/rookery-dashboard/`. This includes:

- Adding or modifying data types displayed in the UI (`ServerStatus`, `ProfileInfo`, `GpuStats`, etc. in `main.rs`)
- Creating or updating Leptos components in `crates/rookery-dashboard/src/components/`
- Adding new API client functions in `crates/rookery-dashboard/src/api.rs`
- Changing SSE event handling or signal wiring in the `App` component
- Styling changes via `crates/rookery-dashboard/style.css`
- Adding graceful degradation for backend-specific features (e.g., endpoints only available on llama-server but not vLLM)

Do NOT use this skill for daemon-side route changes (`rookery-daemon`), CLI changes (`rookery-cli`), or core config/state logic (`rookery-core`).

## Required Skills

None

## Work Procedure

### 1. Understand the Feature

Read the feature description from the orchestrator handoff. Identify:
- Which data types need new or modified fields
- Which components render the affected data
- Whether new API endpoints or SSE events are involved
- Whether the feature requires graceful degradation (e.g., a backend may not support the endpoint)

### 2. Read Existing Code and Patterns

Study the relevant source files to understand current conventions:

- **Data types**: `crates/rookery-dashboard/src/main.rs` — all shared structs (`ServerStatus`, `ProfileInfo`, `GpuStats`, `AgentInfo`, `ModelInfoData`, etc.) and the `App` component with signal wiring and SSE handling.
- **API client**: `crates/rookery-dashboard/src/api.rs` — async functions using `gloo_net::http::Request` for each daemon endpoint. Follow the existing pattern: `Request::get(...).send().await` → `.json().await`, returning `Result<T, String>`.
- **Components**: `crates/rookery-dashboard/src/components/` — each component is a separate file re-exported via `mod.rs`. Components use `#[component]` macro, receive `ReadSignal<T>` / `WriteSignal<T>` props, and use `move || { ... }` closures for reactive rendering.
- **Component registry**: `crates/rookery-dashboard/src/components/mod.rs` — ensure any new component is declared and re-exported here.
- **Styles**: `crates/rookery-dashboard/style.css` — CSS custom properties on `:root` and `.light`, component-scoped classes.
- **HTML shell**: `crates/rookery-dashboard/index.html` — the Trunk HTML template (rarely needs changes).
- **Build config**: `crates/rookery-dashboard/Trunk.toml` — Trunk build settings.

Key Leptos patterns used in this codebase:
- Signals: `let (value, set_value) = signal(Default::default());`
- Async spawns: `wasm_bindgen_futures::spawn_local(async move { ... });`
- Toast notifications: `show_toast(set_toasts, msg, ToastKind::Success)` / `ToastKind::Error`
- Conditional rendering: `move || if condition { view! { ... }.into_any() } else { view! { ... }.into_any() }`
- `#[serde(default)]` on optional fields for backward compatibility with older daemon versions

### 3. Modify Data Types (if needed)

Edit `crates/rookery-dashboard/src/main.rs`:
- Add new fields to the relevant struct(s)
- Use `Option<T>` with `#[serde(default)]` for fields that may be absent (graceful degradation)
- Keep `Serialize, Deserialize` derives intact

Example — adding a `backend` field:
```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerStatus {
    pub state: String,
    pub profile: Option<String>,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub uptime_secs: Option<i64>,
    #[serde(default)]
    pub backend: Option<String>,  // "llama-server" or "vllm"
}
```

### 4. Modify Components

Edit files in `crates/rookery-dashboard/src/components/`:
- Follow existing signal/prop patterns — components receive `ReadSignal<T>` and `WriteSignal<T>` as props
- Use reactive closures (`move || { ... }`) for derived values
- For conditional UI elements (e.g., showing a badge only when a field is `Some`), use `if let` or `.map()` inside `move ||` closures
- For backend-specific features, check the backend field and hide/disable UI elements that don't apply
- When adding a new component file, register it in `components/mod.rs`

Example — adding a backend badge to `StatusCard`:
```rust
let backend_text = move || {
    status.get().backend.clone().unwrap_or_default()
};
// In the view:
// <span class="badge backend">{backend_text}</span>
```

### 5. Add API Functions (if needed)

Edit `crates/rookery-dashboard/src/api.rs`:
- Follow the existing pattern for GET or POST requests
- Return `Result<T, String>` where `T` is the deserialized response type
- For endpoints that may not exist on all backends, handle HTTP errors gracefully in the calling component (not in the API function)

### 6. Build and Verify — Dashboard (trunk)

```bash
cd crates/rookery-dashboard && trunk build --release
```

This compiles the Leptos app to WASM and outputs to `crates/rookery-dashboard/dist/`. The build must succeed with no errors. Warnings from `wasm_bindgen` or `unused` are acceptable but Rust compilation errors are not.

**Troubleshooting**:
- If `trunk` is not found: check `~/.cargo/bin/trunk` or install with `cargo install trunk`
- If `wasm32-unknown-unknown` target is missing: `rustup target add wasm32-unknown-unknown`
- If a Leptos macro error occurs: check that `#[component]` function signatures match Leptos 0.7 conventions (signals, not `Scope`)

### 7. Build and Verify — Daemon (cargo)

```bash
cargo build --release
```

Run from the project root. This builds the daemon which embeds the dashboard via:
```rust
static DASHBOARD_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../rookery-dashboard/dist");
```

The daemon build must succeed. If the `dist/` directory is missing or stale, the daemon will embed whatever was last built — so always run trunk build first.

### 8. Commit

Commit all changed files with a descriptive message covering:
- What UI elements were added/changed
- Which data types were modified
- Any graceful degradation logic added

## Example Handoff

**Feature**: Add backend badge to dashboard — show whether the active profile uses `llama-server` or `vllm` on the status card and profile switcher.

**Work performed**:

1. Modified `crates/rookery-dashboard/src/main.rs`:
   - Added `backend: Option<String>` with `#[serde(default)]` to `ServerStatus`
   - Added `backend: Option<String>` with `#[serde(default)]` to `ProfileInfo`

2. Modified `crates/rookery-dashboard/src/components/status_card.rs`:
   - Added backend badge next to the state badge, showing "llama" or "vllm"
   - Badge only renders when `backend` is `Some`

3. Modified `crates/rookery-dashboard/src/components/profile_switcher.rs`:
   - Added small backend label to each profile card's metadata line
   - Shows "llama" or "vllm" after the model name

4. No changes to `api.rs` — backend field comes from existing `/api/status` and `/api/profiles` responses.

5. Builds:
   - `cd crates/rookery-dashboard && trunk build --release` — ✅ success
   - `cargo build --release` — ✅ success (daemon embeds updated dist/)

6. Committed: `feat(dashboard): show backend badge on status card and profile switcher`

**Files changed**:
- `crates/rookery-dashboard/src/main.rs`
- `crates/rookery-dashboard/src/components/status_card.rs`
- `crates/rookery-dashboard/src/components/profile_switcher.rs`

## When to Return to Orchestrator

Return when:
- All requested UI changes are implemented
- `trunk build --release` succeeds
- `cargo build --release` succeeds (daemon embeds the updated dashboard)
- Changes are committed

Report back with: files changed, build outputs (success/failure), and any issues encountered. Visual verification (actually viewing the dashboard in a browser) is deferred to the user — note this in the handoff.

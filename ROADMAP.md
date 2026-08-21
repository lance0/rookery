# Rookery Roadmap

What is still open. Shipped work is not listed here — see [CHANGELOG.md](CHANGELOG.md) for what landed and when, and [README.md](README.md) for what the current release actually does.

The foundations are all in place and are no longer tracked as roadmap items: the daemon + CLI split, the state machine with reconciliation and orphan adoption, hot-swap with drain, config reload, the Leptos dashboard and its SSE stream, agent management with the watchdog and crash backoff, model discovery and download, the `InferenceBackend` trait with llama-server and vLLM implementations, Prometheus metrics, auto-sleep, API key auth, the upstream release monitor, and the packaging/install path.

## Dashboard

- [ ] Responsive layout for the tabbed UI — the tabs are usable on a phone but not designed for one
- [ ] Touch-friendly controls for GPU gauges, profile switcher, agent controls
- [ ] Stack GPU gauges vertically on narrow screens; hamburger nav below the tab-strip breakpoint
- [ ] Check keyboard shortcuts do not fight mobile gestures
- [ ] KV cache usage gauge (blocked on the llama-server `--metrics` work below)
- [ ] Backend-specific stats: vLLM batch utilization and KV cache usage from its `/metrics`
- [ ] Release notes preview in the upstream monitor (expandable, rendered markdown)
- [ ] Optional one-click rebuild trigger for llama.cpp

## Observability

- [ ] Enable `--metrics` on llama-server and parse KV cache usage ratio + throughput
- [ ] Grafana dashboard template (JSON import) — GPU gauges, request throughput, error rates
- [ ] OpenTelemetry trace export for inference requests
- [ ] Agent chat timeout config: kill hung requests after a configurable timeout

## Agent Management Plane

Rookery as the control plane for a self-managing agent like Hermes: the agent updates and heals itself, Rookery keeps actual state matching desired state. Only the update/describe half of this exists today.

### Desired state declaration
- [ ] `desired_version` on an agent (`"1.3.0"` or `"latest"`)
- [ ] `desired_state` (`"running"` | `"stopped"`)
- [ ] `restart_policy` (`"on-failure"` | `"on-config-change"` | `"never"`)

### Reconciliation loop
- [ ] Version drift detection: compare actual vs desired, trigger an update on mismatch
- [ ] State reconciliation: converge running/stopped on desired
- [ ] Error pattern classification: distinguish OOM / timeout / connection failures from ordinary restarts

### Observability depth
- [ ] `/api/agents/{name}/describe` — the detailed endpoint behind `rookery agent describe`
- [ ] Uptime and restart history, not just current counters
- [ ] Health depth: not just "alive" but "port listening", "gateway connected", "no recent errors"
- [ ] Error aggregation by type over the last N hours
- [ ] `rookery agent version <name>` — desired vs actual, update available
- [ ] `rookery agent update <name> --to <version>` — version-targeted update

### Dashboard control panel
- [ ] Agent status card with version and health indicators
- [ ] One-click update with version selector
- [ ] Restart history timeline (when, why)

## vLLM Backend

Core support works — profiles, compose generation, health checks, swap, log capture, CUDA error detection. What is missing is the configurability a non-stock image needs.

- [ ] Custom environment variables per profile (`environment: Vec<String>` into compose generation)
- [ ] Build context support so `docker_image` can name a local image built from a Dockerfile
- [ ] Entrypoint override for pre-serve setup (pip install, config patches)
- [ ] Per-profile env injection rather than only the hardcoded `HF_TOKEN` passthrough
- [ ] Model warmup: pre-download before a swap to shorten the downtime window
- [ ] Compose health check passthrough: use vLLM's native healthcheck
- [ ] `rookery start --backend vllm` CLI override (profile config decides today)

### Quantization profiles (untested)
- [ ] NVFP4 profile: Qwen3.5-27B-NVFP4, claimed ~80 tok/s gen at 229K context
- [ ] TurboQuant KV profile: AWQ-4bit weights + turboquant35 KV cache at 262K context
- [ ] `rookery bench --profile a --profile b` side-by-side comparison

### A/B testing
- [ ] Dual-port mode: llama.cpp on 8081 and vLLM on 8000, swap an agent between them
- [ ] Tool calling quality benchmark: BFCL-V4 on Q6_K (llama.cpp) vs NVFP4 (vLLM)
- [ ] Write it up: "Tool Calling Quality Across Quantization Formats on RTX 5090"

## Benchmarking

`rookery bench` measures the API path. It cannot tell a model regression from a driver one.

- [ ] Run `llama-bench` directly (pp512/pp2048/pp8192 + tg128) alongside the API-level bench
- [ ] Stop/start the server around `llama-bench` so it gets exclusive GPU access
- [ ] Report both raw-hardware and in-server numbers
- [ ] Store results history (JSON) and compare against a saved per-profile baseline to catch regressions
- [ ] `rookery bench --full` to opt into the extended run

## Security

- [ ] Warn at daemon startup when `listen` is not loopback and no `api_key` is set
- [ ] Rate limiting on the chat proxy endpoint
- [ ] TLS termination guidance (nginx / caddy reverse proxy templates)

## Architecture

- [ ] Split `routes.rs` by domain (server, agents, models, observability) — it is past the size where review is reliable
- [ ] Formalize `InferenceBackend` as a plugin registry, if a third backend (TGI, remote OpenAI) ever justifies it

## Testing & CI

- [ ] End-to-end: daemon startup → start → swap → agent lifecycle (manual only today)
- [ ] vLLM integration tests in CI (gated behind `ROOKERY_INTEGRATION=1`, needs Docker + a free GPU)
- [ ] Test matrix: stable + nightly Rust
- [ ] Dashboard CI job: trunk build + wasm32 validation

## Documentation & Distribution

- [ ] OpenAPI/Swagger spec for the daemon API (machine-readable contract)
- [ ] Document the two-toolchain contributor workflow (Rust + trunk/wasm for the dashboard)
- [ ] Publish to crates.io — optional, evaluate whether the workspace structure allows it

## Deliberately Not Doing

- **Proactive scheduled restarts.** llama-server has been stable enough that a restart timer would only add a failure mode.
- **Multi-user / multi-tenant anything.** Rookery is one user on one workstation, by design. See [DESIGN.md](DESIGN.md).

## Someday

- Multi-GPU with explicit per-profile GPU placement (the data model is ready; the engine picks GPU 0)
- Multi-model concurrent serving (several profiles on different ports at once)
- Request rewriting / filtering as a proxy layer
- Custom agent framework for building and testing agents against local models

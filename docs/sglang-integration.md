# SGLang Integration

SGLang is a first-class backend alongside llama-server and vLLM. It runs as a
Docker container driven by plain `docker run`, and `rookery swap` crosses
engines on the same port: a local llama-server process stops, a container
starts, clients see the same endpoint.

```bash
rookery swap qwen38_sglang   # ~65 s: teardown, container up, health-gated
rookery swap qwen38          # ~17 s back to llama-server, container removed
```

See [configuration.md](configuration.md#sglang-backend) for the full sub-table
reference. This document covers how it works and what to watch.

## Why a separate backend rather than reusing vLLM's

Both are containers, but the lifecycle differs enough to matter. vLLM is driven
by Docker Compose from a generated compose file; SGLang's whole invocation is
described by its config sub-table, so there is nothing to generate and a compose
file would be a second source of truth. `SglangBackend` therefore issues
`docker run` directly and tracks the container by a name derived from the
profile (`rookery-sglang-<profile>`).

## Lifecycle

| operation | how |
|---|---|
| liveness | `docker inspect --format {{.State.Running}}` — there is no host PID to probe |
| identity | container ID; `BackendInfo.pid` is `None`, and status reports `pid: 0` |
| adopt | verifies the running container ID against the recorded one, prefix-matching so a 12-char short ID and a 64-char full ID compare equal |
| stop | `docker stop -t 20` then an explicit `rm`; "no such container" and "is not running" count as success, anything else leaves state intact rather than forgetting a container that may still hold the GPU |
| logs | tails **both** stdout and stderr — SGLang writes startup and per-batch lines to stderr, so tailing stdout alone shows almost nothing |

## Three things that are load-bearing, not stylistic

**The argv is a `Vec<String>`, never a shell string.**
`--json-model-override-args {"language_model_only":true}` must reach the process
as a single argument with its inner double quotes intact. Routing it through a
shell strips them, SGLang receives `{language_model_only:true}`, and dies in
`json.loads`. There is a test asserting the element survives intact.

**Readiness allows 300 seconds.**
Cold start is weight load plus CUDA-graph capture — 60–90 s measured for a 20 GB
NVFP4 checkpoint with a DFlash2 drafter. Inheriting llama-server's timeout would
mark a healthy server dead.

**`SGLANG_ENABLE_POST_CAPTURE_KV_SIZING` defaults to `0`.**
Enabled, it re-sizes the KV pool from memory measured *after* graph capture,
when almost nothing remains. Measured cost: 137,735 usable tokens down to 1,560.

## Telemetry

SGLang exposes no `/slots`, so the dashboard does not try to render one. It
scrapes Prometheus `/metrics` instead, which carries strictly more than
llama-server's slot payload:

| tile | metric |
|---|---|
| KV Used / Total | `kv_used_tokens` / `max_total_num_tokens` |
| KV Usage | `full_token_usage` |
| **GDN State Pool** | `mamba_usage` |
| Accept Length / Rate | `spec_accept_length` / `spec_accept_rate` |
| Prefix Cache Hit | `cache_hit_rate` |
| Requests | `num_running_reqs` / `num_queue_reqs` |
| KV Cache | `kv_cache_memory_usage_gb` |

**GDN State Pool gets its own tile deliberately.** On a 32 GB card serving a
hybrid GatedDeltaNet model, the recurrent state pool — not the KV cache — is
what runs out first. `/slots` cannot show it at all.

This requires `enable_metrics = true` (the default). Without it SGLang serves an
empty `/metrics` and the card renders nothing.

The parser ignores histograms and `_bucket` series, which are most of the
payload and none of the signal here, and drops non-finite values: `fwd_occupancy`
is genuinely `NaN` before any traffic and JSON cannot represent it.

## Release tracking

When SGLang is the running backend, the release monitor polls
`sgl-project/sglang` rather than llama.cpp. Nightly images report
`0.0.0.dev1+g<hash>.d<YYYYMMDD>`, where the semver carries no information at
all, so the **build date** is compared against the release's publish date. A
nightly built after the newest release reads as *ahead* rather than prompting a
downgrade.

## Known upstream issues worth knowing

These are SGLang's, not Rookery's, and none is worked around here.

- **[#36548](https://github.com/sgl-project/sglang/issues/36548)** — cross-request
  contamination under concurrency. Reported at 36.5% wrong answers on affected
  requests at concurrency 8, clean when serial. `max_running_requests = 1` keeps
  you on the safe side; do not raise it casually.
- **[#35150](https://github.com/sgl-project/sglang/issues/35150)** — GDN recurrent
  state drift: greedy output diverges from a non-speculative baseline even when
  every draft token is force-rejected. Open, no fix.
- **[#36291](https://github.com/sgl-project/sglang/issues/36291)** —
  `--enable-deterministic-inference` is accepted and reported as enabled but has
  no effect on NVFP4 + hybrid-Mamba checkpoints.
- **[#35949](https://github.com/sgl-project/sglang/issues/35949)** — vision
  grounding coordinates are wrong on SGLang while correct on vLLM for the same
  checkpoint. Fix unmerged.
- **`--enable-linear-replayssm-spec` is KDA-only** and hard-errors with DFLASH on
  a GatedDeltaNet model, despite reading like a general memory optimisation.

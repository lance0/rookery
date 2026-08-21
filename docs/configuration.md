# Configuration Reference

Config file: `~/.config/rookery/config.toml`

## Validation is a boot gate

**An invalid config no longer boots degraded — the daemon refuses to start.**
`rookeryd` calls `Config::validate()` immediately after loading the file and, on
any error, prints `invalid config: <reason>` plus the config path to stderr and
exits with status 1.

This is a behaviour change. A config with, say, a typo'd `source` or a
`default_profile` naming a profile that does not exist previously started and
then misbehaved silently — a bogus `default_profile` started whichever profile
`HashMap` iteration happened to yield first, and an unrecognised `source` emitted
no model argument at all and failed later inside llama.cpp with an opaque error.
Now it stops at boot with the reason.

The rules enforced:

| Rule | Error when violated |
|---|---|
| Model `source` must be exactly `"hf"` or `"local"` | `unknown source '<x>' — must be "hf" or "local"` |
| `source = "local"` requires `path` | `source = "local" requires \`path\`` |
| `source = "hf"` requires `repo` | `source = "hf" requires \`repo\`` |
| An `hf` model used by a **llama-server** profile also requires `file` | `has source = "hf" but no \`file\` — llama-server needs \`-hf <repo>:<file>\`` |
| Every profile's `model` must name a defined `[models.*]` entry | invalid model reference |
| A profile may not have both `llama_server` and `vllm` sub-tables | `exactly one backend must be specified` |
| vLLM `gpu_memory_utilization` must be in `(0.0, 1.0]` | reports the offending value |
| `default_profile` must name a defined profile | profile not found |
| `llama_server` binary path must be set and exist — **only if** at least one llama-server profile is defined | path required / binary not found |

The `file` requirement is llama-server-only on purpose: vLLM takes the bare repo
and has no GGUF file to name.

Note what is *not* checked: nothing calls `path.exists()` on a model. These are
field-presence checks only, because an HF cache populates on first start and
model directories can be lazily mounted.

Under systemd this failure is bounded rather than a restart loop —
`rookery.service` sets `StartLimitIntervalSec=300` / `StartLimitBurst=5`, so five
failed starts in five minutes puts the unit in `failed` instead of thrashing.
Check `journalctl -u rookery -n 20` for the `invalid config:` line.

Check a config before restarting anything:

```bash
rookery config-validate          # "config OK: <path>", or the error on stderr + exit 1
rookery config-validate --json   # {"valid": true, "path": ...} / {"valid": false, "error": ...}
```

`POST /api/reload` validates the candidate config the same way and rejects it
without swapping, so a bad edit cannot take down a running daemon — the error
comes back on the response and the running server and agents are left untouched.

## Top-Level

```toml
llama_server = "/path/to/llama-server"    # path to llama-server binary
default_profile = "qwen_fast"              # profile used when no name specified
listen = "0.0.0.0:3131"                   # daemon listen address
api_key = "rky-..."                        # optional shared bearer token, unset/empty disables auth
auto_start = true                          # start default profile on daemon boot
idle_timeout = 1800                        # seconds before auto-sleep; 0/omitted disables
model_dirs = ["/mnt/models"]              # extra dirs to scan for model files (optional)
release_check_interval = 1800             # seconds between upstream release checks; 0 disables
# github_token = "ghp_..."               # optional GitHub token for higher rate limits
```

`api_key` is optional. When set, all `/api/*` data routes and the SSE stream require `Authorization: Bearer <key>`. The dashboard HTML shell loads without auth but shows an unlock prompt before fetching data. The CLI automatically reads the key from config.

**Exempt endpoints** (always public): `/api/health`, `/metrics`.

### Security Considerations

- **`listen = "127.0.0.1:..."`** is the secure default. Only bind to `0.0.0.0` if you need LAN access, and always set `api_key` when doing so.
- **SSE query tokens**: The SSE endpoint (`/api/events`) accepts `?token=<key>` because `EventSource` doesn't support custom headers. Query-string tokens may appear in reverse proxy logs, browser history, and HTTP referrer headers. For sensitive deployments, use a reverse proxy that strips or rewrites the query parameter.
- **`/metrics` is always public** when auth is enabled. This is intentional for Prometheus scraping but means GPU stats, server state, and canary health are exposed without auth. If this is a concern, restrict `/metrics` at the reverse proxy level.
- **Agent `update_command`** is executed via `sh -c` and logged to journald. Avoid embedding secrets or tokens directly in the command — use environment variables or credential files instead.
- **TLS**: Rookery does not terminate TLS. For HTTPS, put a reverse proxy (nginx, caddy) in front. Example:
  ```
  # Caddy (automatic HTTPS)
  lancebox.local {
      reverse_proxy localhost:3131
  }
  ```

`idle_timeout` is daemon-wide. When the active backend has been idle for that many seconds with no inference traffic, Rookery unloads it and transitions to `sleeping`. The next `/api/chat` request wakes the last active profile automatically before proxying.

`model_dirs` adds custom directories to the model scanner. Rookery always scans the HuggingFace hub cache and llama.cpp cache automatically — use `model_dirs` for models stored outside those standard locations.

`release_check_interval` controls how often the daemon polls GitHub for new llama.cpp releases. Default is 1800 seconds (30 minutes). Set to 0 to disable. Uses ETag caching to avoid counting against GitHub's rate limit when nothing has changed.

Only `ggml-org/llama.cpp` is polled. vLLM is not tracked: the version comparison parses llama.cpp's `bNNNNN` build-number tags, and vLLM's `vX.Y.Z` tags would fail that parse and silently report "up to date" forever.

`github_token` is optional. Without it, GitHub allows 60 API requests per hour. With a personal access token, the limit is 5000/hr. Polling uses 1 request per interval (one tracked repo). The token only needs public repo read access.

## Models

Define what models are available. Referenced by profiles.

### HuggingFace models (GGUF — for llama-server)

```toml
[models.qwen35]
source = "hf"                              # "hf" (HuggingFace) or "local"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"    # HF repo
file = "UD-Q5_K_XL"                       # quant label (without .gguf)
estimated_vram_mb = 29200                  # for capacity gate (optional)
```

### HuggingFace models (any format — for vLLM)

vLLM supports safetensors, AWQ, GPTQ, NVFP4, and other formats. No `file` field needed — vLLM manages the model inside Docker.

```toml
[models.qwen35_27b_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"
estimated_vram_mb = 20000
```

### Local models

Point directly at a model file on disk (GGUF for llama-server, or any format for vLLM).

```toml
[models.local_model]
source = "local"
path = "/path/to/model.gguf"              # local file path
estimated_vram_mb = 20000
```

## Profiles

Define how to run a model. Multiple profiles can share a model.

```toml
[profiles.qwen_fast]
model = "qwen35"                # references [models.qwen35]
aliases = ["qwen", "fast"]      # optional alternate names for this profile
port = 8081                     # llama-server listen port
ctx_size = 262144               # context window (tokens)
threads = 4                     # CPU threads for inference
threads_batch = 24              # CPU threads for batch processing
batch_size = 4096               # batch size
ubatch_size = 1024              # micro-batch size
gpu_layers = -1                 # -1 = all layers on GPU
gpu_index = 0                   # GPU device index (optional, for multi-GPU setups)
cache_type_k = "q8_0"          # KV cache key quantization
cache_type_v = "q8_0"          # KV cache value quantization
flash_attention = true          # enable flash attention
reasoning_budget = 0            # -1 = unlimited thinking, 0 = disabled
chat_template = "/path/to/template.jinja"  # custom chat template (optional)
temp = 0.7                      # sampling temperature
top_p = 0.8                     # nucleus sampling
top_k = 20                      # top-k sampling
min_p = 0.0                     # min-p sampling
extra_args = ["--no-mmap"]      # additional llama-server args (optional)
```

### KV Cache Quantization

| Type | Quality | VRAM Usage | Notes |
|------|---------|------------|-------|
| `f16` | Best | Highest | |
| `q8_0` | Near-lossless | ~50% of f16 | **Default.** Recommended for most models |
| `q4_0` | Good | ~25% of f16 | Use when VRAM is tight (e.g., Q6 model weights) |

### Reasoning Budget

| Value | Behavior |
|-------|----------|
| `0` | Thinking disabled (no `<think>` tags) |
| `-1` | Unlimited thinking (model decides) |
| `N` | Cap thinking to N tokens |

## vLLM Backend

Profiles can use vLLM instead of llama-server by adding a `[profiles.<name>.vllm]` sub-table:

```toml
[models.qwen35_27b_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"
estimated_vram_mb = 25800

[profiles.qwen_nvfp4]
model = "qwen35_27b_nvfp4"
port = 8081

[profiles.qwen_nvfp4.vllm]
docker_image = "vllm/vllm-openai:cu130-nightly"   # Docker image
gpu_memory_utilization = 0.89                       # fraction of VRAM to use
max_num_seqs = 4                                    # max concurrent sequences
max_num_batched_tokens = 4096                       # per-batch token budget
max_model_len = 234567                              # max context length
quantization = "awq_marlin"                         # quantization method
tool_call_parser = "qwen3_coder"                    # tool call format parser
kv_cache_dtype = "fp8"                              # KV cache quantization
extra_args = ["--enable-chunked-prefill"]            # additional vLLM flags
```

### Prerequisites for vLLM

- Docker + Docker Compose v2+
- NVIDIA Container Toolkit (`nvidia-container-toolkit`)
- HuggingFace token: set `HF_TOKEN` env var (for gated models)

### How It Works

1. Rookery generates `~/.config/rookery/vllm-compose.yml` from your profile config
2. `rookery start` runs `docker compose up -d` instead of spawning llama-server
3. `rookery stop` runs `docker compose down`
4. Health checks, inference canary, and agent management work identically
5. CUDA errors detected in docker logs trigger the same immediate canary

### Backend Selection

The backend is determined by the profile's sub-table:
- `[profiles.name.llama_server]` → llama-server (default, can also be flat with no sub-table)
- `[profiles.name.vllm]` → vLLM via Docker Compose

```bash
rookery start qwen_fast     # uses llama-server (has llama_server sub-table)
rookery start qwen_nvfp4    # uses vLLM (has vllm sub-table)
rookery swap qwen_fast       # swaps between backends seamlessly
```

## Agents

See [Agent Management](agents.md) for full documentation.

```toml
[agents.my_agent]
command = "/path/to/agent"
args = ["run"]
workdir = "/path/to/agent"      # cwd for the agent process (optional)
env = { LOG_LEVEL = "info" }    # extra environment variables (optional)
auto_start = true
restart_on_swap = true
restart_on_crash = true
depends_on_port = 8081
version_file = "/path/to/pyproject.toml"
update_command = "/path/to/agent update"
update_workdir = "/path/to/agent/repo"
restart_on_error_patterns = ["ConnectionError", "ReadTimeout"]
stop_timeout_secs = 30
data_dir = "/path/to/agent/data"
```

`data_dir` points at the agent's SQLite databases, and it is what enables **both**
database-safety features. Without it (and without a `workdir` to fall back to)
the nightly integrity sweep and the pre-change backups do nothing at all — they
have no root to resolve and are silently skipped. If you take one thing from this
page for an agent with a database, it is: set `data_dir`.

- The directory and its immediate subdirectories are scanned for `*.db` and
  checked once a day with a read-only `PRAGMA quick_check`. Findings are logged
  and exported as `rookery_agent_db_corrupt`; they never restart the agent.
- The same root is where a `VACUUM INTO` copy is written before an update, a swap
  bounce, or a manual `rookery agent stop`.

It is deliberately **not** overloaded onto `workdir`, because `workdir` also sets
the agent process's cwd — coupling "enable integrity checks and backups" to
"relocate the agent" is a footgun. An agent that runs from anywhere and writes to
`~/.hermes` should not have to be moved to get checked. `data_dir` falls back to
`workdir` when unset; omit both and the agent is simply not covered.

See [Agent Management](agents.md#database-integrity-data_dir) for the full
behaviour, including backup retention and disk cost.

`stop_timeout_secs` is how long the daemon waits after `SIGTERM` before escalating
to `SIGKILL`. Default is 30 seconds. Raise it for an agent with heavy shutdown
work — an agent checkpointing a large SQLite WAL that gets hard-killed mid-write
is how torn pages happen. Taking the SIGKILL path is logged at `error` level and
should be treated as an incident.

`restart_on_error_patterns` matches stderr lines (case-insensitive substring).
**A single match is not enough: three matches inside a ten-minute sliding window
are required before the agent is bounced.** One `ReadTimeout` is a transient
network condition, not a fatal state, and restarting a whole agent process over
one multiplies the risk of interrupting a write.

The consequence worth knowing: **a pattern that a wedged agent prints exactly
once will no longer restart it.** A genuinely wedged gateway re-emits its error
every poll cycle and trips the threshold in seconds, and failures arriving as
slowly as one every ~5 minutes still reach it — but a single fatal line does not.
`restart_on_crash` still covers an agent that actually exits. The match counter
lives with the process, so a successful restart starts it at zero, and it is
cleared when it fires.

Restarts through this path use the same exponential backoff as crash restarts.

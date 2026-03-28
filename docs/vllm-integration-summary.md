# vLLM Backend Integration Summary

## What Was Built

vLLM is now supported as an alternative inference backend alongside llama-server. The implementation spans 21 files across all 5 crates (~6,800 lines added) with 171 tests (up from 26).

### Milestone 1: Backend Abstraction (core refactoring)

- **InferenceBackend trait** (`rookery-engine/src/backend.rs`) — async trait with methods: `start`, `stop`, `is_running`, `process_info`, `adopt`, `to_server_state`, `is_draining`, `set_draining`, `subscribe_errors`
- **LlamaServerBackend** wraps existing ProcessManager, zero behavior change
- **Config sub-tables** — Profile now uses `[profiles.name.llama_server]` and `[profiles.name.vllm]` sub-tables for backend-specific fields. Flat legacy profiles (no sub-table) auto-detected as llama-server for backward compat
- **BackendType enum** (LlamaServer, Vllm) in ServerState::Running with backward-compatible deserialization
- **Daemon refactored** — AppState holds `Box<dyn InferenceBackend>`, all routes use trait methods, swap orchestration at daemon level with proper drain flag management

### Milestone 2: vLLM Backend

- **Docker Compose generation** (`rookery-engine/src/compose.rs`) — generates compose.yml with NVIDIA GPU reservation, port mapping, HF_TOKEN passthrough, all vLLM flags from config
- **VllmBackend** — full lifecycle: `docker compose up -d` with health polling, `docker compose down`, container adoption on daemon restart, log capture via `docker compose logs -f` with `[vllm]` prefix, CUDA error detection
- **Capacity gate** — vLLM profiles bypass VRAM check (vLLM manages its own memory)
- **API graceful degradation** — `/api/model-info` and `/api/server-stats` return null for llama.cpp-specific `/props` and `/slots` endpoints when backend is vLLM
- **Env-gated integration tests** (`ROOKERY_INTEGRATION=1`) for Docker lifecycle

### Milestone 3: User-Facing

- **CLI** — `rookery status` shows `backend: llama-server` or `backend: vllm`; `rookery profiles` shows `[llama-server]` / `[vllm]` prefix per profile
- **Dashboard** — Backend badge ("llama.cpp" / "vLLM") on status card, backend type in profile switcher metadata, ServerStats shows "N/A" for vLLM (no /slots endpoint)

### Config Format

```toml
# llama-server profile
[profiles.qwen_fast]
model = "qwen35"
port = 8081

[profiles.qwen_fast.llama_server]
ctx_size = 262144
threads = 4
# ... all existing llama-server params

# vLLM profile
[profiles.qwen_nvfp4]
model = "qwen35_27b_nvfp4"
port = 8081

[profiles.qwen_nvfp4.vllm]
docker_image = "vllm/vllm-openai:cu130-nightly"
gpu_memory_utilization = 0.89
max_num_seqs = 4
max_model_len = 234567
quantization = "awq_marlin"
tool_call_parser = "qwen3_coder"
kv_cache_dtype = "fp8"
extra_args = ["--enable-chunked-prefill"]
```

---

## Action Items For You

### 1. Update your config.toml (REQUIRED before running new binary)

Your existing `~/.config/rookery/config.toml` needs the llama-server sub-tables. See `config.example.toml` for the full format. The key change:

```toml
# BEFORE (flat, still works via backward compat):
[profiles.qwen_fast]
model = "qwen35"
port = 8081
ctx_size = 262144
threads = 4
...

# AFTER (explicit sub-table, recommended):
[profiles.qwen_fast]
model = "qwen35"
port = 8081

[profiles.qwen_fast.llama_server]
ctx_size = 262144
threads = 4
...
```

Flat format still works (auto-detected as llama-server), but the sub-table format is recommended.

### 2. Add vLLM profiles to config.toml

Add your desired vLLM profiles. Example from your spec:

```toml
[models.qwen35_27b_nvfp4]
source = "hf"
repo = "kaitchup/Qwen3.5-27B-NVFP4"
estimated_vram_mb = 25800

[profiles.qwen_nvfp4]
model = "qwen35_27b_nvfp4"
port = 8081

[profiles.qwen_nvfp4.vllm]
docker_image = "vllm/vllm-openai:cu130-nightly"
gpu_memory_utilization = 0.89
max_num_seqs = 4
max_model_len = 234567
quantization = "awq_marlin"
tool_call_parser = "qwen3_coder"
kv_cache_dtype = "fp8"
extra_args = ["--enable-chunked-prefill"]
```

### 3. Build and deploy

```bash
cargo build --release
cd crates/rookery-dashboard && trunk build --release && cd ../..
cargo build --release   # re-embed updated dashboard dist

# Stop old daemon, start new one
kill $(pgrep rookeryd)
./target/release/rookeryd &
```

### 4. Test vLLM lifecycle manually

```bash
# Stop current llama-server to free GPU
rookery stop

# Start a vLLM profile
rookery start qwen_nvfp4

# Check status
rookery status        # should show Backend: vllm
rookery status --json # includes "backend": "vllm"

# Swap back to llama-server
rookery swap qwen_fast

# Run integration tests (optional)
ROOKERY_INTEGRATION=1 cargo test -p rookery-engine test_integration_vllm_
```

### 5. Verify NVIDIA Container Toolkit

vLLM requires the NVIDIA Container Toolkit for GPU access inside Docker:

```bash
docker run --rm --runtime=nvidia --gpus all nvidia/cuda:12.0-base nvidia-smi
```

If this fails, install `nvidia-container-toolkit`.

---

## Known Remaining Items

These were identified during the mission but deferred per your request:

| Item | Description | Priority |
|------|-------------|----------|
| **Flaky test** | `test_is_pid_alive_parses_stat` expects process state `R` but sometimes sees `S`/`D` due to scheduler timing. Pre-existing, not caused by this work. | Low |
| **Canary stale receiver** | After backend swap, the inference canary holds the old backend's error receiver. CUDA errors from the new backend may not trigger the canary until next poll cycle. | Medium |
| **Compose --model validation** | Config validation doesn't reject vLLM profiles whose model has no `repo` field (would produce a compose file without `--model`). | Low |
| **Integration test health timeout** | `ROOKERY_INTEGRATION=1` tests timeout at 120s because GPU is occupied by llama-server. Stop llama-server first, then run. | N/A (env) |
| **Manual E2E flows** | Start vLLM, swap between backends, daemon restart recovery, graceful shutdown — need manual verification with Docker + free GPU. | High |

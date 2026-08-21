# Quick Start

## Prerequisites

- Rust toolchain (stable)
- NVIDIA GPU with CUDA support
- [llama.cpp](https://github.com/ggml-org/llama.cpp) built with CUDA (see [Building llama.cpp](#building-llamacpp) below)
- Trunk (for dashboard): `cargo install trunk`

## Build

```bash
git clone https://github.com/lance0/rookery.git
cd rookery

# Build backend
cargo build --release

# Build dashboard (optional — embedded in daemon binary)
cd crates/rookery-dashboard && trunk build --release && cd ../..

# Re-embed dashboard into daemon
touch crates/rookery-daemon/src/routes.rs
cargo build --release -p rookery-daemon
```

## Configure

```bash
mkdir -p ~/.config/rookery
cp config.example.toml ~/.config/rookery/config.toml
```

Edit `config.toml`:
- Set `llama_server` to your llama-server binary path
- Set `listen` to your preferred address (default: `127.0.0.1:3000`, use `0.0.0.0:3131` for LAN access)
- Configure at least one model and profile

The simplest model config points at a HuggingFace GGUF repo — Rookery handles the download:

```toml
[models.qwen35]
source = "hf"
repo = "unsloth/Qwen3.5-35B-A3B-GGUF"
file = "UD-Q4_K_XL"

[profiles.default]
model = "qwen35"
port = 8081
```

If you already have GGUF files on disk, use a local model:

```toml
[models.my_model]
source = "local"
path = "/path/to/my-model.gguf"

[profiles.default]
model = "my_model"
port = 8081
```

Check the file before starting the daemon:

```bash
rookery config
```

**The daemon refuses to boot on an invalid config.** It applies the same validation `rookery config` does and exits non-zero rather than starting on a config it cannot honour, so a bad edit shows up as a daemon that will not come up. Validation covers `default_profile` pointing at a profile that exists, a non-empty `[profiles]` table, and each model's fields: `source` must be `hf` or `local`, `local` needs `path`, `hf` needs `repo` — plus `file` when a llama.cpp profile references it. Run `rookery config` first and you get the error directly instead of reading it out of the service log.

Once the daemon is running, `rookery reload` applies most config edits without a restart. See [CLI Reference](cli.md) for what reload can and cannot change.

See [Configuration](configuration.md) for all options including vLLM backend, sampling parameters, and KV cache tuning.

## Building llama.cpp

Rookery manages a llama-server process. Build llama.cpp with CUDA support:

```bash
git clone https://github.com/ggml-org/llama.cpp.git
cd llama.cpp

cmake -B build \
  -DGGML_CUDA=ON \
  -DGGML_CUDA_FA_ALL_QUANTS=ON \
  -DGGML_NATIVE=ON \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_SHARED_LIBS=OFF

cmake --build build -j$(nproc)
```

The binary is at `build/bin/llama-server`. Set this path as `llama_server` in your rookery config.

**Blackwell (RTX 5090):** Use CUDA 12.8 toolkit and add `-DCMAKE_CUDA_COMPILER=/usr/local/cuda-12.8/bin/nvcc -DCMAKE_CUDA_ARCHITECTURES="120"`. Do NOT use CUDA 13.x (compiler bug).

**Other NVIDIA GPUs:** The default `cmake` invocation auto-detects your GPU architecture.

## Install

```bash
sudo make install
sudo systemctl daemon-reload
sudo systemctl enable --now rookery
```

This installs `rookeryd` and `rookery` to `/usr/local/bin`, generates a systemd unit, and starts the daemon. Customize with:

```bash
sudo make install PREFIX=/opt/rookery SERVICE_USER=myuser HF_HOME=/mnt/models
```

## Run

```bash
# Check status
rookery status
rookery gpu

# Open dashboard
open http://localhost:3000

# Manual start (if auto_start is not set)
rookery start
```

With `auto_start = true` in your config, the default profile starts automatically on daemon boot.

`rookery status` exits `1` when the daemon is unreachable — it still prints its output, so `rookery status --json | jq .state` keeps working while `rookery status || notify` now fires. See [Exit Codes](cli.md#exit-codes).

## Uninstall

```bash
sudo make disable
sudo make uninstall
```

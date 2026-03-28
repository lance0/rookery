# Quick Start

## Prerequisites

- Rust toolchain (stable)
- NVIDIA GPU with CUDA support
- llama.cpp built with CUDA ([lancebox-inference setup](https://github.com/lance0/lancebox-inference))
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
- Configure models and profiles (see [Configuration](configuration.md))

## Run

```bash
# Start daemon
./target/release/rookeryd &

# Start inference server
rookery start

# Check status
rookery status
rookery gpu

# Open dashboard
open http://localhost:3131
```

## Systemd (Recommended)

```bash
sudo cp rookery.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now rookeryd
```

This ensures exactly one daemon instance, proper logging via journald, and auto-restart on failure.

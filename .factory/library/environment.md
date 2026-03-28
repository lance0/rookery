# Environment

Environment variables, external dependencies, and setup notes.

**What belongs here:** Required env vars, external API keys/services, dependency quirks, platform-specific notes.
**What does NOT belong here:** Service ports/commands (use `.factory/services.yaml`).

---

## Required for Build
- Rust 1.94.0+ with cargo
- trunk 0.21+ for dashboard WASM builds
- wasm32-unknown-unknown target (rustup target add)

## Required for vLLM Runtime
- Docker 29+ with Docker Compose v5+
- NVIDIA Container Toolkit (nvidia-container-toolkit)
- HF_TOKEN environment variable for gated HuggingFace models

## Development Machine
- 64 CPU cores, 125GB RAM
- NVIDIA RTX 5090 (32GB VRAM)
- Ubuntu/Debian Linux (kernel 6.17)

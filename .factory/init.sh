#!/bin/bash
set -e

# Ensure Rust toolchain is available
if ! command -v cargo &> /dev/null; then
    echo "ERROR: cargo not found. Install Rust: https://rustup.rs"
    exit 1
fi

# Ensure trunk is available for dashboard builds
if ! command -v trunk &> /dev/null; then
    echo "WARNING: trunk not found. Dashboard builds will fail. Install: cargo install trunk"
fi

# Ensure wasm32 target is installed (needed for dashboard)
if ! rustup target list --installed | grep -q wasm32-unknown-unknown; then
    rustup target add wasm32-unknown-unknown
fi

# Build workspace to verify everything compiles
cargo check --workspace

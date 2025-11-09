#!/bin/bash
# Build script for hello.wasm

set -e

echo "Building hello.wasm..."

# Add wasm32-wasip1 target if not installed
rustup target add wasm32-wasip1

# Build for WASM
cargo build --target wasm32-wasip1 --release

# Copy to examples directory
cp target/wasm32-wasip1/release/hello.wasm ../../examples/hello.wasm

echo "✓ Built: examples/hello.wasm"

# Show file info
ls -lh ../../examples/hello.wasm

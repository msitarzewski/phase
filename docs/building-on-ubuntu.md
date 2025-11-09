# Building Phase on Ubuntu

Complete guide for building the Phase plasm daemon .deb package on Ubuntu.

**Last Updated**: 2025-11-09
**Tested On**: Ubuntu 22.04 LTS

---

## Prerequisites

### 1. Install Rust

```bash
# Install Rust using rustup (recommended)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Follow the prompts (default installation is fine)
# Then reload your shell environment
source $HOME/.cargo/env

# Verify installation
rustc --version
cargo --version
```

**Expected Output**:
```
rustc 1.75.0 (or newer)
cargo 1.75.0 (or newer)
```

### 2. Install cargo-deb

```bash
# Install the Debian packaging tool for Rust
cargo install cargo-deb

# Verify installation
cargo deb --version
```

### 3. Install Build Dependencies

```bash
# Install required system packages
sudo apt update
sudo apt install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    git
```

---

## Clone and Checkout

### 1. Clone the Repository

```bash
# Clone via HTTPS (no authentication required)
git clone https://github.com/msitarzewski/phase.git
cd phase

# Or via SSH (if you have SSH keys configured)
# git clone git@github.com:msitarzewski/phase.git
# cd phase
```

### 2. Checkout the Development Branch

```bash
# Fetch all branches
git fetch --all

# Checkout the session branch with library + binary refactor
git checkout claude/startup-011CUxmJ1exNWpRawHfxRZ2X

# Verify you're on the correct branch
git branch --show-current
# Should output: claude/startup-011CUxmJ1exNWpRawHfxRZ2X

# Verify latest commits
git log --oneline -5
# Should show recent commits including library refactor
```

---

## Build the Project

### 1. Build the Debug Binary (Fast)

```bash
# Build in debug mode (faster compilation, larger binary)
cd daemon
cargo build

# Run tests to verify everything works
cargo test

# Expected: All tests passing
```

### 2. Build the Release Binary (Optimized)

```bash
# Build in release mode (slower compilation, optimized binary)
cargo build --release

# The binary will be at: daemon/target/release/plasmd
ls -lh target/release/plasmd
```

### 3. Build the Debian Package

```bash
# Build the .deb package (includes release build)
cargo deb

# The .deb package will be at: daemon/target/debian/
ls -lh target/debian/*.deb
```

**Expected Output**:
```
-rw-r--r-- 1 user user 4.5M Nov  9 21:40 plasm_0.1.0-1_amd64.deb
```

**Note**: You may see a warning about systemd units - this is expected and doesn't affect the package build.

---

## Install and Test

### 1. Install the .deb Package

```bash
# Install the package
sudo dpkg -i target/debian/plasm_0.1.0-1_amd64.deb

# Check installation
which plasmd
# Should output: /usr/bin/plasmd

# Verify version
plasmd --version
```

### 2. Test the Binary

```bash
# Test local execution with the hello.wasm example
cd ..  # Back to phase root directory
plasmd execute wasm-examples/hello/target/wasm32-wasip1/release/hello.wasm

# Expected output: (Hello, WASM!)
```

### 3. Run the Test Suite

```bash
# Run all tests
cd daemon
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test
cargo test test_wasm_execution_success
```

### 4. Test the PHP SDK (Optional)

```bash
# Install PHP and Composer (if not already installed)
sudo apt install -y php-cli php-sodium composer

# Install PHP SDK dependencies
cd ../php-sdk
composer install

# Run the local test
cd ../examples
php local_test.php

# Expected: Successful WASM execution with receipt
```

---

## Verify Package Contents

### 1. List Package Contents

```bash
# See what files are in the .deb package
dpkg-deb -c daemon/target/debian/plasm_0.1.0-1_amd64.deb

# Expected files:
# /usr/bin/plasmd                 - Main binary
# /usr/share/doc/plasm/           - Documentation
# /lib/systemd/system/plasmd.service - systemd service (if included)
```

### 2. Check Package Metadata

```bash
# View package information
dpkg-deb -I daemon/target/debian/plasm_0.1.0-1_amd64.deb

# Shows:
# - Package name, version, architecture
# - Dependencies
# - Maintainer, description
# - Installed size
```

### 3. Verify Installation

```bash
# Check installed files
dpkg -L plasm

# Check package status
dpkg -s plasm

# Check for any issues
sudo dpkg --audit
```

---

## Troubleshooting

### Build Fails: "linker 'cc' not found"

**Problem**: Missing C compiler

**Solution**:
```bash
sudo apt install build-essential
```

### Build Fails: "Could not find OpenSSL"

**Problem**: Missing OpenSSL development headers

**Solution**:
```bash
sudo apt install libssl-dev pkg-config
```

### cargo-deb Not Found

**Problem**: cargo-deb not in PATH

**Solution**:
```bash
# Ensure Cargo bin directory is in PATH
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc

# Reinstall cargo-deb if needed
cargo install cargo-deb
```

### Tests Fail: "No such file or directory"

**Problem**: Test fixtures or WASM examples not built

**Solution**:
```bash
# Build WASM examples first
cd wasm-examples/hello
./build.sh
cd ../..

# Then run tests
cd daemon
cargo test
```

### Package Install Fails: "Dependency is not satisfiable"

**Problem**: Missing runtime dependencies

**Solution**:
```bash
# Install any missing dependencies
sudo apt install -f

# Then retry installation
sudo dpkg -i target/debian/plasm_0.1.0-1_amd64.deb
```

### Permission Denied When Running plasmd

**Problem**: Binary not executable

**Solution**:
```bash
# Make binary executable
sudo chmod +x /usr/bin/plasmd

# Or reinstall package
sudo dpkg --purge plasm
sudo dpkg -i target/debian/plasm_0.1.0-1_amd64.deb
```

---

## Uninstall

### Remove the Package

```bash
# Remove package but keep configuration
sudo dpkg -r plasm

# Remove package and configuration (purge)
sudo dpkg --purge plasm

# Verify removal
which plasmd
# Should output nothing
```

---

## Clean Build

### Clean Cargo Build Artifacts

```bash
cd daemon

# Remove build artifacts (saves disk space)
cargo clean

# Remove target directory entirely
rm -rf target

# Rebuild from scratch
cargo build --release
cargo deb
```

---

## systemd Service (Optional)

**Note**: The current package configuration has systemd service files but cargo-deb isn't picking them up automatically. To use systemd:

### Manual systemd Setup

```bash
# Copy service file manually
sudo cp daemon/systemd/plasmd.service /lib/systemd/system/

# Reload systemd
sudo systemctl daemon-reload

# Enable service
sudo systemctl enable plasmd

# Start service
sudo systemctl start plasmd

# Check status
sudo systemctl status plasmd

# View logs
sudo journalctl -u plasmd -f
```

---

## Quick Reference

### One-Command Build

```bash
# From phase root directory
cd daemon && cargo deb && cd ..
```

### One-Command Install

```bash
# From phase root directory
sudo dpkg -i daemon/target/debian/plasm_0.1.0-1_amd64.deb
```

### One-Command Test

```bash
# From phase root directory
cd daemon && cargo test && cd ..
```

### Complete Fresh Build

```bash
# Clone, build, package, install (one shot)
git clone https://github.com/msitarzewski/phase.git && \
cd phase && \
git checkout claude/startup-011CUxmJ1exNWpRawHfxRZ2X && \
cd daemon && \
cargo build --release && \
cargo test && \
cargo deb && \
sudo dpkg -i target/debian/plasm_0.1.0-1_amd64.deb && \
plasmd --version
```

---

## Build Times

**Approximate build times on typical hardware**:

| Operation | Time | Notes |
|-----------|------|-------|
| First build (debug) | 3-5 minutes | Downloads and compiles all dependencies |
| Incremental build (debug) | 5-15 seconds | Only recompiles changed code |
| First build (release) | 5-8 minutes | Full optimization |
| cargo deb | 5-8 minutes | Includes release build |
| Subsequent builds | Much faster | Dependencies cached |

**Hardware assumptions**: 4-core CPU, 8GB RAM, SSD

---

## Next Steps

After successful installation:

1. **Read the README**: `less README.md` for usage examples
2. **Try the examples**: `cd examples && php local_test.php`
3. **Explore the API**: `plasmd --help`
4. **Run the daemon**: `plasmd start` (see README for full instructions)

---

## Getting Help

If you encounter issues not covered here:

1. Check the main README: `README.md`
2. Review the cross-architecture demo: `docs/cross-architecture-demo.md`
3. Check the Memory Bank: `memory-bank/` for architectural details
4. Review recent commits: `git log --oneline -20`
5. Open an issue: https://github.com/msitarzewski/phase/issues

---

**Happy Building!** 🚀

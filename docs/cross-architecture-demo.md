# Cross-Architecture Demo Guide

This guide demonstrates Phase's cross-architecture capability: running a client on macOS ARM (Apple Silicon) that discovers and executes jobs on a remote Ubuntu x86_64 node.

## Overview

**Demo Architecture:**
- **Client**: macOS ARM64 (M1/M2/M3) running PHP SDK
- **Worker Node**: Ubuntu 22.04 x86_64 running plasmd daemon
- **Transport**: Kademlia DHT over QUIC/Noise encrypted channels
- **Workload**: WASM module (architecture-independent)

## Prerequisites

### macOS ARM Client

```bash
# Verify architecture
uname -m  # Should output: arm64

# Install PHP 8.1+
brew install php

# Verify PHP
php --version
php -m | grep sodium  # Ensure sodium extension is available

# Install Composer
brew install composer

# Clone repository
git clone https://github.com/phasebased/phase.git
cd phase

# Install PHP SDK dependencies
cd php-sdk
composer install
cd ..
```

### Ubuntu x86_64 Worker Node

```bash
# Verify architecture
uname -m  # Should output: x86_64

# Install from .deb package
sudo dpkg -i daemon/target/debian/plasm_0.1.0_amd64.deb

# Start service
sudo systemctl start plasmd
sudo systemctl status plasmd

# Verify daemon is listening
sudo journalctl -u plasmd -n 20
```

## Network Configuration

### Firewall Rules (Ubuntu Worker)

Phase uses the following ports:

```bash
# QUIC (UDP) - Primary transport
sudo ufw allow 4001/udp

# TCP (fallback)
sudo ufw allow 4001/tcp

# Verify firewall status
sudo ufw status
```

### NAT Traversal

Phase uses QUIC with hole-punching for NAT traversal. Most home routers support this automatically. If discovery fails:

1. **Check router logs** for blocked UDP packets
2. **Enable UPnP** on router (if available)
3. **Configure port forwarding**: Forward UDP 4001 to worker node IP

## Running the Demo

### Step 1: Verify Worker Node is Ready

On Ubuntu x86_64 worker:

```bash
# Check service status
sudo systemctl status plasmd

# Watch logs in real-time
sudo journalctl -u plasmd -f
```

Expected log output:
```
INFO plasmd: Starting Phase daemon v0.1.0
INFO plasmd: Local peer ID: 12D3KooW...
INFO plasmd: Listening on /ip4/0.0.0.0/udp/4001/quic
INFO plasmd: Advertising capabilities: arch=x86_64, runtime=wasmtime-27
```

### Step 2: Run Test from macOS ARM Client

On macOS ARM client:

```bash
# Navigate to examples directory
cd examples

# Run remote test
php remote_test.php
```

### Step 3: Monitor Execution

**On macOS Client (expected output):**
```
Phase Remote Execution Test
===========================

Discovering nodes...
✓ Node discovered: 12D3KooW...
  Architecture: x86_64
  Runtime: wasmtime-27

Submitting job: hello.wasm
✓ Job submitted: 550e8400-e29b-41d4-a716-446655440000

Waiting for execution...
✓ Execution complete

Result:
  Output: dlroW ,olleH
  Exit code: 0
  Wall time: 235ms

Receipt Verification:
✓ Signature valid
✓ Module hash matches
✓ Receipt verified

Test complete!
```

**On Ubuntu Worker (journalctl logs):**
```
INFO plasmd: Peer discovered: <client-peer-id>
INFO plasmd: Job offer received: job_id=550e8400...
INFO plasmd: Accepting job: module_hash=sha256:abc123...
INFO plasmd: Executing WASM module: 84KB, timeout=30s
INFO plasmd: Execution complete: exit_code=0, wall_time=233ms
INFO plasmd: Signed receipt with Ed25519
INFO plasmd: Job result sent to client
```

## Verification Steps

### 1. Architecture Verification

```bash
# On macOS client
echo "Client arch: $(uname -m)"  # Should output: arm64

# On Ubuntu worker (via SSH)
echo "Worker arch: $(uname -m)"  # Should output: x86_64
```

### 2. Signature Verification

The test script automatically verifies Ed25519 signatures. Manual verification:

```php
<?php
require 'vendor/autoload.php';

use PhaseBased\Plasm\Crypto;

$crypto = new Crypto();

// Receipt data from test
$receipt = [
    'job_id' => '550e8400-e29b-41d4-a716-446655440000',
    'module_hash' => 'sha256:...',
    'exit_code' => 0,
    'wall_time_ms' => 235,
];

$signature = '<base64-signature>';
$publicKey = '<node-public-key>';

$valid = $crypto->verify($receipt, $signature, $publicKey);
echo $valid ? "✓ Valid\n" : "✗ Invalid\n";
```

### 3. Network Latency

Measure round-trip time:

```bash
# From macOS client, ping Ubuntu worker
ping -c 5 <worker-ip>

# Expected RTT: 10-100ms (LAN), 50-200ms (Internet)
```

Job execution time includes:
- Network latency (discovery + transmission)
- WASM loading (~10ms)
- Execution time (~233ms for hello.wasm)
- Signature generation (~1ms)

## Troubleshooting

### Issue: No Nodes Discovered

**Symptoms:**
```
Discovering nodes...
✗ No nodes discovered. Ensure plasmd is running on a remote host.
```

**Solutions:**

1. **Verify worker is running:**
   ```bash
   sudo systemctl status plasmd
   ```

2. **Check firewall:**
   ```bash
   sudo ufw status
   # Ensure UDP 4001 is allowed
   ```

3. **Verify network connectivity:**
   ```bash
   # From client, test UDP connectivity
   nc -u <worker-ip> 4001
   ```

4. **Check DHT bootstrap:**
   ```bash
   # Worker logs should show DHT bootstrap
   sudo journalctl -u plasmd | grep -i "bootstrap\|kad"
   ```

### Issue: Job Submission Fails

**Symptoms:**
```
Submitting job: hello.wasm
✗ Submission failed: Connection refused
```

**Solutions:**

1. **Check WASM file exists:**
   ```bash
   ls -lh wasm-examples/hello/target/wasm32-wasip1/release/hello.wasm
   ```

2. **Verify worker accepts jobs:**
   ```bash
   sudo journalctl -u plasmd | grep -i "job offer"
   ```

3. **Check resource limits:**
   - Worker may reject jobs exceeding memory/CPU limits
   - Check manifest requirements vs. worker capabilities

### Issue: Signature Verification Fails

**Symptoms:**
```
Receipt Verification:
✗ Signature verification failed
```

**Solutions:**

1. **Verify php-sodium extension:**
   ```bash
   php -m | grep sodium
   # Install if missing: brew install php
   ```

2. **Check receipt format:**
   - Ensure receipt JSON matches canonical format
   - Module hash must be deterministic

3. **Verify public key:**
   - Ensure client has correct node public key
   - Keys are ephemeral per session in MVP

## Performance Benchmarks

Expected performance for hello.wasm workload:

| Metric | LAN (Gigabit) | Internet (100Mbps) |
|--------|---------------|---------------------|
| Discovery | 50-200ms | 500-2000ms |
| Job Submission | 10-50ms | 100-500ms |
| Execution (WASM) | 233ms | 233ms |
| Result Return | 10-50ms | 100-500ms |
| **Total** | **300-533ms** | **933-3233ms** |

*Note: Execution time is independent of network since WASM runs on worker.*

## Advanced: Multi-Node Demo

To test multi-node discovery:

```bash
# Start multiple worker nodes on different machines
# Ubuntu Node 1 (x86_64)
sudo systemctl start plasmd

# Ubuntu Node 2 (x86_64) - different machine
sudo systemctl start plasmd

# AMD64 Node 3 (if available)
sudo systemctl start plasmd

# From macOS client
php examples/multi_node_test.php  # Discovers all nodes
```

## Security Notes

**MVP Security Model:**

✅ **Implemented:**
- Ed25519 signature verification
- WASM sandboxing (no filesystem/network access)
- Encrypted transport (Noise + QUIC)
- DHT-based anonymous discovery

⚠️ **MVP Limitations:**
- Ephemeral signing keys (regenerated per session)
- No persistent node identity
- No reputation system
- No payment/incentive layer

**Production Considerations:**
- Persistent key management (keyring/TPM)
- Node reputation tracking
- Rate limiting and DDoS protection
- Optional zk-SNARK proofs for privacy

## Next Steps

After successful cross-architecture demo:

1. **Test with custom WASM modules** - Build your own workload
2. **Measure performance** - Benchmark different workload sizes
3. **Multi-hop execution** - Chain jobs across multiple nodes
4. **Integration** - Integrate Phase SDK into your application

## Appendix: Network Topology

```
┌─────────────────────┐
│  macOS ARM Client   │
│  (M1/M2/M3 Mac)     │
│                     │
│  • PHP 8.1+         │
│  • Phase SDK        │
│  • Kademlia Client  │
└──────────┬──────────┘
           │
           │ QUIC/UDP
           │ Encrypted (Noise)
           │
           ▼
    ┌─────────────┐
    │  Kademlia   │
    │     DHT     │
    └──────┬──────┘
           │
           │ Discovery
           │
           ▼
┌─────────────────────┐
│ Ubuntu x86_64 Node  │
│                     │
│  • plasmd daemon    │
│  • wasmtime runtime │
│  • Ed25519 signer   │
└─────────────────────┘
```

## Resources

- **libp2p Documentation**: https://docs.libp2p.io
- **WASM Spec**: https://webassembly.github.io/spec/
- **Ed25519 RFC**: https://datatracker.ietf.org/doc/html/rfc8032
- **Phase Repository**: https://github.com/phasebased/phase

---

**Last Updated**: 2025-11-09
**Tested Configurations**: macOS 14 (ARM64) → Ubuntu 22.04 (x86_64)

# Phase – Open, Verifiable Compute

**Phase** is an open protocol for distributed computation: workloads are discovered, executed, and verified across a global network of independent nodes.  
**Plasm** is the local runtime engine that turns any computer into a node in the network.  
**PhaseBased** (commercial layer) will provide orchestration, SLAs, and billing later—**this repository focuses on the open protocol and free infrastructure.**

---

## Vision

The internet began as a network for sharing information.  
Phase is the next evolution—a network for sharing **computation**.

- **Decentralized:** no data centers, no single owner.  
- **Verifiable:** every result includes a signed receipt of work done.  
- **Private:** jobs execute in sandboxed WebAssembly environments.  
- **Resilient:** discovery and routing are handled peer‑to‑peer using DHT protocols.  

With Phase, you can run a job anywhere—across your LAN, a research cluster, or the public internet—and receive a provable result from an anonymous node.

---

## Architecture

| Layer | Name | Description |
|-------|------|-------------|
| **Protocol** | **Phase** | Defines job manifests, receipts, peer discovery, and encrypted transport. |
| **Runtime** | **Plasm** | Lightweight daemon that runs on any OS and executes WASM workloads. |
| **Client SDKs** | PHP, Swift, C++, React, etc. | Submit jobs, track progress, retrieve results. |
| **(Future)** | **PhaseBased** | Commercial orchestration and guarantees built on top of this open core. |

---

## MVP Scope

Demonstrate end‑to‑end remote execution using anonymous discovery:

1. **Anonymous peer discovery** using a modern DHT (libp2p / Kademlia).  
2. **Job submission** from a PHP client on macOS ARM.  
3. **Remote execution** on a Plasm node (.deb for Ubuntu x86_64).  
4. **Result and receipt return** through encrypted channels.  

**Goal:** a simple PHP script submits a WASM job that runs remotely and returns output + signed proof of execution.

---

## Components

### Plasm Daemon
- Written in **Rust** for safety and portability.
- Embeds a lightweight **WASM runtime** (`wasm3` initially; `wasmtime` next).
- Handles DHT participation, job queueing, encrypted transport, and receipt signing.
- Packaged as `.deb` for Intel/AMD Ubuntu systems.

### PHP Client SDK
- Library to discover peers, submit jobs, track progress, and retrieve results.
- Simple API:
  ```php
  $plasm = new Plasm\Client();
  $jobId = $plasm->submit('hello.wasm', ['cpu'=>1]);
  echo $plasm->result($jobId);
  ```

### Manifests & Receipts
- **Manifest** – declares resources, timeouts, and capabilities.
- **Receipt** – signed proof including module hash, wall time, CPU seconds.

---

## Phases of Development

1. **MVP** – Single‑job remote execution (this repo).  
2. **Networking Expansion** – Multi‑node coordination, redundancy, result recombination.  
3. **Security & Verification** – Deterministic receipts, reputation; explore zk‑proofs.  
4. **Ecosystem SDKs** – Swift, TypeScript, C++, PHP, Python libraries.  
5. **Federation Layer** – Self‑governing clusters, persistent identity, optional payments.  
6. **Commercial Layer** *(PhaseBased)* – SLAs, billing, and guaranteed performance tiers.

---

## Getting Started (after build)

```bash
# 1) Install plasm daemon on remote Ubuntu host
sudo dpkg -i plasm_<version>_amd64.deb
sudo systemctl start plasmd

# 2) On your Mac (PHP client)
composer require phasebased/plasm-php
php examples/remote_test.php
```

Expected output:
```
Discovered node: plasm@203.0.113.42
Sending job: hello.wasm
Result: dlroW ,olleH
Receipt verified ✓
```

---

## Roadmap Preview

- **Distributed Streams:** shard large jobs into parallel WASM streams and recombine results.  
- **Node Discovery Map:** visualize global capacity and latency.  
- **Receipts‑as‑Proof:** deterministic hashes for each compute unit.  
- **Anonymous Volunteer Mesh:** open pool for research and civic compute.  
- **PhaseBased Launch:** commercial orchestration and verified tiers (separate repo).

---

## License
Apache 2.0 © 2025 PhaseBased

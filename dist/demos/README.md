# Demos

Captured asciinema recordings of the LUCID protocol in action.

## `lucid-2node-demo.cast`

**Recorded:** 2026-05-28
**Hardware:** Mac M5 Max (128GB unified) + Parallels Ubuntu ARM64 VM
**Model:** Qwen3-Next 35B-A3B Q4_K_M (loaded on the Mac via llama.cpp + Metal)

Two-node distributed inference demo:

- Node A: the Mac, running `lucidd --worker llama-cpp` with Qwen3 loaded.
- Node B: the Ubuntu VM, running `lucidd --no-local-worker` (no GPU, no model file).
- From inside the VM, `curl http://localhost:11434/api/chat` is issued.
- lucidd on the VM discovers the Mac peer via libp2p mDNS, queries the Phase DHT for "qwen3", finds the Mac's signed advertisement, and routes the request via the `/phase/job-relay/1.0.0` libp2p protocol.
- The Mac executes through `LlamaCppWorker` → `llama-server` on Metal, streams tokens back as a `Vec<JobEvent>`, the VM converts to NDJSON for the curl client.
- Response carries `x-lucid-routed-via: peer:<short-id>` and `x_phase_commitment: <hash>` in the terminal NDJSON frame.

End-to-end latency for short responses: ~2-4 seconds.

## Playback

```bash
# Install asciinema if needed
brew install asciinema   # macOS
sudo apt install asciinema   # Linux

# Play locally
asciinema play lucid-2node-demo.cast

# Or convert to gif / svg / mp4 with asciinema/agg / svg-term-cli / etc.
```

## Notes

- The Linux VM was on Parallels' Shared Network at `10.211.55.5`. The Mac was at `10.211.55.2`. Both addresses are auto-assigned by Parallels and meaningless outside the host.
- The demo uses the v0.1 *batch* peer-relay protocol — the serving peer drains the full inference stream into a `Vec<JobEvent>` and ships the whole vector as one CBOR response. Token streaming across the libp2p substream is a v0.2 polish target.
- `x_phase_commitment` is computed via the SHA-256 chain in `phase-protocol`'s `CommitmentAccumulator`. A verifier can replay the streamed tokens and confirm the hash matches.

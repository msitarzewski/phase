# Milestone: Intended-Stream Vertical Slice

**Status:** 🟨 Blocked — component implementation exists; required physical end-to-end run has not executed
**Track:** Integration gate
**Blocking:** Yes
**Depends on:** Content Plane, Live Relay Plane, Reachability Plane, Network Operations
**Tracker:** [README.md](./README.md)

## Outcome

The project demonstrates the complete LUCID experience on real machines and real networks, with evidence at each trust and transport boundary. This is not a synthetic unit-test milestone and not a “components exist” milestone.

The accepted end state from the [mission](../../MISSION.md#historical-starting-state-2026-05-26) was two machines on different networks where one serves a model and the other uses the Ollama API through Phase. v0.2 strengthens that scenario with verified content distribution, genuine relay streaming, NAT reachability, and receipt binding.

## Canonical demonstration

### Actors

- **Consumer:** clean LUCID installation, no model file, different network from contributor.
- **Contributor:** LUCID node behind consumer NAT, model initially absent or installed through the same content plane, willing to serve under policy.
- **Foundation services:** at least one bootstrap/rendezvous/circuit-relay node; a second site available for resilience testing.
- **Publisher/provider:** publishes the signed alias mapping and serves the verified model artifact; may be the contributor or another node.

### Flow

1. Start from documented clean consumer and contributor state.
2. Both load persistent identities with secure permissions.
3. Consumer discovers foundation services through the default bootstrap path.
4. Contributor obtains/renews a relay reservation and advertises a valid reachable address.
5. Consumer calls `/api/pull` with a human alias.
6. Consumer resolves signed alias → CID, selects a provider, downloads/resumes, verifies, and installs.
7. Contributor advertises the same verified CID as loaded/capable.
8. Consumer issues `/api/chat` or `/api/generate` through an unmodified Ollama-compatible client.
9. Router selects the remote contributor and records the chosen PeerId/CID/path.
10. Contributor accepts under policy and starts the worker.
11. Consumer receives the first output frame before remote generation completes.
12. Output crosses either the circuit relay or a DCUtR-upgraded direct connection, as recorded.
13. Client sees ordered NDJSON output.
14. Terminal receipt binds the exact manifest, output commitment/chunk count, and serving PeerId.
15. Consumer reports success plus CID, serving identity, route, and verified receipt state.

## Evidence contract

Every demonstration captures synchronized, sanitized evidence from all actors:

- Application version/commit, OS, architecture, network role, and PeerId.
- Bootstrap source and discovery timing.
- Alias record publisher, sequence, expiry, resolved CID, provider selection, bytes transferred, resume count, and final verification.
- Relay reservation and connection path changes.
- Request job ID, selected serving PeerId, manifest hash, first-frame timestamp, terminal timestamp, chunk count, commitment, and receipt verification result.
- Resource cleanup after completion/cancellation.
- No prompts, tokens, private keys, raw model bytes, or credentials in committed evidence.

## Required test runs

### Happy-path runs

- [ ] Clean pull and remote inference through a forced relay-only path.
- [ ] Pull interruption followed by resume from the same provider.
- [ ] Pull interruption followed by resume from a different provider serving the same CID.
- [ ] DCUtR-capable path that upgrades from relay to direct.
- [ ] Unmodified Ollama-compatible client in addition to `curl`.
- [ ] Repeated request showing stable identity, content reuse, and any supported cache affinity.

### Failure-path runs

- [ ] Tampered model bytes rejected before registration.
- [ ] Alias conflict shown without arbitrary silent selection.
- [ ] Expired/invalid alias record rejected.
- [ ] Contributor refuses under policy; consumer gets an attributable refusal.
- [ ] First serving peer is unavailable; failover occurs only before output begins.
- [ ] Serving peer disappears mid-stream; request ends unverified/failed, never as success.
- [ ] Receipt is missing, signed by the wrong key, bound to wrong manifest, or has wrong commitment.
- [ ] Client cancels mid-stream; contributor releases work.
- [ ] Relay is lost before request and during request; outcomes match the documented lifecycle.
- [ ] Consumer disk fills during pull; prior installed content stays valid.

## Timing and performance measurements

Record rather than pre-invent release thresholds:

- Time to first bootstrap connection.
- Time to resolve alias and providers.
- Transfer throughput, resume overhead, verification throughput, and install time.
- Time from HTTP request to remote acceptance.
- Remote time to first token and client-observed time to first token.
- Relay-only versus direct-path latency overhead.
- Total request duration and receipt-verification time.
- Peak memory on consumer, contributor, and relay.
- Cleanup latency after cancel/failure.

The release threshold for each metric is approved after baseline runs on the designated Mac and Linux hardware. Regressions thereafter are blockers.

## Traceability matrix

| Claim | Evidence source | Owning milestone |
|---|---|---|
| Content is real and exact | CID recomputation, alias signature, tamper rejection | Content Plane |
| Output is live | first-frame timestamp before remote terminal | Live Relay Plane |
| NAT path is real | relay reservation/path event and topology | Reachability Plane |
| Fresh install works | clean-machine command log and default bootstrap | Network Operations |
| Result is attributable | manifest/PeerId/commitment receipt checks | Live Relay Plane |
| Failure is honest | injected failure and terminal client state | All four dependencies |

## Acceptance criteria

- [ ] Every canonical-flow step succeeds and has evidence.
- [ ] The consumer and contributor are physical machines on different networks; at least one is behind real consumer NAT.
- [ ] Relay-only inference completes with live tokens and verified receipt.
- [ ] DCUtR behavior is demonstrated where supported and relay fallback where not.
- [ ] Clean model pull is content-verified and resumable.
- [ ] First token reaches the client before serving generation completes.
- [ ] Cancellation releases contributor capacity.
- [ ] All listed failure paths produce the documented safe result.
- [ ] Evidence is reproducible from a written runbook and contains no sensitive payloads.
- [ ] The tracker links the evidence bundle, exact commits, configuration, and approved task record.

## Stop conditions

The milestone remains incomplete if any of these occur:

- A model is pre-copied to the consumer and `/api/pull` merely registers it.
- Remote events are replayed from a completed batch.
- Both LUCID nodes are on the same LAN for the only demonstration.
- The serving node is publicly reachable and bypasses the relay for the only demonstration.
- Receipt verification is skipped, waived, or performed only on the server.
- Failure output is useful but mislabeled as verified success.
- A custom test client hides incompatibility with the public Ollama surface.

## Completion evidence required in the tracker

- Sanitized evidence bundle and topology diagram.
- Clean-machine runbook.
- Timing/performance table with hardware and network context.
- Happy/failure-path checklist signed off in review.
- Exact commit/PR, release artifacts, and task documentation.

## 2026-08-09 blocker checkpoint

- Content, live-relay, reachability, reputation, MLX-adapter, API, and validation components now have approved implementation checkpoints and cross-platform automated QA.
- The isolated UMBP smoke proved the native release binary's local version/tags/generate/stream/embed surface and truthful receipt/routing metadata. It did not exercise a remote model, consumer NAT, relay-only inference, or DCUtR.
- This milestone remains blocked exactly as its stop conditions require: no real multi-gigabyte remote pull/resume, no two physical LUCID nodes on different networks, no first-token-before-remote-completion trace, and no full failure-path recording exist yet.
- Unblock by assigning the consumer/contributor/relay topology, real model/bundle, network locations, and evidence run window described by this specification.
- Current component evidence: [2026-08-09 checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).

## 2026-08-10 assigned topology; execution pending

- **Requester**: the Apple Silicon development machine on a physically different network.
- **Public relay/bootstrap**: a new DigitalOcean Ubuntu x86_64 host with Reserved IPv4, running `lucidd --mode infrastructure` locally on TCP `4001`.
- **Contributor**: Pip, an M1 iMac with 16 GB RAM behind consumer NAT, using a real size-appropriate inference model and later the pinned MLX contract.
- **Administration**: Tailscale may provide SSH/recovery, but the test data path must use Phase’s public relay. Caddy TCP `80/443` is a separate web path to UMBP and cannot count as Phase reachability.
- **Transition dependency**: UMBP is being snapshotted before Ubuntu 26.04 LTS upgrade and requires a post-upgrade service/network audit.

The actor roles and network boundary are no longer ambiguous, but the milestone remains blocked until the cloud node is provisioned and the canonical happy/failure flows are recorded. Public rendezvous is excluded from the first run because `lucidd` intentionally disables rendezvous serving pending hard registration quotas; configured relay/bootstrap plus DHT supply the initial test path.

## Operator runbook — 2026-11-07

**State:** executable preparation, not passing acceptance. Reuse `scripts/phase-validate.sh` for source/toolchain/QA evidence and the existing daemon CLI for network actors. The validation script is local-only; it does not provision hosts or manufacture a physical-network pass.

### Observed prerequisites

Read-only SSH preflight on 2026-11-07 found:

- `beacon`: public DigitalOcean Linux x86_64 host, reachable by the existing SSH alias; no running `lucidd`, no Phase checkout/toolchain, and no listener on TCP 4001. Its active firewall permits SSH and web ports but has no Phase-port rule. Caddy is running and must remain untouched.
- `pip`: the historical SSH alias timed out; do not assume the former M1 contributor is available.
- `pipx`: reachable Linux x86_64 host with an NVIDIA RTX 5060 Ti (16 GB), about 30 GiB system RAM and existing real model directories. No `lucidd`, `llama-server`, Ollama service or Rust toolchain was found. This is a candidate contributor, not verified Apple MLX hardware and not proof of the NAT topology.
- Development Mac: installed llama-server and real local GGUF weights are available. Local execution can qualify that backend separately; it cannot satisfy the two-network milestone alone.

No Phase service, public firewall, DNS, Caddy or existing inference service was changed during this preflight. Addresses, keys and model payloads stay out of committed operational evidence. Before choosing the actors, confirm their actual physical networks and consumer-NAT boundary; SSH/Tailscale reachability is administration evidence only.

### Prepare exact binaries and isolated state

1. Run `scripts/phase-validate.sh workspace` on the requester and the existing `linux` profile on each Linux architecture being qualified. Retain failed attempts as well as successful retries. A package reused from August must match that checkpoint's source/binary evidence; a matching version string alone is insufficient.
2. Build the native bundle with `scripts/phase-validate.sh release`. Record the archive/binary SHA-256, source fingerprint, Cargo.lock hash, toolchain and OS for each actor. Use a fingerprinted staging directory, private per-actor identity/policy/catalog paths and unused loopback HTTP ports. Do not replace a standing service for this experiment.
3. Stage the native binary and a version-pinned real llama-server on the selected contributor. Verify the upstream binary digest and model licensing/provenance; use actual GGUF bytes, not an echo worker or a fixture. Choose a model that fits measured available GPU/RAM with a bounded context.
4. On the infrastructure host, verify the dedicated Phase TCP port is free and arrange its scoped ingress rule and rollback before starting the relay. Preserve SSH, Caddy and all web routing. Keep every Ollama HTTP API loopback-only.

### Start the actors

The variables below must contain measured/operator-selected values. `PHASE_RUN_DIR` is a private staging directory on each host; `PHASE_RELAY_MULTIADDR` ends in the actual relay PeerId. Keys are generated on their owning actor and are never copied between roles.

Infrastructure host:

```bash
LUCIDD_PORT=11435 "$PHASE_LUCIDD" --mode infrastructure \
  --no-default-bootstrap --libp2p-port 4001 \
  --external-address "$PHASE_RELAY_PUBLIC_ADDRESS" \
  --identity-path "$PHASE_RUN_DIR/relay.key" \
  --policy-config "$PHASE_RUN_DIR/relay-policy.toml"
```

Read the actual relay PeerId from its startup evidence, then construct its peer-pinned public multiaddr. Public rendezvous serving remains disabled; do not enable it to bypass the quota gate.

Consumer (no local worker and an initially empty artifact store):

```bash
LUCIDD_PORT=11435 "$PHASE_LUCIDD" --mode consume-only \
  --no-default-bootstrap --bootstrap-peer "$PHASE_RELAY_MULTIADDR" \
  --relay-peer "$PHASE_RELAY_MULTIADDR" \
  --identity-path "$PHASE_RUN_DIR/consumer.key" \
  --policy-config "$PHASE_RUN_DIR/consumer-policy.toml" \
  --artifact-dir "$PHASE_RUN_DIR/consumer-artifacts"
```

Contributor:

```bash
LUCIDD_PORT=11435 "$PHASE_LUCIDD" --worker llama-cpp \
  --no-default-bootstrap --bootstrap-peer "$PHASE_RELAY_MULTIADDR" \
  --relay-peer "$PHASE_RELAY_MULTIADDR" \
  --identity-path "$PHASE_RUN_DIR/contributor.key" \
  --policy-config "$PHASE_RUN_DIR/contributor-policy.toml" \
  --model-dir "$PHASE_SOURCE_MODEL_DIR" \
  --artifact-dir "$PHASE_RUN_DIR/contributor-artifacts" \
  --llama-server-binary "$PHASE_LLAMA_SERVER" \
  --llama-ctx-size 2048 --serve-model-content
```

Configure the contributor's existing `authorized_submitters` with the consumer's actual public signing key, `allow_unauthenticated_jobs = false`, `max_concurrent_remote_jobs = 1` and a bounded `max_tokens_ceiling`. Both authorization and signer-to-transport identity binding must hold (`crates/lucidd/src/router.rs:1903-1930`). Do not use the insecure development escape hatch as acceptance policy. Record any battery/thermal serving pause rather than suppressing it silently.

### Pull, stream, cancel and fail

On the consumer, use the advertised normalized alias and retain its signed publisher/CID resolution:

```bash
curl --fail-with-body --no-buffer http://127.0.0.1:11435/api/pull \
  -H 'Content-Type: application/json' \
  --data "{\"model\":\"$PHASE_MODEL_ALIAS\",\"stream\":true}"

curl --fail-with-body --no-buffer --dump-header "$PHASE_RUN_DIR/generate.headers" \
  http://127.0.0.1:11435/api/generate \
  -H 'Content-Type: application/json' \
  --data "{\"model\":\"$PHASE_MODEL_ALIAS\",\"prompt\":\"Describe a tree in three sentences.\",\"stream\":true,\"options\":{\"num_predict\":64}}"
```

These commands are a starting point, not an evidence parser or a whole-milestone pass. Validate the alias before shell interpolation, use a non-sensitive operator-selected prompt, and timestamp receipt of each NDJSON frame with a monotonic clock. The stream's final `x_lucid_receipt_verified` must be `true`, contain a commitment, and have no error. The initial live response header is `pending`; neither HTTP 200 nor `X-Lucid-Routed-Via: peer:...` establishes successful receipt verification. Pair client timing with the contributor's terminal timestamp so buffered output cannot pass as live generation.

Interrupt a multi-GB pull, record the partial-byte offset, then retry the same exact CID/publisher selection (`/api/pull` accepts `cid` and `publisher`). Verify resumed offset and final recomputed CID. Repeat with a second independent provider holding the same bytes. Pulling to a consume-only node must not make inference local; verify the serving identity and transport path.

For cancellation, close the client stream before the requested token budget completes and confirm contributor capacity/process cleanup. For forced relay-only acceptance, restrict direct paths in the isolated test topology and prove a `/p2p-circuit` path from transport evidence; relay configuration alone is not proof. Record DCUtR upgrades separately. Execute the existing failure checklist against isolated actors, including wrong/missing receipts and tampered blobs; existing fixture tests alone do not satisfy physical failure injection.

### Close and report

Stop only the experiment-owned PIDs/units, remove its temporary ingress rule, verify its ports and worker processes are gone, and confirm Caddy/SSH/standing services remain healthy. Retain private raw logs under ignored `target/phase-validation/`; publish only sanitized timings, hashes, public identities, topology and explicit pass/fail/skipped outcomes. Do not commit prompts, generated content, private keys, addresses used only for administration or raw model bytes.

Until every canonical run and failure gate has attributable evidence, keep this milestone blocked. A local real-model smoke, a staged binary or a prepared runbook is useful progress but does not change that state.

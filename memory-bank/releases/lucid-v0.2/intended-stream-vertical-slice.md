# Milestone: Intended-Stream Vertical Slice

**Status:** 🟨 Blocked — component implementation exists; required physical end-to-end run has not executed
**Track:** Integration gate
**Blocking:** Yes
**Depends on:** Content Plane, Live Relay Plane, Reachability Plane, Network Operations
**Tracker:** [README.md](./README.md)

## Outcome

The project demonstrates the complete LUCID experience on real machines and real networks, with evidence at each trust and transport boundary. This is not a synthetic unit-test milestone and not a “components exist” milestone.

The accepted end state from the original mission was two machines on different networks where one serves a model and the other uses the Ollama API through Phase (`memory-bank/MISSION.md:131-135`). v0.2 strengthens that scenario with verified content distribution, genuine relay streaming, NAT reachability, and receipt binding.

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

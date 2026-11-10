# Milestone: Reachability Plane

**Status:** 🟦 In progress — implementation checkpoint and external topology approved; public rendezvous quotas and real-NAT matrix pending
**Track:** Intended stream
**Blocking:** Yes
**Tracker:** [README.md](./README.md)

## Outcome

Phase peers behind typical consumer NAT can discover one another, reserve and use a foundation circuit relay, and attempt a DCUtR direct-path upgrade without manual port forwarding. Rendezvous provides service discovery without replacing Kademlia’s durable records or DNS bootstrap’s first-contact role.

The required libp2p features are already enabled at `crates/phase-net/Cargo.toml:21-39`, but `CombinedBehaviour` currently contains only Kademlia, mDNS, job offer, and the batch job relay (`crates/phase-net/src/discovery.rs:88-99`). The existing backlog names relay server, DCUtR, and rendezvous as the missing coffee-shop path (`memory-bank/activeContext.md:159-172`).

## Scenario definition

The blocking scenario uses three machines or network contexts:

1. A publicly reachable foundation node operating bootstrap, rendezvous, and circuit-relay services.
2. A serving LUCID node behind consumer NAT with no inbound port forwarding.
3. A requesting LUCID node on a different network, also without assumed inbound reachability.

The requester must discover and invoke the serving node through the relay path. Where NATs permit it, DCUtR should upgrade the relayed connection to direct QUIC/TCP; failure to upgrade must retain a functioning relayed path.

## Reuse and integration points

| Responsibility | Extend | Required behavior |
|---|---|---|
| Swarm | `crates/phase-net/src/discovery.rs:210-351` | One swarm and identity; add behaviors to the existing builder |
| Behavior composition | `crates/phase-net/src/discovery.rs:88-99` | Add relay client/server, DCUtR, rendezvous, identify/autonat events as approved |
| Driver model | `crates/phase-net/src/discovery.rs:132-195`, `crates/phase-net/src/discovery.rs:617-700` | Add commands and event handling without blocking swarm polling |
| Bootstrap | `crates/phase-net/src/discovery.rs:311-341`, `crates/lucidd/src/main.rs:134`, `crates/lucidd/src/main.rs:361-365` | Preserve DNS/static bootstrap as first contact |
| Stable identity | `crates/phase-net/src/discovery.rs:210-228` | Use the same persistent key for PeerId and receipts |
| LUCID wiring | `crates/lucidd/src/main.rs:300-407` | Configure role/policy; do not fork networking inside LUCID |

## Required reachability ADR

Approve a decision specifying:

- Node roles: ordinary client, contributor, foundation relay/rendezvous, and whether a private operator can enable the server role.
- Reservation policy and limits: peers per relay, duration, renewal, traffic, concurrent streams, per-peer bandwidth, and denial reasons.
- Advertised address rules and how relay addresses enter Kademlia/provider records.
- Rendezvous namespaces, registration TTL, discovery limits, and relationship to DHT capabilities.
- AutoNAT/observed-address evidence used to decide relay reservation and direct dialing.
- DCUtR attempt policy, timeout, retry, path migration, and fallback.
- Abuse handling and what minimal operational metadata foundation relays retain.
- Compatibility and behavior when one or more facilities are unavailable.

## Work packages

### Behavior wiring

- [ ] Add circuit-relay client support for ordinary peers.
- [ ] Add a configuration-gated circuit-relay server behavior for operator nodes.
- [ ] Add DCUtR behavior and surface success/failure events.
- [ ] Add rendezvous server/client behaviors under explicit role configuration.
- [ ] Wire identify/observed-address and AutoNAT information needed for correct advertisement.
- [ ] Keep behavior event handling non-blocking and covered by unit/state-machine tests.

### Address management

- [ ] Distinguish listen, observed, externally confirmed, and relayed addresses.
- [ ] Never advertise private/unroutable addresses as WAN endpoints unless part of a relay multiaddr.
- [ ] Refresh provider/capability records after reachability changes.
- [ ] Withdraw expired relay reservations and stale relay addresses.
- [ ] Prefer a verified direct path after DCUtR succeeds while retaining controlled fallback.
- [ ] Prevent address spoofing from poisoning peer records.

### Relay reservation and resource policy

- [ ] Request, renew, and release reservations with jittered schedules.
- [ ] Expose reservation state and expiry through structured status.
- [ ] Enforce per-peer and global connection, stream, byte, and bandwidth caps.
- [ ] Rate-limit reservation attempts and repeated failed hole punches.
- [ ] Prioritize swarm health/control traffic so data streams cannot starve discovery.
- [ ] Reject unauthenticated or malformed relay use before expensive allocation.

### Rendezvous

- [ ] Define LUCID and generic Phase namespaces without embedding model aliases in the substrate.
- [ ] Register only capabilities the node is prepared to serve.
- [ ] Paginate and bound discovery results.
- [ ] De-duplicate rendezvous, DHT, mDNS, and configured peer results by PeerId.
- [ ] Treat rendezvous as discovery assistance, not an authority over identity or content.

### Observability

- [ ] Emit connection-path labels: direct, relayed, DCUtR-upgraded, and unknown.
- [ ] Record reservation, renewal, denial, expiry, hole-punch attempt, success, failure reason, and duration.
- [ ] Provide operator metrics for active reservations, streams, bytes, bandwidth, abuse rejections, and resource saturation.
- [ ] Avoid logging prompts, model payload bytes, tokens, or other job content.

## Invariants

1. Relay/rendezvous services use the node’s existing persistent libp2p identity.
2. Discovery through a relay does not weaken signature, manifest, receipt, or policy verification.
3. DCUtR failure does not tear down a healthy relayed request.
4. Stale relay reservations are not advertised as reachable.
5. Relayed traffic is bounded by operator policy.
6. Rendezvous is not treated as a trust root.
7. A node can disable serving as a relay without disabling use of relay clients.
8. No reachability event blocks the swarm driver.

## Network test matrix

| Topology | Required result |
|---|---|
| Same LAN | mDNS/direct path works; relay is unnecessary |
| Public server ↔ NAT client | Direct outbound-established path works |
| NAT requester ↔ NAT server via public relay | Relayed inference works |
| Hole-punch-friendly NAT pair | Relayed connection upgrades through DCUtR |
| Symmetric/hostile NAT | Relay remains functional after DCUtR failure |
| IPv4 requester ↔ dual-stack relay | Supported path works and address family is visible |
| Relay restart | Clients renew/recover through another configured relay |
| Rendezvous unavailable | DHT/configured bootstrap continues with a clear degraded state |

## Failure and security tests

- [ ] Invalid reservation, expired reservation, quota exhaustion, repeated renewals, and connection floods are bounded.
- [ ] Forged observed address and poisoned relay address are rejected or ignored.
- [ ] Relay server cannot be used as an unrestricted general-purpose proxy outside approved Phase protocols.
- [ ] DCUtR success and failure paths preserve PeerId and authenticated transport.
- [ ] Stale rendezvous registration expires and cannot masquerade as current capability.
- [ ] Slow streams and high connection churn do not starve Kademlia queries or reservation renewal.
- [ ] Graceful shutdown withdraws/ends services without corrupting client state.
- [ ] Real consumer-router and VPN/corporate-network tests record actual limitations rather than relying only on simulated NAT.

## Acceptance criteria

- [ ] The three-node coffee-shop scenario succeeds with no port forwarding.
- [ ] A relay-only run completes real LUCID inference and receipt verification.
- [ ] A DCUtR-capable run records successful upgrade and continues the same authenticated peer relationship.
- [ ] A forced hole-punch failure retains a working relayed path.
- [ ] Rendezvous discovers eligible peers and DHT records still resolve content/capabilities independently.
- [ ] Resource limits reject excess load without taking down bootstrap, DHT, or existing healthy streams.
- [ ] Operators can independently enable/disable relay and rendezvous roles.
- [ ] Workspace and network integration QA pass on macOS and Linux.

## Explicit non-goals

- Guaranteeing direct connectivity through every NAT/firewall.
- Using the relay as a generic VPN, HTTP proxy, or content moderation point.
- Treating successful connectivity as authorization to execute work.
- Replacing DNS bootstrap or Kademlia with a centralized rendezvous authority.
- Building global relay deployment automation; that belongs to Network Operations.

## Completion evidence required in the tracker

- Approved reachability/relay-policy ADR.
- Topology diagrams and configuration used for real NAT tests.
- Relay-only and DCUtR-upgrade traces.
- Load/resource-limit evidence.
- Exact commit/PR and task documentation.

## 2026-08-09 approved implementation checkpoint

- Extended the existing `phase-net` swarm with explicit peer/infrastructure roles, circuit-relay client/server, AutoNAT client/server, DCUtR, rendezvous client/server, bounded configuration, and observable direct/relayed paths at `crates/phase-net/src/discovery.rs:395-969`.
- Ordinary LUCID nodes request bounded relay reservations and can refresh rendezvous discovery. Infrastructure mode enables bounded relay/AutoNAT service, but the production daemon keeps rendezvous serving fail-closed until global/per-peer/per-namespace registration quotas exist (`crates/lucidd/src/main.rs:756-765`).
- Deterministic integration coverage proves direct state, relay reservation, exact `Relayed` connection state, rendezvous registration/discovery, and workload traffic over the same swarm. The circuit test explicitly disables DCUtR so a direct upgrade cannot hide the relay path (`crates/phase-net/src/discovery.rs:3879-3969`).
- macOS ARM64 and native Ubuntu x86_64 network suites passed after synchronization was changed from wall-clock races to observed swarm state.
- Remaining before completion: physical coffee-shop/consumer-NAT topology, real DCUtR upgrade, forced hole-punch failure with relay retention, VPN/corporate-network observations, and relay load/resource measurements.
- Evidence and exact fingerprint: [2026-08-09 checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).

## 2026-08-10 approved physical topology

The first real-NAT acceptance run will use three independent roles:

1. A DigitalOcean Ubuntu x86_64 foundation node with Reserved IPv4 runs `lucidd --mode infrastructure` locally and exposes native Phase/libp2p TCP `4001`.
2. Pip, an M1 iMac with 16 GB RAM behind consumer NAT, acts as the inference contributor and establishes outbound relay connectivity. No Sonic router forwarding is assumed.
3. The requester runs from a physically different network. Tailscale may administer the machines but must not carry the acceptance data path.

Web ingress is deliberately separate: DigitalOcean Caddy terminates TCP `80/443` and proxies existing web origins to UMBP over Tailscale. Phase TCP `4001` is not forwarded to UMBP or the Sonic DHCP address. This topology supplies a stable public failure domain without pretending a reverse proxy is a circuit relay.

The topology is approved but not yet evidence. Completion still requires relay-only real inference and receipt verification, a successful DCUtR-capable run, forced hole-punch failure with relay retention, and bounded load/resource observations. Rendezvous acceptance remains separately blocked on admission quotas and a public server deployment.

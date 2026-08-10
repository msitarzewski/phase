# Milestone: Network Operations

**Status:** 🟦 In progress — bounded node/service automation and first external deployment topology approved; provisioning/fleet acceptance pending
**Track:** Intended stream
**Blocking:** Yes
**Depends on:** Reachability Plane
**Tracker:** [README.md](./README.md)

## Outcome

LUCID has an operated, geographically distributed bootstrap/relay/rendezvous layer that a clean installation can use by default, while preserving user choice, independent operation, and clear failure behavior. Operators have reproducible deployment, rotation, backup, upgrade, monitoring, and abuse-response runbooks.

DNS bootstrap already exists but requires an explicit flag (`memory-bank/activeContext.md:165-172`). One foundation relay exists and has a stable identity, but the current plan calls for additional geographic sites and an operator guide (`memory-bank/activeContext.md:159-172`). This milestone turns reachability code into an actual service rather than a lab feature.

## Principles

- Foundation infrastructure is an availability convenience, not a protocol trust root.
- A user can replace or augment the default bootstrap domain and relay list.
- Stable identity, minimal retention, safe limits, and transparent operations matter more than raw node count.
- Deployment instructions must work for an independent operator without access to private foundation automation.
- Geographic distribution means distinct failure domains, not several instances in one provider/region.

## Reuse and integration points

| Responsibility | Extend | Required behavior |
|---|---|---|
| DNS resolver/CLI | `crates/lucidd/src/main.rs:134`, `crates/lucidd/src/main.rs:361-365` | Establish a safe default with override/disable controls |
| Relay service | `crates/lucidd/systemd/lucidd-relay.service` | Generalize the existing unit after reachability roles land |
| Stable identity | `crates/phase-identity/src/storage.rs:25-99`, `crates/phase-net/src/discovery.rs:210-228` | Back up and restore identity securely; never clone one identity across live sites |
| Network config | `crates/phase-net/src/discovery.rs:100-129` | Reuse bootstrap configuration and approved relay-role configuration |
| Current operational record | `memory-bank/activeContext.md:145-172` | Convert the validated single-node knowledge into repeatable runbooks |

## Required operations decisions

- [ ] Default bootstrap decision: enabled by default, explicit opt-out, override and additive sources.
- [ ] DNS record schema/version, TTL, rotation, withdrawal, and emergency revocation.
- [ ] Minimum infrastructure topology: number of sites, provider/region diversity, address families, and capacity target.
- [ ] Operator retention/privacy policy and public service expectations.
- [ ] Identity backup, compromise response, and replacement procedure.
- [ ] Version support window and rolling-upgrade compatibility.
- [ ] Abuse reporting and emergency traffic-shed policy consistent with neutral infrastructure.

## Work packages

### Bootstrap defaults

- [ ] Define the compiled/default bootstrap domain without making it the only source.
- [ ] Support `--no-default-bootstrap`, explicit domains, explicit peers, and offline/LAN-only use.
- [ ] Validate and bound DNS TXT answers before dialing.
- [ ] Cache DNS answers only according to TTL and re-resolve with jitter.
- [ ] Report which bootstrap source succeeded without exposing job content.
- [ ] Fail into an actionable degraded/offline state rather than hanging indefinitely.

### Geographic relay deployment

- [ ] Deploy the approved minimum number of relay/rendezvous nodes across independent regions and preferably providers.
- [ ] Give every node a unique persistent identity and publish attributable coordinates.
- [ ] Configure firewall, IPv4/IPv6 where validated, time synchronization, resource limits, log rotation, and automatic restart.
- [ ] Publish DNS records with staged rollout and low-risk rollback.
- [ ] Validate inter-site DHT connectivity and relay reservation behavior.
- [ ] Document recurring cost, capacity, provider, region, and operational owner without storing credentials in the repository.

### Packaging and service management

- [ ] Provide a versioned systemd unit and environment/config template using the actual production flags.
- [ ] Run under a dedicated non-root account with minimal filesystem/network privileges.
- [ ] Set open-file, process, memory, CPU, and restart limits from measured load.
- [ ] Define safe upgrade, rollback, and identity-preserving migration procedures.
- [ ] Validate a clean-server install end to end from published artifacts.

### Monitoring and alerts

- [ ] Health checks distinguish process, listen, DHT, DNS, reservation, rendezvous, and capacity health.
- [ ] Collect active peers/reservations/streams, traffic, errors, saturation, restart, and protocol-version metrics.
- [ ] Alert on identity changes, DNS mismatch, sustained saturation, renewal failure, disk pressure, and crash loops.
- [ ] Build a public/minimal status view that does not expose peer prompts, model content, IP history beyond operational necessity, or user-level traces.
- [ ] Define log retention and deletion automation.

### Operator documentation

- [ ] Hardware/network prerequisites and cost envelope.
- [ ] DNS and firewall examples.
- [ ] Identity creation, permission checks, encrypted backup, restore, rotation, and compromise handling.
- [ ] Service installation, configuration, upgrade, rollback, and removal.
- [ ] Monitoring, capacity tuning, common failure signatures, and escalation.
- [ ] How an independent operator publishes a relay without becoming foundation-controlled.
- [ ] Consumer troubleshooting for blocked DNS, IPv6 failure, VPNs, captive portals, and relay saturation.

## Operational service levels for qualification

Targets must be measured and approved before release rather than guessed here. At minimum, qualification records:

- Availability window and maintenance behavior for each site.
- Maximum healthy reservations and concurrent streams per instance.
- Sustained and burst bandwidth before control-plane degradation.
- Median and tail bootstrap time from supported regions used in testing.
- Relay-only first-token and total-request latency overhead.
- Recovery time after process restart, node loss, DNS removal, and site loss.
- Log/metric retention and approximate operating cost.

## Failure and security drills

- [ ] Remove one DNS record and verify clients stop dialing it after TTL.
- [ ] Kill one relay during an active request and verify the documented request outcome plus future recovery.
- [ ] Lose an entire region/provider and verify other sites bootstrap new clients.
- [ ] Rotate a compromised relay identity and publish a clear revocation/migration notice.
- [ ] Saturate one relay and verify bounded refusal without control-plane collapse.
- [ ] Supply malicious/oversized DNS TXT records and verify safe rejection.
- [ ] Verify no secrets, private keys, raw prompts, token payloads, or model bytes enter operational telemetry.
- [ ] Install and operate from the public runbook on a clean host by someone other than the author.

## Acceptance criteria

- [ ] A fresh LUCID install joins the public test network with no manually copied peer address.
- [ ] Users can disable, replace, or add bootstrap sources.
- [ ] The approved number of geographically and operationally distinct relays is live and monitored.
- [ ] Relay and rendezvous roles survive reboot and retain identity.
- [ ] Site-loss, DNS-rotation, saturation, and compromise-response drills pass.
- [ ] Independent operator and consumer runbooks pass from clean systems.
- [ ] Public status and privacy/retention statements accurately describe operations.
- [ ] Infrastructure configuration and application release versions are traceable without committing secrets.

## Explicit non-goals

- Guaranteeing zero downtime or global low latency at initial scale.
- Centralizing peer allowlists or model policy at foundation relays.
- Hiding a dependency on foundation DNS; it must be visible and replaceable.
- Embedding infrastructure credentials or private keys in repository configuration.
- Counting multiple machines in one failure domain as geographic resilience.

## Completion evidence required in the tracker

- Approved bootstrap and operator-policy decisions.
- Public relay inventory with PeerIds, regions, roles, and software versions.
- Clean-host operator run result.
- Failure-drill and capacity evidence.
- Exact infrastructure/app commits, release artifacts, and task documentation.

## 2026-08-09 approved implementation checkpoint

- Hardened the existing user-systemd infrastructure service with loopback-only HTTP, operator-owned external address configuration, restart limits, filesystem/device/kernel restrictions, and bounded CPU/memory/task resources at `crates/lucidd/systemd/lucidd-relay.service:1-66`.
- Added a documented non-secret infrastructure environment template at `crates/lucidd/systemd/infrastructure.env.example` and bounded infrastructure-role CLI validation in `crates/lucidd/src/main.rs`.
- Formalized reusable Phase validation profiles and fingerprinted evidence capture in `scripts/phase-validate.sh`; the Linux profile runs offline locked release build, tests, and strict Clippy at `scripts/phase-validate.sh:410-420`.
- Ran the exact source natively on UMBP under a transient resource-bounded user unit. The standing production relay remained active on PID `106521` with zero restarts; the isolated smoke service used loopback port `11435` and was removed after verification.
- Remaining before completion: geographic relay inventory, DNS/default-bootstrap rollout, monitoring/status surface, clean-host independent operator drill, saturation/site-loss/identity-rotation drills, and traceable deployment of a release artifact. Caddy was not changed by this checkpoint.
- Evidence and exact fingerprint: [2026-08-09 checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).

## 2026-08-10 approved first external deployment

The first independent foundation site will be a small DigitalOcean Ubuntu x86_64 host with a Reserved IPv4. It runs Phase infrastructure directly rather than forwarding Phase traffic to UMBP’s DHCP-assigned Sonic address.

### Initial resource envelope

- Recommended starting size: 1 vCPU, 2 GB RAM, and 50 GB disk.
- Functional-but-tight floor: 1 vCPU and 1 GB RAM. This is not the preferred standing size because the current service permits `MemoryMax=1G` before accounting for the OS, journal, Tailscale, and Caddy (`crates/lucidd/systemd/lucidd-relay.service:35-40`).
- No GPU or model storage is required for the foundation relay. Release binaries are built and qualified elsewhere rather than compiled on the host.
- Resize temporarily for saturation work only when measured reservation/stream/bandwidth evidence requires it.

### Port and trust boundaries

- Public TCP `4001` terminates at the local `lucidd --mode infrastructure`; it is the current stable Phase TCP listener (`crates/lucidd/src/main.rs:772-803`).
- Public TCP `80` redirects to HTTPS at local Caddy. Public TCP `443` terminates TLS at local Caddy and may reverse-proxy existing web services to UMBP over Tailscale.
- The Caddy upstream is UMBP’s tailnet identity/address, not the mutable Sonic public address. This stabilizes web ingress but does not make the web origin independent of UMBP.
- The LUCID HTTP API remains bound to `127.0.0.1`; Ollama TCP `11434` is never public (`crates/lucidd/systemd/lucidd-relay.service:28-33`).
- SSH administration uses Tailscale or a tightly restricted source rule. Tailscale is an administration/recovery plane, not Phase acceptance evidence.
- The host receives a unique persistent Phase identity. Reserved-IP reassignment must not clone one live identity onto multiple sites.

### Rollout sequence and current state

1. Complete UMBP’s backup snapshot and Ubuntu 26.04 LTS upgrade.
2. Re-qualify UMBP SSH/Tailscale, Caddy, Docker/Ollama, `lucidd`, firewall, identity, and TCP `4001` before treating it as healthy.
3. Provision the external host, apply firewall/time-sync/resource/log controls, install the exact qualified release artifact, and record its PeerId and external multiaddr.
4. Point public web DNS at the Reserved IP only after Caddy-to-UMBP tailnet proxying is verified. Publish Phase bootstrap/relay coordinates separately.
5. Run the external requester → DigitalOcean relay → Pip contributor matrix and retain attributable evidence.

Provisioning and testing are pending. Public rendezvous serving is also pending: the daemon intentionally disables it because the upstream store lacks the required hard registration quotas (`crates/lucidd/src/main.rs:756-765`). The first site can still provide bootstrap, DHT, bounded circuit relay, and AutoNAT service.

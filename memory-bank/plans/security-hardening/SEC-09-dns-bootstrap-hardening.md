# SEC-09 — DNS bootstrap hardening

**Severity:** 🟡 MEDIUM (M4) | **Phase:** 3 | **Effort:** small (1 agent-wave) | **Status:** planned

## Why
`resolve_dns_bootstrap_peers` (`main.rs:146-196`, added this session) trusts DNS without authentication:
- No DNSSEC enforcement; silent fallback to Cloudflare/Google resolvers on resolv.conf failure (widens the trusted-resolver set without the operator knowing).
- **Every** TXT line starting with `/` is pushed and later dialed (`main.rs:245`) with **no cap** → a spoofed/MITM `bootstrap.phasebased.net` TXT can return thousands of multiaddrs → connection-flood / fd-exhaustion amplification.
- Blast radius of a spoofed record: eclipse-style seeding of the node's initial peer set with attacker nodes. Bounded by libp2p Noise (PeerIDs still can't be impersonated if the multiaddr pins `/p2p/<id>`), but routing/discovery can be skewed and the node coerced into dialing attacker infrastructure.

Note this compounds with SEC-02's hickory-proto CVEs (a malicious resolver can also hang/peg the resolver itself).

## Scope
- `crates/lucidd/src/main.rs` — `resolve_dns_bootstrap_peers`.

## Approach
1. **Cap multiaddrs per domain** — e.g. stop after 64 valid records; log if truncated (no silent unbounded growth).
2. **Require `/p2p/<peer-id>`** in each resolved multiaddr — reject records without a pinned PeerID. This makes the worst a spoofed record can do "make me dial a host," not "make me trust an identity" — Noise rejects the handshake if the host can't prove the pinned PeerID.
3. **Make resolver fallback explicit.** Don't silently fall back to public resolvers; either fail closed, or require an opt-in flag (`--dns-fallback`) and log loudly when used. Document that DNS bootstrap is trust-on-first-use and recommend a trusted resolver / DoH.
4. (Optional, v0.2) consider signing the bootstrap record set (a detached Ed25519 sig in a sibling TXT) so the foundation's seed list is authenticated independent of DNSSEC.

## Acceptance criteria
- At most N (e.g. 64) multiaddrs accepted per domain; excess dropped + logged.
- Multiaddrs without `/p2p/<id>` rejected.
- Resolver fallback is no longer silent (opt-in or fail-closed + loud log).
- The live `--bootstrap-dns bootstrap.phasebased.net` flow still works (the record has a pinned PeerID, so it passes).

## Test plan
- Unit test: a synthetic TXT set of 1000 records → only N kept.
- Unit test: a record without `/p2p/` → rejected.
- Unit test: a well-formed record with `/p2p/<id>` → accepted.
- Live regression: Mac `--bootstrap-dns bootstrap.phasebased.net` still dials umbp.

## Dependencies
None for the caps/validation. The hickory-proto CVE fix is SEC-02 (do that too — they're the same attack surface from different angles).

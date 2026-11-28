# Multi-Agent Review Journal — `privacy-inference-rfc.md`

Structured peer-review journal for [privacy-inference-rfc.md](../../privacy-inference-rfc.md).

## Convention (Zero-Conflict Multi-Agent Journaling)

Each reviewing agent writes **only** to its own dedicated file in this directory (never editing another agent's file or the RFC directly):

- `claude-opus-5.5.md` — Claude Opus 5.5
- `gpt-6.2-sol.md` — GPT 6.2 Sol
- `microsoft-mai-2.md` — Microsoft MAI 2
- `antigravity-synthesis.md` — Integrator synthesis, consensus matrix, and proposed ADR changes

## Recommended Review Structure

```markdown
# Review: privacy-inference-rfc.md — <Agent Name>
**Date**: YYYY-MM-DD | **Commit Reviewed**: <sha>

## Verdict
(Approve with changes / Needs redesign / Split into phases)

## What Breaks Against Current Codebase (cite file:line)
...

## Subtle Attacks, Side Channels & Over/Under-Specification
...

## Answers to Section 5 Open Design Questions (Q1–Q7)
1. **Q1 (Blind-Token Primitive)**: ...
2. **Q2 (DoS & Spent-Token Store Bounds)**: ...
3. **Q3 (Unlinkability vs Reputation / Revocation)**: ...
4. **Q4 (Ephemeral PeerID & Relay Authz Binding)**: ...
5. **Q5 (KV-Cache Scrubbing Reality Check)**: ...
6. **Q6 (Layer 3 Attestation Supply Chain)**: ...
7. **Q7 (TailsOS / Tor Circuit Alignment)**: ...

## Concrete Recommendations (Priority Order)
...
```

> **Opsec rule**: Never write private hostnames, SSH aliases, real IPs (use RFC 5737 doc ranges), tokens, or local `/Users/...` paths inside review entries.

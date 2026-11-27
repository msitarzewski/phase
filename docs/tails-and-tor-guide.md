# TailsOS and Tor: direct LUCID relay connections

This guide is for a LUCID **consumer** that must use outbound TCP through Tor,
including systems where a browser cannot reach the local LUCID API. It uses an
explicit relay address and PeerID instead of DNS TXT bootstrap.

> **Important transport boundary:** `lucidd` does **not** yet implement a built-in
> SOCKS5/Tor transport. The command below relies on **TailsOS's host firewall**
> transparently proxying the logged-in `amnesia` user's outgoing Internet TCP
> traffic through Tor (while dropping UDP/QUIC).
> - Running this exact command on **macOS or ordinary Linux** connects **directly**
>   without Tor and exposes your real IP address (and under `torsocks`, learned
>   QUIC/UDP peer addresses can bypass Tor).
> - On TailsOS, traffic to **local-network private addresses** (such as RFC 1918
>   LAN ranges) is not forced through Tor by Tails' transparent proxy, and `lucidd`
>   currently records peer-reported addresses—so if a serving GPU sits on the same
>   local network, `lucidd` may dial it directly over LAN outside Tor.
> - Tails' transparent TCP proxy also shares one Tor circuit per destination
>   `(IP, port)`; distinct per-identity circuits require explicit SOCKS5
>   `IsolateSOCKSAuth` support (tracked in our post-v0.2 `--tor-socks5` plan).

## What this protects

When Tor carries the consumer's outbound connection, the relay and reachable
serving devices do not see the consumer's ordinary network location. TailsOS
remains amnesic: an ordinary reboot does not retain the consumer's LUCID
identity, chat history, or downloaded state unless an operator deliberately
persists it.

This is **not prompt confidentiality from the serving GPU**. The serving peer
must receive and process the prompt, and the local operator of that GPU can
observe process memory, model inputs, outputs, or logs. Tor also does not hide
content from the selected serving device, prove correct model execution, or
protect a consumer who deliberately discloses identifying information in a
prompt. Use an operator-approved, trusted serving circle for sensitive work.

## Consumer workflow

Use the relay's literal public IPv4 address, TCP port, and pinned PeerID:

```bash
lucidd \
  --mode consume-only \
  --outbound-only \
  --no-default-bootstrap \
  --bootstrap-peer /ip4/<RELAY_IP>/tcp/<PORT>/p2p/<RELAY_PEER_ID> \
  --relay-peer /ip4/<RELAY_IP>/tcp/<PORT>/p2p/<RELAY_PEER_ID>
```

`--mode consume-only` disables a local worker. `--outbound-only` disables direct
inbound listeners, local multicast discovery, and hole punching. The identical
explicit `--bootstrap-peer` and `--relay-peer` values pin both startup bootstrap
and circuit relay to the operator-approved relay identity.

Do not add a `--bootstrap-dns` option in this workflow. `--no-default-bootstrap`
prevents the built-in TXT bootstrap. TXT resolution needs DNS, and Tor's
support for network-name resolution is commonly limited to A/AAAA records rather
than TXT queries. A literal `/ip4/...` multiaddr also avoids the additional
name-resolution trust decision.

Use only the TCP multiaddr above. Do not substitute `/quic-v1`, and do not rely
on UDP, mDNS, or LAN multicast: those paths are not the explicit outbound-only
Tor path and can fail or escape the intended transport boundary.

## Relay port 443

An operator of a dedicated Linux relay host can opt in to TCP 443:

```bash
sudo ./scripts/setup-cloud-relay.sh \
  --ip 203.0.113.10 \
  --port 443
```

This example uses an RFC 5737 documentation address; replace it with the
operator's actual public IPv4 address at deployment time. Port 443 is useful
for Tor exits and restrictive firewalls that allow only familiar outbound
ports. Do not use it on a host where another service already owns 443.

Root is used only during installation. The resulting system service still runs
as `User=lucid`; systemd grants only `CAP_NET_BIND_SERVICE` for the low-port bind.
The default remains unprivileged TCP 4001, and `--port 443` is rejected in
rootless setup mode.

## Authorization and routing

The direct relay connection does **not** authorize every advertising peer. LUCID
consumer routing is default-deny: model requests route only to explicitly
enrolled or pinned operator-approved circle devices. Keep enrollment limited to
the devices the consumer is meant to trust, and verify each relay/serving PeerID
out of band before adding it.

The built-in desktop chat records message history only in memory. It does not
use local storage or write chat history to disk. Closing the chat application or
rebooting a TailsOS session discards that in-memory history. Separately, lucidd
may maintain operational configuration, identity, logs, or downloaded model
state; the TailsOS persistence choice for those files determines what survives.

## Ephemeral identity versus repeatable identity

Choose deliberately between these operational modes:

- **Ephemeral each boot:** keep lucidd's state on the amnesic volume and do not
  persist its identity/configuration. This reduces cross-boot linkability, but
  the consumer gets a fresh PeerID and must repeat enrollment and setup.
- **Tails Dotfiles persistence:** persist only the required lucidd state through
  Tails Dotfiles. The same identity is convenient and remains recognizable to
  enrolled peers across boots, but it creates durable cross-boot linkability
  and leaves persisted state on the configured removable volume.

Do not store relay secrets, invitation credentials, or prompt transcripts in
Dotfiles. Re-enroll intentionally after a fresh identity, and remove persisted
state when the linkability tradeoff is no longer acceptable.

## Troubleshooting checklist

1. Confirm the relay's PeerID and the chosen TCP port through an operator-owned
   channel; never infer or guess a PeerID.
2. Confirm `<PORT>` in both multiaddrs matches the relay listener.
3. Keep the consumer command on `--outbound-only` and retain
   `--no-default-bootstrap`.
4. Confirm no `/dns4/...`, TXT lookup, `/quic-v1`, mDNS, or UDP dependency was
   substituted into the pinned multiaddrs.
5. Treat relay reachability, enrollment, and serving-GPU authorization as three
   separate checks. A reachable relay does not make an arbitrary peer trusted.

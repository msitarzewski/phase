# Run a Cloud Relay and Tor/Tails guidance

Peers behind residential NATs or cellular carriers reach each other through a lightweight **Circuit Relay v2** node (`--mode infrastructure`) and upgrade to direct connections via DCUtR hole punching whenever possible. A relay only forwards Noise-encrypted bytes and never sees plaintext prompts or responses.

## One-command Linux Cloud Relay setup

Any small Linux VPS with a public IPv4 address (~24 MiB RSS in practice) can serve as a private or community relay using `scripts/setup-cloud-relay.sh`:

```bash
curl -fsSL https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/setup-cloud-relay.sh -o setup-cloud-relay.sh
sudo bash setup-cloud-relay.sh --ip 203.0.113.10 --port 443
```

What the script sets up (fail-closed):

- Rejects loopback, RFC 1918, link-local, and CGNAT addresses for `--ip`.
- Downloads the verified `lucidd` release bundle (`SHA256SUMS.txt` checked).
- Creates an unprivileged system user `lucid` (`/usr/sbin/nologin`).
- Installs a hardened systemd unit (`/etc/systemd/system/lucidd-relay.service`) with `NoNewPrivileges=yes`, `ProtectSystem=strict`, `MemoryDenyWriteExecute=yes`, and—only when `--port 443` is selected—`AmbientCapabilities=CAP_NET_BIND_SERVICE` so restrictive corporate/hotel firewalls that block port `4001` still pass traffic.
- Opens the chosen TCP port in `ufw` (unless `--no-firewall` is passed).
- Prints the node's exact PeerID-pinned multiaddr and DNS TXT record value.

> [!NOTE]
> Running without `sudo` installs a user systemd unit with `loginctl enable-linger` on port `4001` (port `443` requires root for `CAP_NET_BIND_SERVICE`). Public rendezvous serving stays disabled in `lucidd` infrastructure mode until upstream registration-count quotas are available.

### Hardened relay limits in `phase-net`

Circuit Relay v2 server mode is strictly bounded against abuse:

- Default reservations: `128` total, `2` per peer, `1 hour` duration (hard ceiling `1024` / `8` / `1h`).
- Default circuits: `256` total, `4` per peer, `2 minute` duration, `64 MiB` per circuit (hard ceiling `2048` / `16` / `1h` / `1 GiB`).

## PeerID-pinned DNS TXT bootstrap

By default, `lucidd` resolves TXT records on `bootstrap.phasebased.net` via your system DNS resolver (`--bootstrap-dns`, up to 64 addresses per domain). Every TXT record must end with `/p2p/<PeerId>`, which Noise authenticates on dial.

To run your own private relay without touching the default bootstrap domain:

```bash
lucidd \
  --no-default-bootstrap \
  --bootstrap-peer /ip4/203.0.113.10/tcp/443/p2p/12D3KooWYourRelayPeerId \
  --relay-peer /ip4/203.0.113.10/tcp/443/p2p/12D3KooWYourRelayPeerId
```

Or publish a TXT record on your own domain (`_dns` value `/ip4/203.0.113.10/tcp/443/p2p/12D3KooW...`) and pass `--no-default-bootstrap --bootstrap-dns bootstrap.example.org`.

> [!WARNING]
> System DNS TXT lookups do not enforce DNSSEC today; `--dns-fallback` (off by default) falls back to `1.1.1.1` only if you opt in. For maximum bootstrap certainty, pin `--no-default-bootstrap --relay-peer <multiaddr>` directly.

## Running over Tor and TailsOS

Native `--tor-socks5` support is not yet built into `phase-net`. Today, privacy-focused users can run `lucidd` in **consume-only outbound-only** mode behind an OS-level transparent Tor proxy such as TailsOS or Whonix:

```bash
lucidd \
  --mode consume-only \
  --outbound-only \
  --no-default-bootstrap \
  --bootstrap-peer /ip4/203.0.113.10/tcp/443/p2p/12D3KooWYourRelayPeerId \
  --relay-peer /ip4/203.0.113.10/tcp/443/p2p/12D3KooWYourRelayPeerId
```

What `--outbound-only` disables automatically in `phase-net`:

- Zero direct TCP/QUIC listen sockets
- mDNS LAN broadcast discovery disabled
- DCUtR hole punching disabled (prevents leaking your real IP during direct-upgrade attempts)
- AutoNAT client probes disabled

### Honest Tor & TailsOS caveats

1. **Serving peers still see plaintext prompts:** Tor hides your IP address from the relay and serving peer; it does *not* hide your prompt text from the serving peer executing inference.
2. **Same-LAN RFC 1918 addresses:** TailsOS's firewall blocks non-Tor internet traffic, but carefully audit your local network rules and avoid dialing private LAN multiaddrs if you require strict Tor-only routing.
3. **Use TCP port `443` relays:** Tor exits only carry TCP (never UDP/QUIC) and commonly allow port `443` cleanly.
4. **Use an ephemeral identity** (`--identity-path /tmp/ephemeral.key`) if you do not want sessions linked across reboots by `PeerId`.

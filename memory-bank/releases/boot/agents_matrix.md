# Agents Matrix (who does what)

| Area        | Agent/Owner     | Responsibilities                                         |
|-------------|------------------|----------------------------------------------------------|
| Bootloader  | Systems Agent    | ESP layout, systemd-boot/GRUB, menu, fallback paths     |
| Kernel      | Kernel Agent     | Configs per arch, DTBs, drivers                          |
| Initramfs   | Tooling Agent    | Busybox base, net bring-up, kexec-tools                  |
| Discovery   | Networking Agent | mDNS + libp2p/Kademlia integration                       |
| Security    | Security Agent   | Manifest schema, signing, verification, CAS policies     |
| Fetch       | Transport Agent  | HTTPS/IPFS fetch, mirrors, retry/backoff                 |
| Runtime     | Runtime Agent    | Plasm post-boot, WASM hello job, receipts                |
| Packaging   | Release Agent    | Images, checksums, signatures, reproducibility           |
| Docs        | Docs Agent       | Quickstarts, troubleshooting, threat model, Secure Boot  |

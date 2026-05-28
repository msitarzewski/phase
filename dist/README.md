# dist

Pre-built binaries for distribution. One subdirectory per `<crate>-<version>-<target-triple>`.

Targets currently shipped:
- `aarch64-apple-darwin` — Mac (M-series). Built natively via `cargo build --release`.
- `aarch64-unknown-linux-gnu` — Linux ARM64 (Parallels VMs on Apple Silicon, Raspberry Pi 5, AWS Graviton).
- `x86_64-unknown-linux-gnu` — Linux x86_64 (Intel/AMD servers, cloud).

To rebuild:
```bash
# Mac native
cargo build --release -p lucidd
cp target/release/lucidd dist/lucidd-0.1.0-aarch64-apple-darwin/

# Linux ARM64 via cross (Docker)
cross build --release -p lucidd --target aarch64-unknown-linux-gnu
cp target/aarch64-unknown-linux-gnu/release/lucidd dist/lucidd-0.1.0-aarch64-unknown-linux-gnu/

# Linux x86_64 via cross
cross build --release -p lucidd --target x86_64-unknown-linux-gnu
cp target/x86_64-unknown-linux-gnu/release/lucidd dist/lucidd-0.1.0-x86_64-unknown-linux-gnu/
```

Layout (per target):
```
dist/lucidd-0.1.0-<target>/
├── lucidd          # the binary
├── README.md       # install + run
└── LICENSE         # AGPL-3.0-or-later
```

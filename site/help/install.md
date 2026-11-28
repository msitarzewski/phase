# Install LUCID

LUCID ships as a **Desktop app** (macOS Apple Silicon and Ubuntu/Debian Linux) and a standalone **`lucidd` CLI daemon** (macOS Apple Silicon, Linux `x86_64`, Linux `aarch64`, and Windows `x86_64`). Every release bundle is verified against `SHA256SUMS.txt` before installation.

## macOS (Apple Silicon, macOS 14+)

### Desktop app (recommended)

1. Open [GitHub Releases](https://github.com/msitarzewski/phase/releases) and download `LUCID_0.2.0_aarch64.dmg`.
2. Open the disk image and drag **LUCID** into **Applications**.
3. Launch **LUCID** from Applications. It starts its own bundled `lucidd` sidecar on `127.0.0.1:11435` (falling back to an ephemeral loopback port if `:11435` is busy) and never runs as root.

### Terminal installer (CLI + optional `.app`)

Install `~/.local/bin/lucidd` and `~/Applications/LUCID.app` with automatic SHA-256 and `codesign --verify --deep --strict` verification:

```bash
curl -fsSL https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/install-lucid.sh | bash -s -- --app
```

CLI only (omitting the Desktop app):

```bash
curl -fsSL https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/install-lucid.sh | bash
```

## Linux (`x86_64` and `aarch64`)

### Ubuntu / Debian Desktop (`.deb`)

Download the `.deb` package for your architecture (`amd64` or `arm64`) from [GitHub Releases](https://github.com/msitarzewski/phase/releases) and install it with `apt` so WebKitGTK and GTK 3 dependencies (`libwebkit2gtk-4.1-0`, `libgtk-3-0`) are resolved automatically:

```bash
sudo apt install ./LUCID_0.2.0_amd64.deb
```

### Linux CLI installer (`x86_64` & `aarch64`)

The `x86_64` release binary is compiled for baseline `x86-64` (`-mno-bmi -mno-bmi2 -mno-avx2`) so it runs with zero `SIGILL` crashes even on 2012-era Intel/AMD CPUs:

```bash
curl -fsSL https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/install-lucid.sh | bash
```

Useful installer flags in `scripts/install-lucid.sh`:

- `--tag vX.Y.Z` — pin an exact GitHub release tag instead of `latest`
- `--from-dir /path/to/dir` — install offline from a local directory containing the archive and `SHA256SUMS.txt`
- `--prefix ~/.local` — choose the install root (default `~/.local`, binary in `~/.local/bin/lucidd`)
- `--install-systemd` — install the user-level `lucidd-relay.service` unit
- `--setup-relay 203.0.113.10` — write a mode-`0600` `infrastructure.env` for a relay host

## Windows (`x86_64` CLI)

Install `lucidd.exe` to `%LOCALAPPDATA%\LUCID\bin` and add it to your user `PATH`:

```powershell
irm https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/install-lucid.ps1 | iex
```

`scripts/install-lucid.ps1` fails closed on checksum mismatch, rejects zip path traversal or drive-prefixed archive entries, smoke-tests `lucidd.exe --help`, and exits cleanly without touching system-wide directories.

To pin a specific release tag or install from a local folder:

```powershell
.\scripts\install-lucid.ps1 -Tag v0.2.0-rc12
.\scripts\install-lucid.ps1 -FromDir C:\Downloads\lucid-release
```

## Verify checksums manually

Every GitHub Release includes a single `SHA256SUMS.txt` covering all `.tar.gz`, `.zip`, `.dmg`, and `.deb` assets.

On macOS:

```bash
shasum -a 256 -c --ignore-missing SHA256SUMS.txt
```

On Linux:

```bash
sha256sum -c --ignore-missing SHA256SUMS.txt
```

On Windows PowerShell:

```powershell
Get-FileHash -Algorithm SHA256 .\lucidd-0.2.0-x86_64-pc-windows-msvc.zip
```

## Build from source

Requires Rust `1.88+` (workspace edition `2021`). To build and test the Rust workspace (all 8 crates, including `lucidd` and `plasmd`):

```bash
git clone https://github.com/msitarzewski/phase.git
cd phase
cargo build --release --locked
cargo test --workspace --locked
```

To build LUCID Desktop (requires Node.js 22+ and Tauri v2 platform dependencies):

```bash
cd apps/lucid-desktop
bash stage-sidecar.sh ../../target/release/lucidd
npm ci
npm run check
npx tauri build
```

> [!TIP]
> Ready to send your first prompt or connect to a friend's GPU? Continue to [Your first chat and joining a circle](first-chat.md).

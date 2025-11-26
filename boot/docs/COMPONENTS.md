# Phase Boot Components Reference

**Version:** 0.1.0-M2
**Last Updated:** 2025-11-26

---

## Table of Contents

1. [Binaries](#binaries)
   - [phase-discover](#phase-discover)
   - [phase-verify](#phase-verify)
   - [phase-fetch](#phase-fetch)
2. [Scripts](#scripts)
   - [init](#init)
   - [kexec-boot.sh](#kexec-bootsh)
   - [mode-handler.sh](#mode-handlersh)
   - [overlayfs-setup.sh](#overlayfs-setupsh)
   - [plasm-init.sh](#plasm-initsh)
3. [Configuration Files](#configuration-files)
   - [manifest.schema.json](#manifestschemajson)
4. [Directory Reference](#directory-reference)

---

## Binaries

### phase-discover

**Purpose**: Boot-time manifest discovery via libp2p Kademlia DHT
**Language**: Rust
**Source**: `/home/user/phase/daemon/src/bin/phase_discover.rs`
**Binary Location**: `/bin/phase-discover` (in initramfs)

#### Usage

```bash
phase-discover [OPTIONS]

Options:
  -a, --arch <ARCH>              Target architecture [default: x86_64]
                                 Values: x86_64, arm64

  -c, --channel <CHANNEL>        Update channel [default: stable]
                                 Values: stable, testing, nightly

  --ephemeral                    Use ephemeral identity (Private Mode)
                                 Generates one-time Ed25519 keypair

  -b, --bootstrap <MULTIADDR>... Bootstrap nodes (multiaddr format)
                                 Example: /ip4/1.2.3.4/tcp/4001/p2p/12D3Koo...

  -t, --timeout <SECONDS>        Discovery timeout [default: 30]

  -f, --format <FORMAT>          Output format [default: text]
                                 Values: text, json

  -q, --quiet                    Quiet mode (only output manifest URL)

  -h, --help                     Print help
  -V, --version                  Print version
```

#### Examples

```bash
# Standard discovery (Internet Mode)
phase-discover --arch x86_64 --channel stable

# Private Mode (ephemeral identity)
phase-discover --arch x86_64 --channel stable --ephemeral

# Custom bootstrap nodes
phase-discover --arch x86_64 --channel stable \
  --bootstrap /ip4/192.168.1.10/tcp/4001/p2p/12D3KooWABC...

# JSON output
phase-discover --arch x86_64 --channel stable --format json

# Quiet mode (for scripting)
phase-discover --arch x86_64 --channel stable --quiet > /tmp/manifest_url
```

#### Output Formats

**Text Format** (default):
```
Local peer ID: 12D3KooWXYZ...
Looking up: /phase/stable/x86_64/manifest
Added bootstrap node: 12D3KooWABC...
Started DHT query: QueryId(1)
Listening on: /ip4/0.0.0.0/tcp/54321
Connected to: 12D3KooWABC...
Bootstrap successful
Discovered peer: 12D3KooWDEF...
Found manifest!
MANIFEST_URL=https://cdn.phase.dev/manifests/stable-x86_64-1234.json
```

**JSON Format**:
```json
{
  "key": "/phase/stable/x86_64/manifest",
  "manifest_url": "https://cdn.phase.dev/manifests/stable-x86_64-1234.json",
  "peer_id": "12D3KooWXYZ...",
  "provider_count": 1
}
```

**Quiet Mode**:
```
https://cdn.phase.dev/manifests/stable-x86_64-1234.json
```

#### Implementation Details

**DHT Key Construction** (`phase_discover.rs:109`):
```rust
let manifest_key = format!("/phase/{}/{}/manifest", args.channel, args.arch);
// Example: "/phase/stable/x86_64/manifest"
```

**Identity Generation** (`phase_discover.rs:88-101`):
- **Ephemeral Mode** (`--ephemeral`): Generates new Ed25519 keypair each run
- **Session Mode** (default): Generates new keypair per boot (no persistent storage yet)

**libp2p Configuration** (`phase_discover.rs:115-130`):
- **Transport**: TCP with Noise encryption and Yamux multiplexing
- **Kademlia Mode**: Client (does not store records or route queries)
- **Store**: MemoryStore (ephemeral, in-memory only)
- **Idle Timeout**: 60 seconds

**Bootstrap Process** (`phase_discover.rs:136-158`):
1. Add bootstrap nodes to Kademlia routing table
2. Call `kademlia.bootstrap()` to discover peers
3. Wait for `QueryResult::Bootstrap` event

**Query Process** (`phase_discover.rs:161-206`):
1. Create `RecordKey` from manifest key
2. Call `kademlia.get_record(key)`
3. Wait for `QueryResult::GetRecord` event
4. Extract manifest URL from record value (UTF-8 string)
5. Output URL and exit with code 0

**Error Handling**:
- **Timeout**: Exit code 1 after `--timeout` seconds
- **No Bootstrap Nodes**: Warning, but continues (may fail)
- **Query Failed**: Exit code 1 with error message
- **Parse Error**: Exit code 1 if record value is not valid UTF-8

**Exit Codes**:
- `0`: Success, manifest URL found
- `1`: Failure (timeout, query error, no record found)

---

### phase-verify

**Purpose**: Boot manifest signature verification with rollback protection
**Language**: Rust
**Source**: `/home/user/phase/daemon/src/bin/phase_verify.rs`
**Binary Location**: `/bin/phase-verify` (in initramfs)

#### Usage

```bash
phase-verify --manifest <PATH> [OPTIONS]

Options:
  -m, --manifest <PATH>          Path to manifest JSON file (required)

  -k, --key <PATH>               Path to targets public key
                                 (optional, uses embedded key if not provided)

  --check-version <PATH>         Path to cached version file for rollback protection
                                 Example: /cache/phase/version

  --update-version               Update cached version after successful verification

  -f, --format <FORMAT>          Output format [default: text]
                                 Values: text, json

  -q, --quiet                    Quiet mode (exit code only)

  -h, --help                     Print help
  -V, --version                  Print version
```

#### Examples

```bash
# Verify manifest with embedded key
phase-verify --manifest manifest.json

# Verify with custom public key
phase-verify --manifest manifest.json --key targets.pub

# Verify with rollback protection
phase-verify --manifest manifest.json --check-version /cache/phase/version

# Verify and update cached version
phase-verify --manifest manifest.json \
  --check-version /cache/phase/version \
  --update-version

# JSON output
phase-verify --manifest manifest.json --format json

# Quiet mode (for scripting)
if phase-verify --manifest manifest.json --quiet; then
  echo "Manifest verified"
else
  echo "Verification failed"
  exit 1
fi
```

#### Output Formats

**Text Format** (default):
```
Verifying manifest v1234 (stable/x86_64)
Signature verified (keyid: targets-key-2025)
VERIFIED
  Version: 1234
  Channel: stable
  Arch:    x86_64
  Key:     targets-key-2025
```

**JSON Format**:
```json
{
  "status": "VERIFIED",
  "manifest_version": 1234,
  "channel": "stable",
  "arch": "x86_64",
  "key_id": "targets-key-2025"
}
```

**Failure Output** (text):
```
Verifying manifest v1234 (stable/x86_64)
Signature invalid (keyid: targets-key-2025)
FAILED
  Error: No valid signature found
```

**Failure Output** (JSON):
```json
{
  "status": "FAILED",
  "manifest_version": 1234,
  "channel": "stable",
  "arch": "x86_64",
  "key_id": "none",
  "error": "No valid signature found"
}
```

#### Implementation Details

**Embedded Root Key** (`phase_verify.rs:113`):
```rust
const EMBEDDED_ROOT_KEY: &[u8] = include_bytes!("../../keys/root.pub.placeholder");
```
- Compiled into binary at build time
- Used when `--key` option not provided
- Production builds will embed actual root public key

**Public Key Parsing** (`phase_verify.rs:258-285`):

Supports three formats:
1. **Raw 32-byte Ed25519 key** (binary)
2. **Hex-encoded** (64 hexadecimal characters)
3. **Base64-encoded** (standard base64)

**Signature Verification** (`phase_verify.rs:289-313`):

```rust
fn verify_signature(data_b64: &str, sig_b64: &str, key: &VerifyingKey) -> Result<bool> {
    // 1. Decode base64 signed data
    let data = BASE64.decode(data_b64)?;

    // 2. Decode base64 signature (64 bytes)
    let sig_bytes = BASE64.decode(sig_b64)?;
    let signature = Signature::from_bytes(&sig_array);

    // 3. Hash the data (pre-hash signing with SHA256)
    let hash = Sha256::digest(&data);

    // 4. Verify Ed25519 signature over hash
    match key.verify(&hash, &signature) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}
```

**Rollback Protection** (`phase_verify.rs:141-163`):

```rust
if let Some(version_path) = &args.check_version {
    if version_path.exists() {
        let cached_version: u64 = fs::read_to_string(version_path)?.trim().parse()?;

        if manifest.manifest_version < cached_version {
            // ROLLBACK DETECTED - FAIL VERIFICATION
            return Err(anyhow!(
                "Rollback detected: manifest v{} < cached v{}",
                manifest.manifest_version, cached_version
            ));
        }
    }
}
```

**Version Cache Update** (`phase_verify.rs:222-232`):
```rust
if args.update_version {
    if let Some(version_path) = &args.check_version {
        fs::write(version_path, manifest.manifest_version.to_string())?;
    }
}
```

**Multi-Signature Support** (`phase_verify.rs:194-218`):

Manifest can contain multiple signatures. Verification succeeds if **at least one** signature is valid:

```rust
for sig in &manifest.signatures {
    match verify_signature(&manifest.signed.data, &sig.sig, &verifying_key) {
        Ok(true) => {
            verified = true;
            verified_keyid = sig.keyid.clone();
            break;  // One valid signature is sufficient
        }
        // ... try next signature
    }
}
```

**Exit Codes**:
- `0`: Verification successful
- `1`: Verification failed (invalid signature, rollback, parsing error)

---

### phase-fetch

**Purpose**: Boot artifact downloader with SHA256 verification
**Language**: Rust
**Source**: `/home/user/phase/daemon/src/bin/phase_fetch.rs`
**Binary Location**: `/bin/phase-fetch` (in initramfs)

#### Usage

```bash
phase-fetch --manifest <PATH> --output <DIR> [OPTIONS]

Options:
  -m, --manifest <PATH>          Path to verified manifest JSON file (required)

  -o, --output <DIR>             Output directory for artifacts (required)

  -a, --artifact <NAME>          Specific artifact to fetch [default: all]
                                 Values: kernel, initramfs, rootfs, all

  -r, --retry <COUNT>            Retry count per artifact [default: 3]

  -t, --timeout <SECONDS>        Per-download timeout [default: 300]

  -q, --quiet                    Quiet mode (minimal output)

  -h, --help                     Print help
  -V, --version                  Print version
```

#### Examples

```bash
# Download all artifacts
phase-fetch --manifest manifest.json --output /boot

# Download only kernel
phase-fetch --manifest manifest.json --output /boot --artifact kernel

# Download with custom retry and timeout
phase-fetch --manifest manifest.json --output /boot \
  --retry 5 --timeout 600

# Quiet mode
phase-fetch --manifest manifest.json --output /boot --quiet
```

#### Output

```
Fetching artifacts from manifest v1234 (stable/x86_64)
Fetching kernel (52428800 bytes, hash: sha256:abcd1234...)
  Downloading from https://cdn.phase.dev/stable/kernel-x86_64-1234.img...
  Downloaded 10 MB...
  Downloaded 20 MB...
  Downloaded 30 MB...
  Downloaded 40 MB...
  Downloaded 50 MB...
  Hash verified: sha256:abcd1234
Successfully fetched kernel
Fetching initramfs (10485760 bytes, hash: sha256:ef567890...)
  Downloading from https://cdn.phase.dev/stable/initramfs-x86_64-1234.img...
  Downloaded 10 MB...
  Hash verified: sha256:ef567890
Successfully fetched initramfs
All artifacts fetched successfully:
  kernel -> /boot/kernel (52428800 bytes)
  initramfs -> /boot/initramfs (10485760 bytes)
```

#### Implementation Details

**Manifest Parsing** (`phase_fetch.rs:100-109`):
```rust
let manifest: Manifest = serde_json::from_str(&manifest_content)?;
// Extract: manifest_version, channel, arch, artifacts{}
```

**Multi-URL Fallback** (`phase_fetch.rs:189-243`):

For each artifact, tries all URLs with retry logic:
1. Try URL 1 with retry count
2. If all retries fail, try URL 2 with retry count
3. Continue until success or all URLs exhausted
4. Exponential backoff between retries: 2^attempt seconds

```rust
for (url_idx, url) in artifact.urls.iter().enumerate() {
    for attempt in 0..retry_count {
        match download_and_verify(url, &hash, size, &output_path, timeout) {
            Ok(_) => return Ok(result),
            Err(e) => {
                // Exponential backoff: 2^attempt seconds
                let backoff_secs = 2_u64.pow(attempt);
                thread::sleep(Duration::from_secs(backoff_secs));
            }
        }
    }
    // Try next URL
}
```

**Streaming Hash Verification** (`phase_fetch.rs:246-348`):

Downloads file while computing SHA256 hash:

```rust
let mut response = client.get(url).send()?;
let mut file = File::create(&temp_path)?;
let mut hasher = Sha256::new();
let mut total_bytes = 0u64;
let mut buffer = [0u8; 8192];

loop {
    let bytes_read = response.read(&mut buffer)?;
    if bytes_read == 0 { break; }

    file.write_all(&buffer[..bytes_read])?;
    hasher.update(&buffer[..bytes_read]);  // Stream through hash
    total_bytes += bytes_read as u64;
}

// Verify size
if total_bytes != expected_size {
    return Err(anyhow!("Size mismatch"));
}

// Verify hash
let computed_hash = hex::encode(hasher.finalize());
if computed_hash != expected_hash {
    return Err(anyhow!("Hash mismatch"));
}
```

**Atomic Write** (`phase_fetch.rs:262, 344-345`):

Downloads to `.tmp` file, verifies, then atomically renames:
```rust
let temp_path = output_path.with_extension("tmp");
// ... download and verify ...
fs::rename(&temp_path, output_path)?;  // Atomic on same filesystem
```

**Hash File Creation** (`phase_fetch.rs:212-213`):

Creates `.sha256` file alongside each artifact:
```
/boot/kernel
/boot/kernel.sha256  (contains: "sha256:abcd1234...")
```

**Content-Length Validation** (`phase_fetch.rs:278-286`):

If server provides `Content-Length` header, validates before download:
```rust
if let Some(content_length) = response.content_length() {
    if content_length != expected_size {
        return Err(anyhow!("Size mismatch (pre-download check)"));
    }
}
```

**Progress Indication** (`phase_fetch.rs:308-311`):

Logs progress every 10 MB:
```rust
if !quiet && total_bytes % (10 * 1024 * 1024) == 0 {
    info!("  Downloaded {} MB...", total_bytes / (1024 * 1024));
}
```

**Error Handling**:
- **HTTP Error**: Tries next URL
- **Size Mismatch**: Deletes temp file, tries next URL/retry
- **Hash Mismatch**: Deletes temp file, tries next URL/retry
- **Network Timeout**: Tries next retry with backoff
- **All URLs Failed**: Exit code 1

**Exit Codes**:
- `0`: All requested artifacts downloaded and verified
- `1`: One or more artifacts failed to download

---

## Scripts

### init

**Purpose**: PID 1 init script - first userspace process
**Language**: POSIX shell (`/bin/sh`)
**Source**: `/home/user/phase/boot/initramfs/init`
**Location**: `/init` (in initramfs root)

#### Responsibilities

1. **Mount Essential Filesystems** (`init:27-43`)
   - `/proc` - Process information
   - `/sys` - Kernel and device information
   - `/dev` - Device nodes (devtmpfs)
   - `/run` - Runtime data (tmpfs)

2. **Parse Kernel Command Line** (`init:49-87`)
   - Extract `phase.mode=` (internet, local, private)
   - Extract `phase.channel=` (stable, testing, nightly)
   - Extract `phase.cache=` (true, false)
   - Extract `phase.nowrite=` (true, false)

3. **Initialize Network** (`init:109-158`)
   - Bring up loopback interface
   - Configure first available network interface with DHCP
   - Write network status to `/tmp/network.status`
   - Uses modular `net-init.sh` if available, fallback to basic init

4. **Run Discovery** (`init:164-202`)
   - Execute `phase-discover` if network available
   - Write manifest URL to `/tmp/manifest_url`
   - Skip in Local Mode (uses cache)

5. **Print Boot Banner** (`init:208-247`)
   - Display Phase Boot logo
   - Show configuration (mode, network, channel)
   - List available commands

6. **Execute Shell** (`init:300-301`)
   - `exec /bin/sh` - Become shell (remains PID 1)
   - (M3+: Will instead execute mode handler → kexec)

#### Configuration Variables

```bash
PHASE_VERSION="0.1.0-M2"        # Phase Boot version
SCRIPTS_DIR="/scripts"          # Location of boot scripts
PHASE_MODE="internet"           # Boot mode (parsed from cmdline)
PHASE_CHANNEL=""                # Update channel (parsed from cmdline)
PHASE_CACHE="true"              # Cache enabled (parsed from cmdline)
PHASE_NOWRITE="false"           # No-write mode (parsed from cmdline)
```

#### Kernel Cmdline Parsing

Parses `/proc/cmdline` for Phase-specific parameters:

```bash
phase.mode=internet      # Boot mode
phase.channel=stable     # Update channel
phase.cache=true         # Enable caching
phase.nowrite=true       # Read-only mode (Private Mode)
```

Example cmdline:
```
console=tty0 phase.mode=internet phase.channel=stable phase.cache=true
```

#### Network Initialization

**Modular (Preferred)** (`init:113-124`):
- Calls `/scripts/net-init.sh` if available
- Reads status from `/tmp/network.status`
- Reads interface from `/tmp/network.interface`
- Reads IP from `/tmp/network.ip`

**Fallback (Basic)** (`init:126-154`):
- Brings up loopback: `ip link set lo up`
- Iterates `/sys/class/net/*`
- Brings up first non-loopback interface
- Runs DHCP client (`udhcpc` or `dhcpcd`)

#### Discovery Execution

```bash
if /bin/phase-discover $DISCOVER_ARGS --timeout 30 --quiet > /tmp/manifest_url; then
    MANIFEST_URL=$(cat /tmp/manifest_url)
    echo "Discovered manifest: $MANIFEST_URL"
else
    echo "Discovery failed or timed out"
fi
```

#### Cleanup Handler

Registered via `trap cleanup EXIT`:
```bash
cleanup() {
    umount /run 2>/dev/null || true
    umount /dev 2>/dev/null || true
    umount /sys 2>/dev/null || true
    umount /proc 2>/dev/null || true
}
```

---

### kexec-boot.sh

**Purpose**: Load and execute new kernel via kexec
**Language**: POSIX shell
**Source**: `/home/user/phase/boot/initramfs/scripts/kexec-boot.sh`
**Location**: `/scripts/kexec-boot.sh` (in initramfs)

#### Usage

```bash
kexec-boot.sh --kernel <PATH> --initramfs <PATH> [OPTIONS]

Required:
  --kernel PATH       Path to kernel image (vmlinuz, bzImage, etc.)
  --initramfs PATH    Path to initramfs image (initramfs.img, initrd.img, etc.)

Optional:
  --cmdline "..."     Additional kernel command line parameters
  --dtb PATH          Path to device tree blob (ARM/ARM64 only)
```

#### Examples

```bash
# Basic x86_64 boot
kexec-boot.sh --kernel /boot/vmlinuz --initramfs /boot/initramfs.img

# With custom cmdline
kexec-boot.sh --kernel /boot/vmlinuz --initramfs /boot/initramfs.img \
  --cmdline "debug loglevel=7"

# ARM64 with device tree
kexec-boot.sh --kernel /boot/vmlinuz --initramfs /boot/initramfs.img \
  --dtb /boot/bcm2711-rpi-4-b.dtb
```

#### Implementation Details

**File Validation** (`kexec-boot.sh:96-113`):

```bash
validate_file() {
    local file="$1"
    local desc="$2"

    if [ ! -f "$file" ]; then
        error "$desc not found: $file"
        return 1
    fi

    if [ ! -r "$file" ]; then
        error "$desc not readable: $file"
        return 1
    fi

    log "$desc validated: $file"
    return 0
}
```

**Cmdline Construction** (`kexec-boot.sh:115-179`):

Preserves Phase-specific and essential parameters from current cmdline:

```bash
get_current_cmdline_params() {
    local cmdline=$(cat /proc/cmdline)
    local params=""

    for param in $cmdline; do
        case "$param" in
            # Preserve Phase parameters
            phase.mode=*|phase.channel=*|phase.cache=*|phase.nowrite=*)
                params="$params $param"
                ;;
            # Preserve essential parameters
            console=*|root=*)
                params="$params $param"
                ;;
        esac
    done

    echo "$params"
}
```

**Kernel Loading** (`kexec-boot.sh:181-219`):

```bash
load_kernel() {
    local kernel="$1"
    local initramfs="$2"
    local cmdline="$3"
    local dtb="$4"

    log "Loading kernel via kexec..."

    # Build kexec command
    if [ -n "$dtb" ]; then
        kexec -l "$kernel" --initrd="$initramfs" \
              --command-line="$cmdline" --dtb="$dtb"
    else
        kexec -l "$kernel" --initrd="$initramfs" \
              --command-line="$cmdline"
    fi

    log "Kernel loaded successfully"
}
```

**Execution** (`kexec-boot.sh:221-239`):

```bash
execute_kexec() {
    log "Executing kexec to boot new kernel..."
    log "This will replace the current kernel - no return!"

    sleep 1  # Give user moment to see message

    if ! kexec -e; then
        error "Failed to execute kexec"
        error "System may be in inconsistent state"
        return 1
    fi

    # Should never reach here
    error "kexec -e returned unexpectedly"
    return 1
}
```

**Logging** (`kexec-boot.sh:10-26`):

All operations logged to `/tmp/phase-boot.log`:
```bash
LOG_FILE="/tmp/phase-boot.log"

log() {
    echo "[KEXEC] $1"
    echo "$(date '+%H:%M:%S') [KEXEC] $1" >> "$LOG_FILE"
}
```

**Exit Codes**:
- Script does not normally exit (kexec replaces kernel)
- `1`: Validation failure, kexec load failure, or kexec execution failure

---

### mode-handler.sh

**Purpose**: Orchestrates boot based on `phase.mode` parameter
**Language**: POSIX shell
**Source**: `/home/user/phase/boot/initramfs/scripts/mode-handler.sh`
**Location**: `/scripts/mode-handler.sh` (in initramfs)

#### Responsibilities

Routes boot flow to appropriate handler based on `phase.mode`:
- **Internet Mode**: DHT discovery → download → kexec
- **Local Mode**: Cache lookup → (optional LAN discovery) → kexec
- **Private Mode**: Ephemeral DHT discovery → tmpfs-only → kexec

#### Mode Handlers

**Internet Mode** (`mode-handler.sh:91-163`):

```bash
handle_internet_mode() {
    # 1. Verify network available
    [ "$NETWORK_UP" = "true" ] || return 1

    # 2. Run DHT discovery
    phase-discover --arch $(uname -m) --channel $PHASE_CHANNEL \
      --timeout 60 --quiet > /tmp/manifest_url

    # 3. Fetch manifest (M3 stub)
    MANIFEST_URL=$(cat /tmp/manifest_url)

    # 4. Verify manifest (M3 stub)
    # phase-verify --manifest /tmp/manifest.json

    # 5. Download artifacts (M3 stub)
    # phase-fetch --manifest /tmp/manifest.json --output /boot

    # 6. kexec into new kernel
    exec /scripts/kexec-boot.sh \
      --kernel /tmp/phase-kernel \
      --initramfs /tmp/phase-initramfs
}
```

**Local Mode** (`mode-handler.sh:167-245`):

```bash
handle_local_mode() {
    # 1. Check cache directory
    local cache_dir="/cache/phase"
    [ -d "$cache_dir" ] || return 1

    # 2. Find cached images matching channel
    cached_kernel=$(find "$cache_dir" -name "*-$PHASE_CHANNEL-vmlinuz" | head -n 1)
    cached_initramfs=$(find "$cache_dir" -name "*-$PHASE_CHANNEL-initramfs" | head -n 1)

    # 3. Setup OverlayFS for writable root
    /scripts/overlayfs-setup.sh \
      --lower /tmp/overlay-lower \
      --upper /tmp/overlay-upper \
      --work /tmp/overlay-work \
      --merged /tmp/newroot

    # 4. kexec with cached image
    exec /scripts/kexec-boot.sh \
      --kernel "$cached_kernel" \
      --initramfs "$cached_initramfs"
}
```

**Private Mode** (`mode-handler.sh:249-344`):

```bash
handle_private_mode() {
    # 1. Force no-write mode
    PHASE_NOWRITE="true"

    # 2. Verify network available
    [ "$NETWORK_UP" = "true" ] || return 1

    # 3. Run DHT discovery with ephemeral identity
    phase-discover --arch $(uname -m) --channel $PHASE_CHANNEL \
      --ephemeral --timeout 60 --quiet > /tmp/manifest_url

    # 4. Download to tmpfs only (no disk caching)
    # phase-fetch --manifest /tmp/manifest.json --output /tmp

    # 5. Setup tmpfs-only OverlayFS
    mount -t tmpfs -o size=1G tmpfs /tmp/ephemeral
    /scripts/overlayfs-setup.sh \
      --lower /tmp/overlay-lower \
      --upper /tmp/ephemeral/upper \
      --work /tmp/ephemeral/work \
      --merged /tmp/newroot

    # 6. kexec with ephemeral image
    exec /scripts/kexec-boot.sh \
      --kernel /tmp/phase-kernel \
      --initramfs /tmp/phase-initramfs \
      --cmdline "phase.nowrite=true"
}
```

#### Network Status Check

```bash
check_network() {
    if [ -f /tmp/network.status ] && [ "$(cat /tmp/network.status)" = "up" ]; then
        NETWORK_UP="true"
        NETWORK_INTERFACE=$(cat /tmp/network.interface)
        NETWORK_IP=$(cat /tmp/network.ip)
        return 0
    else
        NETWORK_UP="false"
        return 1
    fi
}
```

#### Main Orchestration

```bash
main() {
    # Parse phase parameters from cmdline
    parse_phase_params

    # Check network status
    check_network || true

    # Route to mode handler
    case "$PHASE_MODE" in
        internet)
            handle_internet_mode || exit 1
            ;;
        local)
            handle_local_mode || exit 1
            ;;
        private)
            handle_private_mode || exit 1
            ;;
    esac
}
```

---

### overlayfs-setup.sh

**Purpose**: Creates writable OverlayFS overlay for boot filesystem
**Language**: POSIX shell
**Source**: `/home/user/phase/boot/initramfs/scripts/overlayfs-setup.sh`
**Location**: `/scripts/overlayfs-setup.sh` (in initramfs)

#### Usage

```bash
overlayfs-setup.sh --lower <PATH> --upper <PATH> --work <PATH> --merged <PATH>

Required:
  --lower PATH    Lower (read-only) directory
  --upper PATH    Upper (read-write) directory - will be created
  --work PATH     Work directory - will be created (same FS as upper)
  --merged PATH   Merged mount point - will be created
```

#### Examples

```bash
# Basic overlay
overlayfs-setup.sh \
  --lower /cache/rootfs \
  --upper /tmp/overlay-upper \
  --work /tmp/overlay-work \
  --merged /newroot

# Ephemeral overlay (tmpfs upper)
mount -t tmpfs tmpfs /tmp/ephemeral
overlayfs-setup.sh \
  --lower /cache/rootfs \
  --upper /tmp/ephemeral/upper \
  --work /tmp/ephemeral/work \
  --merged /newroot
```

#### Implementation Details

**OverlayFS Support Check** (`overlayfs-setup.sh:163-186`):

```bash
check_overlay_support() {
    # Check if overlay is available in kernel
    if ! grep -q overlay /proc/filesystems; then
        # Try loading module
        modprobe overlay 2>> "$LOG_FILE"

        # Check again
        if ! grep -q overlay /proc/filesystems; then
            error "OverlayFS not supported by kernel"
            return 1
        fi
    fi

    log "OverlayFS support confirmed"
    return 0
}
```

**Directory Creation** (`overlayfs-setup.sh:137-161`):

```bash
ensure_directory() {
    local dir="$1"
    local desc="$2"

    if [ ! -d "$dir" ]; then
        mkdir -p "$dir" || return 1
    fi

    # Verify writable (except merged, which is mount point)
    if [ "$desc" != "merged directory" ]; then
        [ -w "$dir" ] || return 1
    fi

    return 0
}
```

**Filesystem Verification** (`overlayfs-setup.sh:188-219`):

Ensures upper and work directories are on same filesystem:

```bash
verify_same_filesystem() {
    local upper="$1"
    local work="$2"

    # Get device numbers via stat
    upper_dev=$(stat -c '%d' "$upper")
    work_dev=$(stat -c '%d' "$work")

    if [ "$upper_dev" != "$work_dev" ]; then
        error "Upper and work must be on same filesystem"
        return 1
    fi

    return 0
}
```

**Mount Operation** (`overlayfs-setup.sh:221-246`):

```bash
mount_overlay() {
    local lower="$1"
    local upper="$2"
    local work="$3"
    local merged="$4"

    # Build mount options
    local mount_opts="lowerdir=$lower,upperdir=$upper,workdir=$work"

    # Execute mount
    mount -t overlay overlay -o "$mount_opts" "$merged"

    return $?
}
```

**Mount Verification** (`overlayfs-setup.sh:248-284`):

```bash
verify_mount() {
    local merged="$1"

    # Check if mount point
    mountpoint -q "$merged" || return 1

    # Check readable
    ls "$merged" >/dev/null 2>&1 || return 1

    # Test writable (unless phase.nowrite=true)
    if ! grep -q 'phase\.nowrite=true' /proc/cmdline; then
        touch "$merged/.overlay_test_$$" || return 1
        rm -f "$merged/.overlay_test_$$"
    fi

    return 0
}
```

**Exit Codes**:
- `0`: OverlayFS mounted and verified successfully
- `1`: Kernel support missing, validation failed, or mount failed

---

### plasm-init.sh

**Purpose**: Initialize and start Plasm distributed compute daemon
**Language**: POSIX shell
**Source**: `/home/user/phase/boot/initramfs/scripts/plasm-init.sh`
**Location**: `/scripts/plasm-init.sh` (in initramfs)

#### Usage

```bash
plasm-init.sh
# No arguments - reads config from /etc/plasm/config.json if present
```

#### Responsibilities

1. Find `plasmd` binary (`/bin`, `/usr/bin`, `/usr/local/bin`)
2. Create config directory (`/etc/plasm`)
3. Create data directory (`/var/lib/plasm`)
4. Start daemon in background
5. Wait for daemon ready (health check or process verification)
6. Write status to `/tmp/plasm.status` and PID to `/tmp/plasm.pid`

#### Implementation Details

**Binary Location** (`plasm-init.sh:28-42`):

```bash
find_plasmd() {
    if [ -x "/bin/plasmd" ]; then
        echo "/bin/plasmd"
    elif [ -x "/usr/bin/plasmd" ]; then
        echo "/usr/bin/plasmd"
    elif [ -x "/usr/local/bin/plasmd" ]; then
        echo "/usr/local/bin/plasmd"
    else
        return 1  # Not found
    fi
}
```

**Daemon Startup** (`plasm-init.sh:119-130`):

```bash
# Start with config if available
if [ -f "/etc/plasm/config.json" ]; then
    "$plasmd_bin" start --config /etc/plasm/config.json >> "$LOG_FILE" 2>&1 &
else
    "$plasmd_bin" start >> "$LOG_FILE" 2>&1 &
fi

daemon_pid=$!
log "Daemon started with PID: $daemon_pid"
```

**Readiness Check** (`plasm-init.sh:49-80`):

```bash
wait_for_daemon() {
    local wait_time=0
    local max_wait=30

    while [ $wait_time -lt $max_wait ]; do
        # Check if process running
        if pgrep -x plasmd > /dev/null 2>&1; then
            # If curl available, check health endpoint
            if command -v curl > /dev/null 2>&1; then
                if curl -s -f http://localhost:4001/health > /dev/null 2>&1; then
                    log "Daemon health check passed"
                    return 0
                fi
            else
                # No curl, just verify process
                sleep 1
                if pgrep -x plasmd > /dev/null 2>&1; then
                    return 0
                fi
            fi
        fi

        sleep 1
        wait_time=$((wait_time + 1))
    done

    warn "Daemon readiness check timed out"
    return 1
}
```

**Status Files**:
- `/tmp/plasm.status`: Contains "ready" if daemon initialized
- `/tmp/plasm.pid`: Contains daemon PID
- `/tmp/plasm-init.log`: Initialization log

**Exit Codes**:
- `0`: Daemon initialized and ready
- `1`: Binary not found, startup failed, or readiness timeout

---

## Configuration Files

### manifest.schema.json

**Purpose**: JSON Schema for Phase Boot manifest format
**Language**: JSON Schema (Draft 2020-12)
**Source**: `/home/user/phase/boot/schemas/manifest.schema.json`
**Schema ID**: `https://phase.dev/schemas/boot/manifest.json`

#### Schema Overview

```json
{
  "version": "0.1",
  "manifest_version": 1234,
  "channel": "stable",
  "arch": "x86_64",
  "created_at": "2025-11-26T00:00:00Z",
  "expires_at": "2025-12-26T00:00:00Z",
  "artifacts": {
    "kernel": {
      "hash": "sha256:abcd1234...",
      "size": 52428800,
      "urls": [
        "https://cdn.phase.dev/stable/kernel-x86_64-1234.img",
        "https://mirror.phase.dev/stable/kernel-x86_64-1234.img"
      ],
      "ipfs": "QmXYZ..."
    },
    "initramfs": { /* ... */ },
    "rootfs": { /* ... */ }
  },
  "cmdline": "console=tty0 quiet",
  "signatures": [
    {
      "keyid": "targets-key-2025",
      "sig": "base64-encoded-ed25519-signature"
    }
  ],
  "signed": {
    "data": "base64-encoded-canonical-json"
  }
}
```

#### Required Fields

| Field              | Type    | Description |
|--------------------|---------|-------------|
| `version`          | string  | Schema version (e.g., "0.1") |
| `manifest_version` | integer | Monotonic counter for rollback protection (≥1) |
| `channel`          | enum    | Release channel: stable, testing, nightly |
| `arch`             | enum    | Architecture: x86_64, arm64, aarch64 |
| `artifacts`        | object  | Boot artifacts (kernel, initramfs required) |
| `signatures`       | array   | Ed25519 signatures (≥1 signature required) |
| `signed`           | object  | Canonical signed data (base64-encoded JSON) |

#### Optional Fields

| Field        | Type   | Description |
|--------------|--------|-------------|
| `created_at` | string | ISO 8601 timestamp |
| `expires_at` | string | ISO 8601 expiration |
| `cmdline`    | string | Additional kernel cmdline params |

#### Artifact Schema

Each artifact (kernel, initramfs, rootfs) has:

```json
{
  "hash": "sha256:[64 hex chars]",
  "size": 12345678,
  "urls": [
    "https://primary.example.com/artifact",
    "https://fallback.example.com/artifact"
  ],
  "ipfs": "Qm... or bafy..."  // Optional IPFS CID
}
```

**Hash Format**: `sha256:` prefix + 64 hexadecimal characters

**URL Requirements**:
- At least one URL required
- Tried in order (fallback on failure)
- Must be valid URIs

**IPFS CID** (optional):
- Pattern: `^(Qm|bafy)[a-zA-Z0-9]+`
- Used as last-resort fallback

#### Signature Schema

```json
{
  "keyid": "targets-key-2025",
  "sig": "base64-encoded-64-byte-ed25519-signature"
}
```

**Signature Verification**:
1. Decode `signed.data` (base64 → canonical JSON)
2. Hash with SHA256
3. Verify Ed25519 signature over hash

#### DTBs (Device Trees)

ARM64 systems may include device tree blobs:

```json
{
  "artifacts": {
    "kernel": { /* ... */ },
    "initramfs": { /* ... */ },
    "dtbs": {
      "bcm2711-rpi-4-b": {
        "hash": "sha256:...",
        "size": 12345,
        "urls": ["https://..."]
      },
      "rockchip-rk3588": { /* ... */ }
    }
  }
}
```

**Schema Reference**: `/home/user/phase/boot/schemas/manifest.schema.json:54-60`

---

## Directory Reference

### Runtime Directories

**`/tmp/`** (tmpfs):
- `manifest_url` - Discovered manifest URL
- `manifest.json` - Downloaded manifest (M3+)
- `phase-boot.log` - Main boot log
- `plasm-init.log` - Plasm daemon log
- `network.status` - "up" or "down"
- `network.interface` - Active interface name (e.g., "eth0")
- `network.ip` - Assigned IP address
- `plasm.status` - "ready" if daemon initialized
- `plasm.pid` - Plasm daemon PID

**`/boot/`** (artifact output):
- `kernel` - Downloaded kernel
- `kernel.sha256` - Kernel hash
- `initramfs` - Downloaded initramfs
- `initramfs.sha256` - Initramfs hash
- `rootfs` - Downloaded rootfs (optional)
- `rootfs.sha256` - Rootfs hash

**`/cache/phase/`** (persistent cache, Local Mode):
- `{version}-{channel}-vmlinuz` - Cached kernel
- `{version}-{channel}-initramfs` - Cached initramfs
- `{version}-{channel}-rootfs` - Cached rootfs
- `version` - Cached manifest version (rollback protection)

**`/etc/plasm/`**:
- `config.json` - Plasm daemon configuration (optional)

**`/var/lib/plasm/`**:
- Plasm daemon data directory

---

## See Also

- [ARCHITECTURE.md](./ARCHITECTURE.md) - System architecture overview
- [SECURITY.md](./SECURITY.md) - Security architecture
- [manifest.schema.json](../schemas/manifest.schema.json) - Manifest schema specification

---

**Last Updated**: 2025-11-26
**Maintained By**: Phase Boot Team

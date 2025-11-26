# Phase Boot Troubleshooting Guide

## Overview

This guide helps diagnose and resolve common issues with Phase Boot. Each section covers specific problem categories with symptoms, causes, and solutions.

**Quick Start**:
- Boot issues → [Boot Issues](#boot-issues)
- Network problems → [Network Issues](#network-issues)
- Signature/verification failures → [Verification Issues](#verification-issues)
- kexec failures → [Kexec Issues](#kexec-issues)
- Plasm daemon issues → [Plasm Issues](#plasm-issues)
- Diagnostic commands → [Diagnostic Tools](#diagnostic-tools)

---

## Boot Issues

### System Doesn't Boot at All

**Symptoms**:
- USB appears to be ignored
- System boots to existing OS
- No boot menu appears

**Likely Causes**:
1. USB not set as first boot device
2. UEFI boot mode disabled (Legacy/CSM mode active)
3. Secure Boot enabled (not supported in M1)
4. Corrupted image or USB write failure

**Solutions**:

1. **Configure BIOS/UEFI Settings**:
   - Enter BIOS/UEFI setup (usually F2, F12, Del, or Esc at boot)
   - **Disable Secure Boot** (typically in Security or Boot menu)
   - **Enable UEFI boot mode** (disable Legacy/CSM mode)
   - **Set USB as first boot device** (in Boot Priority menu)
   - Save settings and reboot

2. **Verify USB Write**:
   ```bash
   # Re-write the image carefully
   sudo ./scripts/write-usb.sh --image build/phase-boot-x86_64.img --device /dev/sdX

   # Verify partition table
   sudo fdisk -l /dev/sdX
   # Should show GPT partition table with ESP partition
   ```

3. **Check Image Integrity**:
   ```bash
   # Verify image file is not corrupted
   ls -lh build/phase-boot-x86_64.img
   # Should be ~800MB-1GB

   # Check if image is bootable
   file build/phase-boot-x86_64.img
   ```

**Diagnostic Info**:
- BIOS/UEFI version and vendor
- Boot mode (UEFI vs Legacy)
- Secure Boot status
- USB device type and capacity

---

### Boot Hangs at Init

**Symptoms**:
- Kernel loads successfully
- Boot hangs with message: `Phase Boot initializing...`
- No further progress
- No shell prompt appears

**Likely Causes**:
1. Failed to mount essential filesystems
2. Init script crashed or exited unexpectedly
3. Missing required binaries in initramfs
4. Kernel panic after init starts

**Solutions**:

1. **Enable Kernel Debug Output**:
   - At boot menu, select entry and press 'e' to edit
   - Add to kernel command line: `debug loglevel=7`
   - Press Ctrl+X or F10 to boot
   - Watch for error messages

2. **Check Init Script**:
   ```bash
   # If you get a shell prompt (even emergency shell):

   # Verify init is running
   ps aux | grep init

   # Check what's mounted
   mount

   # Should see /proc, /sys, /dev, /run mounted
   ```

3. **Mount Essential Filesystems Manually** (emergency shell):
   ```bash
   mount -t proc proc /proc
   mount -t sysfs sysfs /sys
   mount -t devtmpfs devtmpfs /dev
   mount -t tmpfs tmpfs /run
   ```

4. **Verify Init Script Executable**:
   ```bash
   # In emergency shell
   ls -la /init
   # Should show: -rwxr-xr-x (executable)

   # Try running init manually to see errors
   /init
   ```

**Diagnostic Info**:
- Last message displayed before hang
- Kernel version (`uname -r` if you get shell)
- Contents of `/proc/cmdline`
- Output of `dmesg` if available

---

### Kernel Panic Messages

**Symptoms**:
- System displays: `Kernel panic - not syncing: ...`
- Common panic messages:
  - `VFS: Unable to mount root fs`
  - `No working init found`
  - `Attempted to kill init`

**Likely Causes**:
1. Initramfs not loaded or corrupted
2. Init script missing or not executable
3. Required kernel modules missing
4. Out of memory

**Solutions**:

1. **"No working init found"**:
   - Init script missing from initramfs
   - Rebuild image: `make clean && make`
   - Verify init exists: `cpio -t < boot/initramfs.cpio | grep "^init$"`

2. **"VFS: Unable to mount root fs"**:
   - For Phase Boot, this usually means initramfs issue
   - Kernel can't find initramfs or it's malformed
   - Rebuild initramfs: `make initramfs`
   - Check CPIO format: `file boot/initramfs.cpio`

3. **"Attempted to kill init"**:
   - Init script crashed
   - Check init script syntax: `sh -n boot/initramfs/init`
   - Review recent changes to `/boot/initramfs/init`

4. **Memory Issues**:
   - Increase VM RAM (if using QEMU/VirtualBox)
   - Minimum 512MB required, 2GB recommended
   - Check available memory: add `mem=2G` to kernel cmdline

**Diagnostic Info**:
- Full panic message
- Kernel command line (shown in panic output)
- Hardware platform (VM or physical)
- RAM available

---

### "Failed to Mount" Errors

**Symptoms**:
- Error messages like:
  - `[INIT] ERROR: Failed to mount /proc`
  - `[INIT] ERROR: Failed to mount /sys`
  - `mount: mounting ... failed`

**Likely Causes**:
1. Kernel missing filesystem support (procfs, sysfs, devtmpfs)
2. `/proc`, `/sys`, `/dev` directories missing from initramfs
3. Already mounted (not actually an error)

**Solutions**:

1. **Verify Kernel Configuration**:
   ```bash
   # Check kernel has required features
   zgrep CONFIG_PROC_FS /proc/config.gz
   zgrep CONFIG_SYSFS /proc/config.gz
   zgrep CONFIG_DEVTMPFS /proc/config.gz

   # All should show: =y
   ```

2. **Check Mount Points Exist**:
   ```bash
   # From emergency shell
   ls -la / | grep -E 'proc|sys|dev|run'

   # If missing, create them
   mkdir -p /proc /sys /dev /run
   ```

3. **Ignore Benign "Already Mounted" Warnings**:
   - Some UEFI systems pre-mount `/sys/firmware/efi`
   - These warnings are usually harmless
   - Init script continues despite warnings

4. **Review Init Script**:
   - Check `/boot/initramfs/init` line 27-42 (mount_essential function)
   - Ensure it's not using incompatible mount options

**Diagnostic Info**:
- Exact error message
- Which filesystem failed to mount
- Output of `mount` command (if shell accessible)
- Kernel config (`/proc/config.gz` or `.config`)

---

## Network Issues

### No Network Interface Detected

**Symptoms**:
- Message: `[NET-WIRED] ERROR: No wired network interfaces found`
- `ip link show` shows only `lo` (loopback)
- Network status shows: `DOWN`

**Likely Causes**:
1. Kernel missing network drivers
2. Network interface not recognized
3. USB/PCIe network device not initialized
4. Virtual machine network adapter not configured

**Solutions**:

1. **Check for Network Interfaces**:
   ```bash
   # List all interfaces
   ip link show

   # Check kernel sees the hardware
   dmesg | grep -i eth
   dmesg | grep -i net

   # Look in /sys
   ls /sys/class/net/
   ```

2. **Verify Kernel Drivers** (if you can access host):
   ```bash
   # Check kernel config has network support
   zgrep CONFIG_NET= /proc/config.gz
   zgrep CONFIG_ETHERNET= /proc/config.gz
   zgrep CONFIG_NETDEVICES= /proc/config.gz

   # Should all show: =y
   ```

3. **Virtual Machine Configuration**:
   - **QEMU**: Ensure `-netdev` and `-device` specified
     ```bash
     qemu-system-x86_64 \
       -netdev user,id=net0 \
       -device virtio-net-pci,netdev=net0 \
       ...
     ```
   - **VirtualBox**: Verify network adapter enabled and type set to "Virtio-Net" or "Intel PRO/1000"
   - **VMware**: Ensure network adapter added to VM

4. **Physical Hardware**:
   - Try different USB ports
   - For USB Ethernet adapters, wait 10-15 seconds after boot
   - Check if network card has activity lights
   - Try known-working network cable

5. **Module Loading** (if modules available):
   ```bash
   # Check if network driver modules exist
   find /lib/modules -name "*net*" -o -name "*ethernet*"

   # Load manually if found
   modprobe virtio_net  # For QEMU virtio
   modprobe e1000       # For Intel NICs
   ```

**Diagnostic Info**:
- Output of `ip link show`
- Output of `dmesg | grep -i net`
- Output of `lspci` or `lsusb` (if available)
- VM platform and network adapter type
- Physical hardware model

---

### DHCP Failures

**Symptoms**:
- Message: `[NET-WIRED] ERROR: DHCP failed on eth0`
- Interface shows UP but no IP address
- `ip addr show` shows no `inet` address
- Retries all fail: `Failed on interface eth0 after 3 attempts`

**Likely Causes**:
1. No DHCP server on network
2. DHCP client (udhcpc/dhcpcd) missing or broken
3. Network cable unplugged
4. VLAN or authentication required
5. DHCP server too slow to respond

**Solutions**:

1. **Check Interface Status**:
   ```bash
   # Verify interface is UP
   ip link show eth0
   # Should show: state UP

   # Check for carrier (cable connected)
   cat /sys/class/net/eth0/carrier
   # Should show: 1

   # If carrier is 0, cable is unplugged or bad
   ```

2. **Manual DHCP Attempt**:
   ```bash
   # Try DHCP manually with verbose output
   udhcpc -i eth0 -n -q -v

   # Or with dhcpcd
   dhcpcd -d eth0

   # Watch for errors in output
   ```

3. **Check DHCP Server**:
   - Verify DHCP server running on network (router/server)
   - Check if other devices can get DHCP leases
   - For QEMU user networking, DHCP should work automatically
   - For bridged networking, ensure bridge configured correctly

4. **Increase Timeout**:
   ```bash
   # Edit timeout in /scripts/net-wired.sh (if accessible)
   # Change DHCP_TIMEOUT from 10 to 30 seconds

   # Or try manual DHCP with longer timeout
   udhcpc -i eth0 -t 30 -T 3 -n -q
   ```

5. **Static IP Fallback** (emergency):
   ```bash
   # If DHCP unavailable, configure static IP
   ip addr add 192.168.1.100/24 dev eth0
   ip route add default via 192.168.1.1
   echo "nameserver 8.8.8.8" > /etc/resolv.conf

   # Test connectivity
   ping -c 3 8.8.8.8
   ```

**Diagnostic Info**:
- Output of `ip addr show`
- Output of `ip link show`
- DHCP client logs: `tail -50 /tmp/network.log`
- Network configuration (DHCP server IP, network range)
- `cat /sys/class/net/eth0/carrier` (link status)

---

### DNS Resolution Failures

**Symptoms**:
- Ping by IP works: `ping 8.8.8.8` succeeds
- Ping by name fails: `ping google.com` fails
- Message: `FAILED (DNS issue)` in diagnostics
- Error: `ping: bad address 'google.com'`

**Likely Causes**:
1. No DNS servers configured
2. `/etc/resolv.conf` missing or empty
3. DNS servers unreachable
4. Firewall blocking DNS (port 53)

**Solutions**:

1. **Check DNS Configuration**:
   ```bash
   # View DNS configuration
   cat /etc/resolv.conf

   # Should contain one or more:
   # nameserver 8.8.8.8
   # nameserver 1.1.1.1
   ```

2. **Configure DNS Manually**:
   ```bash
   # Add Google DNS
   echo "nameserver 8.8.8.8" > /etc/resolv.conf
   echo "nameserver 1.1.1.1" >> /etc/resolv.conf

   # Test resolution
   ping -c 3 google.com
   ```

3. **Test DNS Directly** (if nslookup/dig available):
   ```bash
   # Test Google DNS
   nslookup google.com 8.8.8.8

   # Test Cloudflare DNS
   nslookup google.com 1.1.1.1
   ```

4. **Verify DNS Ports Not Blocked**:
   ```bash
   # If nc (netcat) available
   nc -zvu 8.8.8.8 53

   # Should show: succeeded or open
   ```

5. **Re-run DHCP** (should set DNS automatically):
   ```bash
   # Release current lease
   ip addr flush dev eth0

   # Request new lease (will update /etc/resolv.conf)
   udhcpc -i eth0 -n -q

   # Check resolv.conf was updated
   cat /etc/resolv.conf
   ```

**Diagnostic Info**:
- Contents of `/etc/resolv.conf`
- Output of `ping -c 3 8.8.8.8` (test by IP)
- Output of `ping -c 3 google.com` (test by name)
- Network log: `tail -20 /tmp/network.log`

---

### Cannot Reach Bootstrap Nodes

**Symptoms**:
- Message: `[MODE] ERROR: DHT discovery failed or timed out`
- Network is up with IP and DNS working
- Ping to internet works
- Discovery times out after 60 seconds

**Likely Causes**:
1. Firewall blocking DHT traffic (UDP ports)
2. Internet connectivity limited (captive portal)
3. phase-discover binary missing or broken
4. Bootstrap nodes unreachable or offline
5. Restrictive network policy (corporate firewall)

**Solutions**:

1. **Verify Internet Connectivity**:
   ```bash
   # Test various connectivity levels
   ping -c 3 8.8.8.8          # Basic IP
   ping -c 3 google.com       # DNS resolution
   curl -I https://google.com # HTTP/HTTPS

   # All should succeed for DHT to work
   ```

2. **Check phase-discover Binary**:
   ```bash
   # Verify binary exists and is executable
   which phase-discover
   ls -la /bin/phase-discover

   # Test with verbose output
   phase-discover --arch $(uname -m) --channel stable --timeout 60

   # Watch for error messages
   ```

3. **Test Discovery Manually**:
   ```bash
   # Run with debug output
   phase-discover --arch $(uname -m) --channel stable --timeout 60 --verbose

   # Should show:
   # - Connecting to bootstrap nodes
   # - DHT queries
   # - Manifest URL if successful
   ```

4. **Check Firewall/Network Restrictions**:
   - DHT requires outbound UDP (various high ports)
   - Try from different network (home vs corporate)
   - Check if captive portal present (try browsing)
   - Some networks block P2P protocols

5. **Use Local Mode** (if cache available):
   ```bash
   # Reboot and select "Local Mode" from boot menu
   # Or add to kernel cmdline:
   # phase.mode=local
   ```

6. **Check Bootstrap Node Status** (from another system):
   ```bash
   # Verify bootstrap nodes reachable
   # (actual addresses from Phase project documentation)
   ping bootstrap1.phase.network
   ping bootstrap2.phase.network
   ```

**Diagnostic Info**:
- Output of `/scripts/net-diag.sh`
- Discovery log: `tail -50 /tmp/phase-boot.log | grep DISCOVER`
- Network environment (home/corporate/public)
- Output of `phase-discover --verbose`
- Firewall logs if available

---

### DHT Discovery Timeouts

**Symptoms**:
- Message: `Discovery failed or timed out`
- Takes full 60 seconds before failing
- Network connectivity is working
- Intermittent success/failure

**Likely Causes**:
1. Slow network connection
2. Few DHT peers available
3. phase-discover timeout too short
4. Network latency/packet loss
5. Bootstrap nodes overloaded

**Solutions**:

1. **Increase Discovery Timeout**:
   ```bash
   # Try manual discovery with longer timeout
   phase-discover --arch $(uname -m) --channel stable --timeout 120

   # If this works, timeout is too short
   ```

2. **Retry Multiple Times**:
   ```bash
   # DHT can be eventually consistent
   for i in 1 2 3; do
     echo "Attempt $i..."
     phase-discover --arch $(uname -m) --channel stable --timeout 60 && break
     sleep 5
   done
   ```

3. **Check Network Quality**:
   ```bash
   # Test latency
   ping -c 10 8.8.8.8
   # Look for packet loss % and avg latency

   # Should be <100ms with 0% loss
   ```

4. **Switch to Different Channel**:
   ```bash
   # Try different release channel
   phase-discover --arch $(uname -m) --channel beta --timeout 60

   # More peers might be on stable/beta channels
   ```

5. **Use Cached Image** (if available):
   - Reboot and select "Local Mode"
   - Uses cached image, no discovery needed
   - See [Local Mode section](#handle-local-mode) in mode-handler.sh

**Diagnostic Info**:
- Network latency: `ping -c 10 8.8.8.8`
- Discovery attempt time (should be ~60s)
- Time of day (network congestion)
- Channel being used (stable/beta/dev)
- Contents of `/tmp/phase-boot.log`

---

## Verification Issues

**Note**: Signature verification is an M3+ feature. These issues apply to future releases.

### "Signature Verification Failed"

**Symptoms**:
- Message: `ERROR: Signature verification failed for manifest`
- Message: `ERROR: Invalid signature for kernel`
- Downloaded files present but won't boot

**Likely Causes**:
1. Manifest tampered with in transit
2. Signing key mismatch
3. Corrupted download
4. Man-in-the-middle attack
5. Clock skew (certificate expiration)

**Solutions**:

1. **Retry Download**:
   ```bash
   # Corruption during download is possible
   # Phase Boot will retry automatically
   # If manual retry needed:
   rm /tmp/phase-manifest
   # Re-run discovery
   ```

2. **Check System Clock**:
   ```bash
   # Verify system time is correct
   date

   # If incorrect, can cause certificate validation failures
   # Set manually if needed (format: MMDDhhmmYYYY)
   date 112612002025  # Nov 26 12:00 2025
   ```

3. **Verify Signing Keys**:
   ```bash
   # Check which signing keys are trusted
   ls -la /etc/phase/trusted-keys/

   # Should contain official Phase project keys
   ```

4. **Check Logs for Details**:
   ```bash
   # Verification errors should show specific failure
   grep -i "signature\|verify" /tmp/phase-boot.log

   # Look for:
   # - Which file failed
   # - Which key was used
   # - Specific crypto error
   ```

5. **Report Security Issue**:
   - If verification consistently fails, may indicate:
     - Compromised repository
     - Network attack
     - Bug in verification code
   - Report to Phase project security team
   - Do NOT bypass signature verification

**Diagnostic Info**:
- Full error message
- Contents of manifest: `cat /tmp/phase-manifest`
- System time: `date`
- Verification log from `/tmp/phase-boot.log`
- Network path (ISP, VPN, proxy)

---

### "Rollback Detected" Errors

**Symptoms**:
- Message: `ERROR: Rollback detected - manifest version older than cached`
- Message: `ERROR: Version downgrade prevented`
- Newer version already cached locally

**Likely Causes**:
1. Attempting to boot older version than cached
2. phase.channel changed (e.g., stable → beta → stable)
3. Manifest version numbering issue
4. Malicious rollback attempt

**Solutions**:

1. **Verify Version Information**:
   ```bash
   # Check cached version
   cat /cache/phase/VERSION

   # Check manifest version
   grep version /tmp/phase-manifest

   # Cached should NOT be newer than manifest
   ```

2. **Clear Cache** (if intentional downgrade):
   ```bash
   # WARNING: Only if you intend to use older version
   rm -rf /cache/phase/*

   # Reboot and re-download
   ```

3. **Check Channel Consistency**:
   ```bash
   # Verify kernel cmdline channel
   cat /proc/cmdline | grep phase.channel

   # Should match intended channel (stable/beta/dev)
   ```

4. **Allow Rollback** (emergency only):
   ```bash
   # Add kernel parameter (boot menu, press 'e')
   phase.allow_rollback=true

   # WARNING: Only use if you know why version is older
   ```

**Diagnostic Info**:
- Cached version: `cat /cache/phase/VERSION`
- Manifest version: `grep version /tmp/phase-manifest`
- Channel setting: `cat /proc/cmdline | grep phase.channel`
- Rollback log from `/tmp/phase-boot.log`

---

### "No Valid Manifest Found"

**Symptoms**:
- Message: `ERROR: No valid manifest found`
- Discovery succeeded but manifest missing or invalid
- Empty or malformed `/tmp/phase-manifest`

**Likely Causes**:
1. Manifest URL returned by discovery is invalid
2. Download failed
3. Manifest file malformed/corrupted
4. Server/CDN issue

**Solutions**:

1. **Check Manifest URL**:
   ```bash
   # See what URL discovery returned
   cat /tmp/manifest_url

   # Should be valid HTTPS URL
   # Example: https://cdn.phase.network/stable/x86_64/manifest.json
   ```

2. **Download Manifest Manually**:
   ```bash
   # Test if URL is accessible
   MANIFEST_URL=$(cat /tmp/manifest_url)

   # If curl available
   curl -v "$MANIFEST_URL"

   # If wget available
   wget -O /tmp/test-manifest "$MANIFEST_URL"

   # Should download JSON file
   ```

3. **Validate Manifest Format**:
   ```bash
   # Check if manifest is valid JSON
   cat /tmp/phase-manifest

   # Should contain:
   # - version
   # - architecture
   # - kernel URL
   # - initramfs URL
   # - signatures
   ```

4. **Check Network/CDN Status**:
   ```bash
   # Test connectivity to CDN
   ping cdn.phase.network

   # Try alternate DNS
   echo "nameserver 1.1.1.1" > /etc/resolv.conf

   # Retry download
   ```

5. **Retry Discovery**:
   ```bash
   # DHT might have returned stale/bad URL
   rm /tmp/manifest_url /tmp/phase-manifest

   # Re-run discovery
   phase-discover --arch $(uname -m) --channel stable --timeout 60
   ```

**Diagnostic Info**:
- Contents of `/tmp/manifest_url`
- Contents of `/tmp/phase-manifest`
- Output of `curl -v $(cat /tmp/manifest_url)`
- Network log: `/tmp/network.log`
- Phase boot log: `/tmp/phase-boot.log`

---

### Hash Mismatch Errors

**Symptoms**:
- Message: `ERROR: Hash mismatch for kernel`
- Message: `ERROR: Downloaded file hash doesn't match manifest`
- Download completes but verification fails

**Likely Causes**:
1. Corrupted download (network issue)
2. Tampered file (security issue)
3. Incorrect hash in manifest
4. Partial download

**Solutions**:

1. **Verify Download Completed**:
   ```bash
   # Check file size matches manifest
   ls -lh /tmp/phase-kernel /tmp/phase-initramfs

   # Compare to manifest expected sizes
   grep size /tmp/phase-manifest
   ```

2. **Retry Download**:
   ```bash
   # Remove downloaded files
   rm /tmp/phase-kernel /tmp/phase-initramfs

   # Phase Boot should re-download automatically
   # Or trigger manually if needed
   ```

3. **Verify Hash Manually**:
   ```bash
   # Calculate SHA256 hash (if sha256sum available)
   sha256sum /tmp/phase-kernel

   # Compare to manifest
   grep kernel /tmp/phase-manifest | grep hash

   # Hashes should match exactly
   ```

4. **Check Network Integrity**:
   ```bash
   # Test download stability
   curl -o /tmp/test https://cdn.phase.network/test/1MB.bin
   sha256sum /tmp/test

   # If hash doesn't match expected, network is corrupting data
   ```

5. **Report Verification Failure**:
   - If hashes consistently don't match:
     - Possible compromised CDN/mirror
     - Possible network tampering
     - Possible bug in hash generation
   - Report to Phase project security team
   - Do NOT bypass hash verification

**Diagnostic Info**:
- Expected hash from manifest
- Actual hash: `sha256sum /tmp/phase-kernel`
- File size: `ls -lh /tmp/phase-kernel`
- Network path (ISP, proxy, VPN)
- Download log from `/tmp/phase-boot.log`

---

## Kexec Issues

### "kexec Failed to Load"

**Symptoms**:
- Message: `[KEXEC] ERROR: Failed to load kernel with kexec`
- Message: `kexec -l` returns error code
- Kernel file and initramfs present

**Likely Causes**:
1. kexec binary missing
2. Kernel image format incompatible
3. Invalid command line parameters
4. Insufficient memory
5. kexec disabled in kernel

**Solutions**:

1. **Verify kexec Binary**:
   ```bash
   # Check kexec is available
   which kexec
   ls -la /sbin/kexec

   # Test kexec version
   kexec --version

   # Should show version info
   ```

2. **Check kexec Kernel Support**:
   ```bash
   # Verify kexec not disabled
   cat /proc/sys/kernel/kexec_load_disabled
   # Should show: 0

   # If shows 1, kexec is disabled
   # Enable it:
   echo 0 > /proc/sys/kernel/kexec_load_disabled
   ```

3. **Validate Kernel Image**:
   ```bash
   # Check kernel file format
   file /tmp/phase-kernel

   # Should show:
   # - "bzImage" or "kernel image" for x86_64
   # - "PE32+ executable" for ARM64 UEFI

   # Check file is not corrupted
   ls -lh /tmp/phase-kernel
   # Should be 5-20MB typically
   ```

4. **Try Manual kexec Load**:
   ```bash
   # Load with verbose output
   kexec -l /tmp/phase-kernel \
     --initrd=/tmp/phase-initramfs \
     --command-line="console=tty0 phase.mode=internet" \
     --verbose

   # Watch for specific error messages
   ```

5. **Check kexec Log**:
   ```bash
   # Review detailed error log
   cat /tmp/phase-boot.log | grep KEXEC

   # Look for specific failure point
   ```

6. **Verify Architecture Match**:
   ```bash
   # Current kernel architecture
   uname -m

   # New kernel architecture (if file command shows it)
   file /tmp/phase-kernel

   # Must match (both x86_64 or both aarch64)
   ```

**Diagnostic Info**:
- Output of `kexec --version`
- Contents of `/proc/sys/kernel/kexec_load_disabled`
- Output of `file /tmp/phase-kernel`
- Output of `file /tmp/phase-initramfs`
- Full kexec log: `cat /tmp/phase-boot.log | grep KEXEC`

---

### Memory Issues During kexec

**Symptoms**:
- Message: `ERROR: Not enough memory for kexec`
- Message: `Cannot allocate memory`
- System has limited RAM
- Large kernel/initramfs files

**Likely Causes**:
1. Insufficient available RAM
2. Memory fragmentation
3. Large initramfs
4. Memory leaks in previous processes

**Solutions**:

1. **Check Available Memory**:
   ```bash
   # View memory usage
   free -m

   # Should have at least 200MB available
   # More if kernel + initramfs is large

   # Detailed view
   cat /proc/meminfo | grep -E 'MemAvailable|MemFree'
   ```

2. **Free Up Memory**:
   ```bash
   # Kill unnecessary processes
   ps aux
   # Kill any non-essential processes

   # Clear buffer cache
   sync
   echo 3 > /proc/sys/vm/drop_caches

   # Check memory again
   free -m
   ```

3. **Check Image Sizes**:
   ```bash
   # See how much memory kernel + initramfs need
   ls -lh /tmp/phase-kernel /tmp/phase-initramfs

   # Total should be less than available RAM
   ```

4. **Increase VM Memory** (if using VM):
   - For QEMU: increase `-m` parameter (e.g., `-m 2G`)
   - For VirtualBox: increase RAM in VM settings
   - For VMware: increase memory in .vmx file
   - Minimum 2GB recommended

5. **Use Smaller Initramfs** (if building custom):
   ```bash
   # Reduce initramfs size by excluding unnecessary files
   # Edit initramfs generation script
   # Remove debug symbols, extra tools
   ```

**Diagnostic Info**:
- Output of `free -m`
- Size of kernel + initramfs: `ls -lh /tmp/phase-*`
- Process list: `ps aux --sort=-rss | head -20`
- Total RAM available: `cat /proc/meminfo | grep MemTotal`

---

### Architecture Mismatch Errors

**Symptoms**:
- Message: `ERROR: Architecture mismatch`
- Message: `Exec format error`
- Downloaded x86_64 kernel on ARM64 system (or vice versa)

**Likely Causes**:
1. Discovery returned wrong architecture
2. Manual download used wrong arch
3. phase-discover arch detection failed
4. Multi-arch system confusion

**Solutions**:

1. **Verify System Architecture**:
   ```bash
   # Check actual CPU architecture
   uname -m

   # Should be:
   # - x86_64 (Intel/AMD)
   # - aarch64 (ARM64)
   ```

2. **Check Downloaded Kernel Architecture**:
   ```bash
   # Inspect kernel file
   file /tmp/phase-kernel

   # Should mention same architecture as `uname -m`
   ```

3. **Verify Discovery Used Correct Arch**:
   ```bash
   # Check what arch was requested
   grep "arch" /tmp/phase-boot.log | grep -i discover

   # Should show same as `uname -m`
   ```

4. **Force Correct Architecture**:
   ```bash
   # Re-run discovery with explicit arch
   phase-discover --arch $(uname -m) --channel stable --timeout 60

   # Ensure $(uname -m) is correct
   ```

5. **Check Manifest Architecture**:
   ```bash
   # Verify manifest specifies correct arch
   grep -i arch /tmp/phase-manifest

   # Should match system architecture
   ```

**Diagnostic Info**:
- System arch: `uname -m`
- Kernel file type: `file /tmp/phase-kernel`
- Manifest arch: `grep arch /tmp/phase-manifest`
- Discovery log: `grep discover /tmp/phase-boot.log`

---

## Plasm Issues

### Daemon Fails to Start

**Symptoms**:
- Message: `[PLASM-INIT] ERROR: plasmd binary not found`
- Message: `[PLASM-INIT] ERROR: Daemon failed to become ready`
- Plasm status shows: `NOT READY`

**Likely Causes**:
1. plasmd binary not included in image
2. Binary not executable
3. Missing dependencies
4. Configuration error
5. Port already in use

**Solutions**:

1. **Verify Binary Exists**:
   ```bash
   # Check for plasmd binary
   which plasmd
   ls -la /bin/plasmd /usr/bin/plasmd /usr/local/bin/plasmd

   # Should find one of these
   ```

2. **Check Binary is Executable**:
   ```bash
   # Verify permissions
   ls -la $(which plasmd)

   # Should show: -rwxr-xr-x

   # If not executable:
   chmod +x $(which plasmd)
   ```

3. **Test Binary Directly**:
   ```bash
   # Try running plasmd directly
   /bin/plasmd --version

   # Should show version info

   # Try starting manually
   /bin/plasmd start --foreground

   # Watch for error messages
   ```

4. **Check Configuration**:
   ```bash
   # Verify config file exists and is valid
   ls -la /etc/plasm/config.json

   # Validate JSON syntax
   cat /etc/plasm/config.json

   # Should be valid JSON
   ```

5. **Check Port Availability**:
   ```bash
   # Plasm default port is 4001
   # Check if already in use
   netstat -ln | grep 4001
   # Or:
   ss -ln | grep 4001

   # If port in use, kill conflicting process
   ```

6. **Review Daemon Logs**:
   ```bash
   # Check initialization log
   cat /tmp/plasm-init.log

   # Look for specific error messages
   tail -50 /tmp/plasm-init.log
   ```

7. **Check Dependencies**:
   ```bash
   # Verify required libraries present
   ldd $(which plasmd)

   # Should show all dependencies found
   # "not found" indicates missing library
   ```

**Diagnostic Info**:
- Binary location: `which plasmd`
- Binary info: `file $(which plasmd)`
- Daemon log: `cat /tmp/plasm-init.log`
- Config: `cat /etc/plasm/config.json`
- Process status: `ps aux | grep plasmd`

---

### WASM Execution Errors

**Symptoms**:
- Message: `ERROR: WASM execution failed`
- Message: `ERROR: Invalid WASM module`
- Plasm daemon running but execution fails

**Likely Causes**:
1. WASM runtime missing or broken
2. Invalid WASM binary
3. Insufficient memory for execution
4. Security constraints blocking execution
5. WASM runtime version incompatible

**Solutions**:

1. **Verify WASM Runtime**:
   ```bash
   # Check plasmd has WASM support
   /bin/plasmd --features

   # Should list WASM runtime (wasmtime/wasmer)
   ```

2. **Check WASM Module**:
   ```bash
   # Validate WASM file format
   file /path/to/module.wasm

   # Should show: "WebAssembly"

   # Check file not corrupted
   ls -la /path/to/module.wasm
   ```

3. **Test with Simple WASM**:
   ```bash
   # Try executing known-good WASM module
   # (if plasm has test command)
   plasmd test --module /test/hello.wasm
   ```

4. **Check Memory Limits**:
   ```bash
   # Verify sufficient memory for WASM execution
   free -m

   # WASM modules may need substantial heap
   # Increase memory if in VM
   ```

5. **Review Plasm Configuration**:
   ```bash
   # Check WASM runtime settings
   cat /etc/plasm/config.json | grep -i wasm

   # Look for:
   # - runtime type
   # - memory limits
   # - security settings
   ```

6. **Check Daemon Logs**:
   ```bash
   # Daemon logs may show WASM-specific errors
   tail -100 /tmp/plasm-init.log | grep -i wasm

   # Look for initialization errors
   ```

**Diagnostic Info**:
- Plasm version: `/bin/plasmd --version`
- WASM module: `file /path/to/module.wasm`
- Daemon log: `tail -100 /tmp/plasm-init.log`
- Available memory: `free -m`
- Config: `cat /etc/plasm/config.json`

---

### Connection to Network Fails

**Symptoms**:
- Plasm daemon starts but can't connect to network
- Message: `ERROR: Failed to connect to Phase network`
- Message: `ERROR: No peers found`
- Daemon isolated

**Likely Causes**:
1. Network connectivity issue
2. Firewall blocking Plasm ports
3. Bootstrap nodes unreachable
4. Phase network offline
5. Plasm network configuration incorrect

**Solutions**:

1. **Verify Basic Network**:
   ```bash
   # Ensure internet connectivity works
   ping -c 3 8.8.8.8
   ping -c 3 google.com

   # Must succeed before Plasm can connect
   ```

2. **Check Plasm Network Config**:
   ```bash
   # Review network settings
   cat /etc/plasm/config.json | grep -i network

   # Look for:
   # - bootstrap nodes
   # - listen address
   # - peer discovery settings
   ```

3. **Test Plasm Connectivity**:
   ```bash
   # If plasmd has connectivity test
   plasmd network-test

   # Or check daemon status
   curl http://localhost:4001/network/peers

   # Should list connected peers (if any)
   ```

4. **Check Firewall Rules**:
   ```bash
   # Plasm needs outbound connections
   # Check if firewall rules block it
   iptables -L -n | grep 4001

   # Or:
   nft list ruleset | grep 4001
   ```

5. **Verify Bootstrap Nodes**:
   ```bash
   # Test if bootstrap nodes are reachable
   # (addresses from config.json)
   ping bootstrap1.plasm.network

   # If ping fails, bootstrap nodes may be down
   ```

6. **Review Daemon Logs**:
   ```bash
   # Look for network connection errors
   tail -100 /tmp/plasm-init.log | grep -i network
   tail -100 /tmp/plasm-init.log | grep -i peer

   # Shows connection attempts and failures
   ```

7. **Restart Daemon with Debug**:
   ```bash
   # Kill existing daemon
   pkill plasmd

   # Start with debug logging
   /bin/plasmd start --log-level debug --foreground

   # Watch output for network issues
   ```

**Diagnostic Info**:
- Network status: `/scripts/net-diag.sh`
- Plasm config: `cat /etc/plasm/config.json`
- Daemon log: `tail -100 /tmp/plasm-init.log`
- Peer list: `curl http://localhost:4001/network/peers`
- Firewall rules: `iptables -L -n`

---

## Diagnostic Tools

### Using net-diag.sh

The network diagnostics script provides comprehensive network information.

**Location**: `/scripts/net-diag.sh`

**Usage**:
```bash
# Run full diagnostics
/scripts/net-diag.sh
```

**Output Sections**:
1. **Network Interfaces**: Shows all network devices and their state
2. **IP Configuration**: Shows assigned IP addresses
3. **Routing Table**: Shows default gateway and routes
4. **DNS Configuration**: Shows configured DNS servers
5. **Connectivity Tests**: Tests ping to 1.1.1.1, 8.8.8.8, and DNS resolution
6. **DHCP Status**: Shows DHCP lease information
7. **Network Log**: Shows recent network events
8. **Troubleshooting Hints**: Suggests fixes for common issues

**Example Output**:
```
======================================
  Phase Boot Network Diagnostics
======================================

=== Network Interfaces ===
1: lo: <LOOPBACK,UP> ...
2: eth0: <BROADCAST,MULTICAST,UP> ...

=== IP Configuration ===
eth0: 192.168.1.100/24

=== Routing Table ===
default via 192.168.1.1 dev eth0

=== DNS Configuration ===
nameserver 8.8.8.8

=== Connectivity Tests ===
Ping 1.1.1.1 (Cloudflare DNS): OK
Ping 8.8.8.8 (Google DNS): OK
DNS resolution (google.com): OK

=== Troubleshooting ===
No issues detected
```

**Common Issues Detected**:
- No interfaces UP
- No IPv4 addresses assigned
- No default gateway
- Cannot reach internet

---

### Using journalctl and dmesg

System logs provide detailed boot and runtime information.

**dmesg** - Kernel ring buffer (boot messages):
```bash
# View all kernel messages
dmesg

# Search for errors
dmesg | grep -i error
dmesg | grep -i fail

# Filter by subsystem
dmesg | grep -i net       # Network
dmesg | grep -i disk      # Storage
dmesg | grep -i usb       # USB devices

# Follow new messages
dmesg -w

# Show with timestamps
dmesg -T

# Show last 50 lines
dmesg | tail -50
```

**journalctl** - System journal (if systemd present):
```bash
# Note: Phase Boot M1 uses sysvinit, not systemd
# journalctl may not be available in early milestones

# If available:
journalctl -b          # This boot
journalctl -u plasmd   # Plasm daemon logs
journalctl -f          # Follow logs
journalctl --since "10 minutes ago"
```

**Common Log Searches**:
```bash
# Network initialization errors
dmesg | grep -E "eth|net|link|dhcp"

# Module loading issues
dmesg | grep -i "module\|modprobe"

# Memory issues
dmesg | grep -i "oom\|memory"

# Kernel panics
dmesg | grep -i "panic\|oops"
```

---

### Debug Boot Modes

Enable verbose logging for troubleshooting.

**Method 1: Edit Boot Entry Temporarily**

At boot menu:
1. Select desired boot mode
2. Press `e` to edit
3. Find kernel command line (line starting with `linux`)
4. Add debug parameters:
   ```
   debug loglevel=7 phase.debug=true
   ```
5. Press `Ctrl+X` or `F10` to boot

**Method 2: Edit Boot Configuration** (persistent)

Modify `/boot/loader/entries/*.conf`:
```bash
# Mount ESP
mount /dev/sda1 /mnt

# Edit boot entry
vi /mnt/loader/entries/phase-internet.conf

# Add to options line:
options ... debug loglevel=7

# Unmount
umount /mnt
```

**Debug Parameters**:

| Parameter | Effect |
|-----------|--------|
| `debug` | Enable kernel debug output |
| `loglevel=7` | Kernel log level (0=quiet, 7=debug) |
| `phase.debug=true` | Enable Phase Boot debug logging |
| `init=/bin/sh` | Drop to emergency shell (skip init) |
| `single` | Single-user mode |
| `systemd.log_level=debug` | Systemd debug (if systemd used) |

**Interpreting Debug Output**:
- `[    0.000000]` - Kernel timestamp (seconds since boot)
- `[NET-INIT]` - Network initialization script
- `[KEXEC]` - kexec boot script
- `[PLASM-INIT]` - Plasm daemon initialization

**Capturing Boot Logs**:

From emergency shell:
```bash
# Save dmesg to file
dmesg > /tmp/boot.log

# Save to USB (if another USB available)
mount /dev/sdb1 /mnt
dmesg > /mnt/phase-boot.log
umount /mnt
```

From serial console (advanced):
- Add `console=ttyS0,115200` to kernel cmdline
- Connect via serial cable
- Use screen/minicom: `screen /dev/ttyS0 115200`
- All output captured to terminal

---

### Manual Network Diagnostics

If `net-diag.sh` is not available or you need more control:

**Check Interface Status**:
```bash
# List interfaces
ip link show

# Bring interface up
ip link set eth0 up

# Check carrier (cable connected)
cat /sys/class/net/eth0/carrier
```

**Check IP Configuration**:
```bash
# View IP addresses
ip addr show

# View routing table
ip route show

# View ARP table
ip neigh show
```

**Test Connectivity**:
```bash
# Ping by IP (tests routing)
ping -c 3 8.8.8.8

# Ping by name (tests DNS)
ping -c 3 google.com

# Traceroute (if available)
traceroute 8.8.8.8
```

**DHCP Manual Request**:
```bash
# Using udhcpc
udhcpc -i eth0 -n -q -v

# Using dhcpcd
dhcpcd -d eth0
```

**DNS Testing**:
```bash
# Check DNS config
cat /etc/resolv.conf

# Test resolution (if nslookup available)
nslookup google.com 8.8.8.8

# Manually set DNS
echo "nameserver 8.8.8.8" > /etc/resolv.conf
```

**Port Testing** (if nc/netcat available):
```bash
# Test if port is reachable
nc -zv google.com 443

# Test UDP
nc -zvu 8.8.8.8 53
```

---

### Collecting Diagnostic Information

When reporting issues, collect this information:

**System Information**:
```bash
# Architecture and kernel
uname -a

# Memory
free -m

# Disk space
df -h

# CPU info
cat /proc/cpuinfo | head -20

# Kernel command line
cat /proc/cmdline
```

**Network State**:
```bash
# Full network diagnostics
/scripts/net-diag.sh > /tmp/network-diag.txt

# Or manual collection:
ip addr show > /tmp/ip-addr.txt
ip route show > /tmp/ip-route.txt
cat /etc/resolv.conf > /tmp/resolv.txt
```

**Logs**:
```bash
# Kernel messages
dmesg > /tmp/dmesg.txt

# Phase Boot logs
cat /tmp/network.log > /tmp/network-log.txt
cat /tmp/phase-boot.log > /tmp/phase-boot-log.txt
cat /tmp/plasm-init.log > /tmp/plasm-log.txt
```

**Configuration**:
```bash
# Boot configuration
cat /proc/cmdline > /tmp/cmdline.txt

# Network status files
cat /tmp/network.status
cat /tmp/network.interface
cat /tmp/network.ip
```

**Export to USB**:
```bash
# Insert second USB drive
# Mount it
mount /dev/sdb1 /mnt

# Copy all diagnostics
cp /tmp/*.txt /mnt/
cp /tmp/*.log /mnt/

# Unmount
umount /mnt
```

**Create Diagnostic Bundle**:
```bash
# Create archive (if tar available)
cd /tmp
tar czf phase-diagnostics.tar.gz \
  dmesg.txt \
  network.log \
  phase-boot.log \
  plasm-init.log \
  cmdline.txt \
  network-diag.txt

# Copy to USB
mount /dev/sdb1 /mnt
cp phase-diagnostics.tar.gz /mnt/
umount /mnt
```

---

## Getting Help

If this troubleshooting guide doesn't resolve your issue:

1. **Check Documentation**:
   - `/boot/docs/testing.md` - Testing procedures
   - `/boot/docs/tested-hardware.md` - Hardware compatibility

2. **Review Logs**:
   - `/tmp/network.log` - Network initialization
   - `/tmp/phase-boot.log` - Mode handler and kexec
   - `/tmp/plasm-init.log` - Plasm daemon
   - `dmesg` - Kernel messages

3. **Run Diagnostics**:
   - `/scripts/net-diag.sh` - Network diagnostics
   - `free -m` - Memory status
   - `ip addr` - Network configuration

4. **Report Issue**:
   - Include diagnostic information (see above)
   - Describe exact symptoms
   - List steps to reproduce
   - Attach logs and diagnostic bundle

5. **Community Support**:
   - Project repository: https://github.com/msitarzewski/phase
   - Issue tracker: https://github.com/msitarzewski/phase/issues
   - Include diagnostic bundle in issue report

---

## Quick Reference

**Most Common Issues**:

| Symptom | Quick Fix |
|---------|-----------|
| System won't boot from USB | Disable Secure Boot in BIOS |
| Boot hangs at init | Add `debug loglevel=7` to cmdline |
| No network interface | Check VM network adapter or cable |
| DHCP fails | Try `udhcpc -i eth0 -n -q -v` |
| DNS doesn't work | `echo "nameserver 8.8.8.8" > /etc/resolv.conf` |
| Discovery timeout | Increase timeout or check firewall |
| kexec fails to load | Verify `/proc/sys/kernel/kexec_load_disabled` is 0 |
| Plasm won't start | Check `/tmp/plasm-init.log` for errors |

**Essential Commands**:
```bash
/scripts/net-diag.sh           # Network diagnostics
dmesg | grep -i error          # Kernel errors
cat /tmp/network.log           # Network log
cat /tmp/phase-boot.log        # Boot log
free -m                        # Memory status
ip addr show                   # Network config
```

**Log Locations**:
- `/tmp/network.log` - Network initialization
- `/tmp/phase-boot.log` - Mode handler, discovery, kexec
- `/tmp/plasm-init.log` - Plasm daemon
- `dmesg` - Kernel ring buffer
- `/tmp/manifest_url` - Discovered manifest URL
- `/tmp/phase-manifest` - Downloaded manifest

---

*For additional help, see the Phase Boot documentation at `/boot/docs/` or visit the project repository.*

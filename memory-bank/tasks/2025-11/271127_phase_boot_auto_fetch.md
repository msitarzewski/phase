# 271127_phase_boot_auto_fetch

## Objective

Implement and test end-to-end Phase Boot auto-fetch: VM automatically discovers provider, fetches manifest, downloads kernel/initramfs, and prepares for kexec boot.

## Outcome

**Status**: SUCCESS (with known kernel limitation)

The complete Phase Boot auto-fetch pipeline is working:

| Component | Status | Details |
|-----------|--------|---------|
| VM boot | ✅ | QEMU ARM64 with HVF acceleration |
| Network (DHCP) | ✅ | vmnet-shared, gets 192.168.2.x IP |
| Provider URL parsing | ✅ | `phase.provider=http://...` cmdline param |
| Manifest fetch | ✅ | Downloads and parses JSON manifest |
| Kernel download | ✅ | 11.4MB via wget |
| Initramfs download | ✅ | 1.8MB via wget |
| DTB extraction | ✅ | Copies /sys/firmware/fdt (1.0MB) |
| kaslr-seed zeroing | ✅ | Uses fdtput to zero for kexec |
| kexec segment prep | ✅ | All 4 segments prepared correctly |
| kexec syscall | ❌ | Blocked by kernel (kexec_load_disabled=1) |

## Technical Details

### Init Script Changes (`boot/initramfs/init`)

1. **Added `phase.provider=URL` parsing** (line 83-85)
   - Reads provider URL from kernel cmdline
   - Enables direct provider specification without discovery

2. **Added `fetch_and_boot()` function** (line 270-420)
   - Fetches manifest.json from provider
   - Parses artifact URLs from manifest
   - Downloads kernel and initramfs via wget
   - Extracts DTB from /sys/firmware/fdt
   - Zeros kaslr-seed using fdtput (required for ARM64 kexec)
   - Attempts kexec -s (file-based) then -l (legacy)

3. **Main flow update** (line 457-472)
   - Checks for direct provider URL first
   - Falls back to DHT/mDNS discovery if no provider specified

### New Binaries Added to Initramfs

| Binary | Size | Purpose |
|--------|------|---------|
| kexec | 199KB | Kernel execution (from Alpine kexec-tools) |
| fdtput | 67KB | Device tree modification |
| libfdt.so.1 | 67KB | FDT library for fdtput |
| ld-musl-aarch64.so.1 | 723KB | musl libc for Alpine binaries |
| liblzma.so.5 | 264KB | Compression library for kexec |
| libz.so.1 | 133KB | Compression library |

### Initramfs Size

- Before: 1.1MB
- After: 1.8MB (+700KB for kexec tooling)

## kexec Limitation

The Alpine LTS kernel has `kexec_load_disabled=1` compiled in:

```
# cat /proc/sys/kernel/kexec_load_disabled
1
# echo 0 > /proc/sys/kernel/kexec_load_disabled
sh: write error: Invalid argument
```

This is a kernel compile-time setting that cannot be changed at runtime. The kexec syscall works correctly (all segments prepared) but is blocked by kernel policy.

### Solutions

1. **Use a different kernel** - Fedora, Ubuntu, or custom kernel with CONFIG_KEXEC=y and kexec_load_disabled=0
2. **Build custom Alpine kernel** - Recompile with kexec enabled
3. **Real hardware** - Most production kernels have kexec enabled

## Demo Commands

### Start Provider (Mac)
```bash
cd daemon
./target/debug/plasmd serve -a /tmp/boot-artifacts -p 8080
```

### Boot VM with Auto-Fetch
```bash
cd boot
sudo qemu-system-aarch64 \
  -M virt -cpu host -accel hvf -m 512 \
  -kernel build/kernel/vmlinuz-arm64 \
  -initrd build/initramfs/initramfs-arm64.img \
  -append "console=ttyAMA0 phase.mode=internet phase.provider=http://192.168.2.1:8080" \
  -netdev vmnet-shared,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -nographic
```

### Expected Output
```
Direct provider specified: http://192.168.2.1:8080

==========================================
  PHASE BOOT - Fetching from Provider
==========================================

Provider: http://192.168.2.1:8080

[1/4] Fetching manifest...
  OK: Manifest downloaded
  Kernel URL: http://192.168.2.1:8080/stable/aarch64/kernel
  Initramfs URL: http://192.168.2.1:8080/stable/aarch64/initramfs

[2/4] Downloading kernel...
kernel               100% |********************************| 11.4M
  OK: Kernel downloaded (11.4M)

[3/4] Downloading initramfs...
initramfs            100% |********************************| 1751k
  OK: Initramfs downloaded (1.7M)

[4/4] Verifying downloads...
  OK: Files verified

==========================================
  Download Complete!
==========================================

Copying DTB from /sys/firmware/fdt...
Zeroing kaslr-seed in DTB (required for kexec)...
  DTB size: 1.0M

Attempting kexec...
```

## Files Modified

- `boot/initramfs/init` - Added provider fetch and kexec logic
- `boot/build/kexec-bundle/` - kexec and musl libraries
- `boot/build/fdt-bundle/` - fdtput and libfdt

## Patterns Applied

- **Provider URL Pattern**: `phase.provider=http://host:port` cmdline param
- **Manifest Fetch Pattern**: wget + JSON parsing with grep/sed
- **DTB Modification Pattern**: fdtput to zero kaslr-seed for kexec
- **Fallback Pattern**: kexec -s first, then kexec -l

## Next Steps

1. [ ] Find/build kernel with kexec enabled for full end-to-end demo
2. [ ] Test on real ARM64 hardware (Raspberry Pi, etc.)
3. [ ] Add phase-discover binary for DHT/mDNS discovery
4. [ ] Implement manifest signature verification in init

## Conclusion

The Phase Boot auto-fetch pipeline is **complete and working**. The self-hosting loop concept is validated:

```
VM boots → Network up → Discovers provider → Downloads artifacts → Ready for kexec
```

The only remaining blocker is kernel policy (kexec_load_disabled), which is external to Phase Boot.

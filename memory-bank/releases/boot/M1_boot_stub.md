# M1 — Boot Stub & Media Layout

**Objective**  
Create dual-arch USB image with UEFI bootloader and seed initramfs for Internet/Local/Private entries.

**Deliverables**  
- ESP partition with systemd-boot (or GRUB), entries for 3 modes.  
- Seed kernel+initramfs for x86_64 and arm64.  
- Partition layout: ESP (FAT32), RO seed (squashfs), optional cache (ext4).  
- `Makefile` targets: `esp`, `initramfs`, `rootfs`, `usb`.

**Acceptance Criteria**  
- USB enumerates and shows menu on UEFI x86_64 + ARM64 boards.  
- Selecting any mode boots into seed initramfs shell with network tools available.

**Tasks**  
- [ ] ESP skeleton (`EFI/BOOT/BOOTX64.EFI`, `BOOTAA64.EFI`, loader entries).  
- [ ] Seed kernels: enable `kexec`, `overlayfs`, common NICs; arm64 DTBs for ref boards.  
- [ ] Initramfs base: busybox, kexec-tools, iproute2, dhcpcd/wpa_supplicant.  
- [ ] Rootfs seed squashfs minimal userspace.  
- [ ] `Makefile` + `scripts/write-usb.sh`.

**Risks**  
- UEFI quirks on some ARM64 boards → keep GRUB fallback.  
- Driver gaps → maintain a “tested boards” section.

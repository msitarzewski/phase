# Phase Boot - Comprehensive Build System
# Build bootable USB images for x86_64 and ARM64 architectures

#------------------------------------------------------------------------------
# Configuration
#------------------------------------------------------------------------------

# Default architecture (can be overridden: make ARCH=arm64)
ARCH ?= x86_64

# Version and branding
VERSION := 0.1.0-M1
PROJECT := phase-boot
IMAGE_NAME := $(PROJECT)-$(ARCH).img

# Build directories
BUILD_DIR := build
ESP_BUILD := $(BUILD_DIR)/esp
INITRAMFS_BUILD := $(BUILD_DIR)/initramfs
ROOTFS_BUILD := $(BUILD_DIR)/rootfs
CACHE_BUILD := $(BUILD_DIR)/cache
KERNEL_DIR := kernel
SCRIPTS_DIR := scripts

# Source directories
ESP_SRC := esp
INITRAMFS_SRC := initramfs
ROOTFS_SRC := rootfs

# Image configuration
IMAGE_SIZE := 4096  # MB (4GB)
ESP_SIZE := 512     # MB
CACHE_SIZE := 1024  # MB
ROOTFS_SIZE := 512  # MB

# Kernel URLs (using mainstream kernel.org stable kernels)
KERNEL_VERSION := 6.6.59
KERNEL_BASE_URL := https://cdn.kernel.org/pub/linux/kernel/v6.x
KERNEL_X86_64 := linux-$(KERNEL_VERSION).tar.xz
KERNEL_ARM64 := linux-$(KERNEL_VERSION).tar.xz

# Bootloader
SYSTEMD_BOOT_X86 := /usr/lib/systemd/boot/efi/systemd-bootx64.efi
SYSTEMD_BOOT_ARM := /usr/lib/systemd/boot/efi/systemd-bootaa64.efi

# Tools
MKSQUASHFS := mksquashfs
MKFS_FAT := mkfs.vfat
MKFS_EXT4 := mkfs.ext4
SGDISK := sgdisk
DD := dd
QEMU_X86 := qemu-system-x86_64
QEMU_ARM := qemu-system-aarch64

# Colors for output
COLOR_RESET := \033[0m
COLOR_BOLD := \033[1m
COLOR_GREEN := \033[32m
COLOR_BLUE := \033[34m
COLOR_YELLOW := \033[33m

#------------------------------------------------------------------------------
# Phony Targets
#------------------------------------------------------------------------------

.PHONY: all clean help deps \
        esp kernel-x86_64 kernel-arm64 initramfs rootfs image \
        test-qemu-x86 test-qemu-arm \
        verify dirs

#------------------------------------------------------------------------------
# Default Target
#------------------------------------------------------------------------------

all: dirs esp initramfs rootfs image
	@echo "$(COLOR_GREEN)$(COLOR_BOLD)✓ Build complete: $(BUILD_DIR)/$(IMAGE_NAME)$(COLOR_RESET)"

#------------------------------------------------------------------------------
# Help Target
#------------------------------------------------------------------------------

help:
	@echo "$(COLOR_BOLD)Phase Boot - Build System$(COLOR_RESET)"
	@echo ""
	@echo "$(COLOR_BOLD)Usage:$(COLOR_RESET)"
	@echo "  make [target] [ARCH=x86_64|arm64]"
	@echo ""
	@echo "$(COLOR_BOLD)Main Targets:$(COLOR_RESET)"
	@echo "  $(COLOR_BLUE)all$(COLOR_RESET)              - Build everything (default)"
	@echo "  $(COLOR_BLUE)clean$(COLOR_RESET)            - Remove all build artifacts"
	@echo "  $(COLOR_BLUE)help$(COLOR_RESET)             - Show this help message"
	@echo ""
	@echo "$(COLOR_BOLD)Component Targets:$(COLOR_RESET)"
	@echo "  $(COLOR_BLUE)esp$(COLOR_RESET)              - Build ESP partition contents"
	@echo "  $(COLOR_BLUE)kernel-x86_64$(COLOR_RESET)    - Download/prepare x86_64 kernel"
	@echo "  $(COLOR_BLUE)kernel-arm64$(COLOR_RESET)     - Download/prepare ARM64 kernel"
	@echo "  $(COLOR_BLUE)initramfs$(COLOR_RESET)        - Build initramfs for current ARCH"
	@echo "  $(COLOR_BLUE)rootfs$(COLOR_RESET)           - Build seed SquashFS"
	@echo "  $(COLOR_BLUE)image$(COLOR_RESET)            - Assemble full USB image"
	@echo ""
	@echo "$(COLOR_BOLD)Testing Targets:$(COLOR_RESET)"
	@echo "  $(COLOR_BLUE)test-qemu-x86$(COLOR_RESET)    - Test x86_64 image in QEMU"
	@echo "  $(COLOR_BLUE)test-qemu-arm$(COLOR_RESET)    - Test ARM64 image in QEMU"
	@echo ""
	@echo "$(COLOR_BOLD)Utility Targets:$(COLOR_RESET)"
	@echo "  $(COLOR_BLUE)deps$(COLOR_RESET)             - Check/install dependencies"
	@echo "  $(COLOR_BLUE)verify$(COLOR_RESET)           - Verify build outputs"
	@echo "  $(COLOR_BLUE)dirs$(COLOR_RESET)             - Create build directories"
	@echo ""
	@echo "$(COLOR_BOLD)Configuration:$(COLOR_RESET)"
	@echo "  ARCH           = $(ARCH)"
	@echo "  VERSION        = $(VERSION)"
	@echo "  IMAGE_SIZE     = $(IMAGE_SIZE) MB"
	@echo "  BUILD_DIR      = $(BUILD_DIR)"
	@echo ""
	@echo "$(COLOR_BOLD)Examples:$(COLOR_RESET)"
	@echo "  make                    # Build x86_64 image"
	@echo "  make ARCH=arm64         # Build ARM64 image"
	@echo "  make test-qemu-x86      # Test in QEMU"
	@echo "  make clean all          # Clean rebuild"
	@echo ""

#------------------------------------------------------------------------------
# Directory Creation
#------------------------------------------------------------------------------

dirs:
	@echo "$(COLOR_YELLOW)→ Creating build directories...$(COLOR_RESET)"
	@mkdir -p $(BUILD_DIR)
	@mkdir -p $(ESP_BUILD)
	@mkdir -p $(INITRAMFS_BUILD)
	@mkdir -p $(ROOTFS_BUILD)
	@mkdir -p $(CACHE_BUILD)
	@mkdir -p $(KERNEL_DIR)
	@mkdir -p $(SCRIPTS_DIR)
	@echo "$(COLOR_GREEN)✓ Build directories created$(COLOR_RESET)"

#------------------------------------------------------------------------------
# Dependency Checking
#------------------------------------------------------------------------------

deps:
	@echo "$(COLOR_YELLOW)→ Checking dependencies...$(COLOR_RESET)"
	@echo -n "  mksquashfs: "; command -v $(MKSQUASHFS) >/dev/null && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗ (install squashfs-tools)$(COLOR_RESET)"
	@echo -n "  mkfs.vfat:  "; command -v $(MKFS_FAT) >/dev/null && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗ (install dosfstools)$(COLOR_RESET)"
	@echo -n "  mkfs.ext4:  "; command -v $(MKFS_EXT4) >/dev/null && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗ (install e2fsprogs)$(COLOR_RESET)"
	@echo -n "  sgdisk:     "; command -v $(SGDISK) >/dev/null && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗ (install gdisk)$(COLOR_RESET)"
	@echo -n "  qemu-x86:   "; command -v $(QEMU_X86) >/dev/null && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗ (install qemu-system-x86)$(COLOR_RESET)"
	@echo -n "  qemu-arm:   "; command -v $(QEMU_ARM) >/dev/null && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗ (install qemu-system-arm)$(COLOR_RESET)"
	@echo -n "  cpio:       "; command -v cpio >/dev/null && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗ (install cpio)$(COLOR_RESET)"
	@echo -n "  gzip:       "; command -v gzip >/dev/null && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗ (install gzip)$(COLOR_RESET)"
	@echo -n "  wget:       "; command -v wget >/dev/null && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗ (install wget)$(COLOR_RESET)"
	@echo ""
	@echo "$(COLOR_BOLD)To install all dependencies on Ubuntu/Debian:$(COLOR_RESET)"
	@echo "  sudo apt-get install squashfs-tools dosfstools e2fsprogs gdisk \\"
	@echo "                       qemu-system-x86 qemu-system-arm cpio gzip wget \\"
	@echo "                       busybox-static systemd-boot-efi"

#------------------------------------------------------------------------------
# Kernel Download/Preparation
#------------------------------------------------------------------------------

kernel-x86_64: dirs
	@echo "$(COLOR_YELLOW)→ Preparing x86_64 kernel...$(COLOR_RESET)"
	@if [ ! -f "$(KERNEL_DIR)/vmlinuz-x86_64" ]; then \
		echo "  Checking for pre-built kernel..."; \
		if [ -f "/boot/vmlinuz" ]; then \
			echo "  Using host kernel: /boot/vmlinuz"; \
			cp /boot/vmlinuz $(KERNEL_DIR)/vmlinuz-x86_64; \
		else \
			echo "  $(COLOR_YELLOW)No kernel found. Please provide vmlinuz-x86_64$(COLOR_RESET)"; \
			echo "  Place kernel at: $(KERNEL_DIR)/vmlinuz-x86_64"; \
			exit 1; \
		fi \
	fi
	@echo "$(COLOR_GREEN)✓ x86_64 kernel ready$(COLOR_RESET)"

kernel-arm64: dirs
	@echo "$(COLOR_YELLOW)→ Preparing ARM64 kernel...$(COLOR_RESET)"
	@if [ ! -f "$(KERNEL_DIR)/vmlinuz-arm64" ]; then \
		echo "  $(COLOR_YELLOW)ARM64 kernel not found$(COLOR_RESET)"; \
		echo "  Place kernel at: $(KERNEL_DIR)/vmlinuz-arm64"; \
		echo "  (You may need to cross-compile or download from distro)"; \
		exit 1; \
	fi
	@echo "$(COLOR_GREEN)✓ ARM64 kernel ready$(COLOR_RESET)"

#------------------------------------------------------------------------------
# ESP Partition Build
#------------------------------------------------------------------------------

esp: dirs kernel-$(ARCH)
	@echo "$(COLOR_YELLOW)→ Building ESP partition for $(ARCH)...$(COLOR_RESET)"

	# Create ESP directory structure
	@mkdir -p $(ESP_BUILD)/EFI/BOOT
	@mkdir -p $(ESP_BUILD)/loader/entries
	@mkdir -p $(ESP_BUILD)/dtbs

	# Copy bootloader (systemd-boot)
	@if [ "$(ARCH)" = "x86_64" ]; then \
		if [ -f "$(SYSTEMD_BOOT_X86)" ]; then \
			cp $(SYSTEMD_BOOT_X86) $(ESP_BUILD)/EFI/BOOT/BOOTX64.EFI; \
		else \
			echo "$(COLOR_YELLOW)  Warning: systemd-boot not found, creating placeholder$(COLOR_RESET)"; \
			touch $(ESP_BUILD)/EFI/BOOT/BOOTX64.EFI; \
		fi \
	elif [ "$(ARCH)" = "arm64" ]; then \
		if [ -f "$(SYSTEMD_BOOT_ARM)" ]; then \
			cp $(SYSTEMD_BOOT_ARM) $(ESP_BUILD)/EFI/BOOT/BOOTAA64.EFI; \
		else \
			echo "$(COLOR_YELLOW)  Warning: systemd-boot ARM64 not found, creating placeholder$(COLOR_RESET)"; \
			touch $(ESP_BUILD)/EFI/BOOT/BOOTAA64.EFI; \
		fi \
	fi

	# Copy kernel
	@echo "  Copying kernel..."
	@cp $(KERNEL_DIR)/vmlinuz-$(ARCH) $(ESP_BUILD)/vmlinuz-$(ARCH)

	# Copy boot loader configuration
	@echo "  Copying boot configurations..."
	@cp -r $(ESP_SRC)/loader/loader.conf $(ESP_BUILD)/loader/
	@cp -r $(ESP_SRC)/loader/entries/*.conf $(ESP_BUILD)/loader/entries/

	# Update kernel paths in boot entries for current architecture
	@sed -i 's|/vmlinuz-x86_64|/vmlinuz-$(ARCH)|g' $(ESP_BUILD)/loader/entries/*.conf
	@sed -i 's|/initramfs-x86_64.img|/initramfs-$(ARCH).img|g' $(ESP_BUILD)/loader/entries/*.conf

	@echo "$(COLOR_GREEN)✓ ESP partition built$(COLOR_RESET)"

#------------------------------------------------------------------------------
# Initramfs Build
#------------------------------------------------------------------------------

initramfs: dirs
	@echo "$(COLOR_YELLOW)→ Building initramfs for $(ARCH)...$(COLOR_RESET)"

	# Create initramfs working directory
	@rm -rf $(INITRAMFS_BUILD)/work
	@mkdir -p $(INITRAMFS_BUILD)/work

	# Copy initramfs structure
	@echo "  Copying initramfs structure..."
	@cp -a $(INITRAMFS_SRC)/* $(INITRAMFS_BUILD)/work/

	# Ensure init script is executable
	@chmod +x $(INITRAMFS_BUILD)/work/init

	# Copy busybox (if available) for basic utilities
	@if [ -f /bin/busybox ]; then \
		echo "  Installing busybox..."; \
		mkdir -p $(INITRAMFS_BUILD)/work/bin; \
		cp /bin/busybox $(INITRAMFS_BUILD)/work/bin/busybox; \
		(cd $(INITRAMFS_BUILD)/work/bin && \
			for cmd in sh ls cat mount umount ip ifconfig ps grep sleep; do \
				ln -sf busybox $$cmd 2>/dev/null || true; \
			done); \
	fi

	# Install essential utilities if available
	@echo "  Installing essential utilities..."
	@for util in /sbin/ip /bin/mount /bin/umount /sbin/udhcpc; do \
		if [ -f $$util ]; then \
			mkdir -p $(INITRAMFS_BUILD)/work/$$(dirname $$util); \
			cp $$util $(INITRAMFS_BUILD)/work/$$util 2>/dev/null || true; \
		fi \
	done

	# Create device nodes
	@echo "  Creating device nodes..."
	@mkdir -p $(INITRAMFS_BUILD)/work/dev
	@cd $(INITRAMFS_BUILD)/work/dev && \
		(mknod -m 600 console c 5 1 2>/dev/null || true) && \
		(mknod -m 666 null c 1 3 2>/dev/null || true) && \
		(mknod -m 666 zero c 1 5 2>/dev/null || true) && \
		true

	# Build initramfs archive
	@echo "  Creating initramfs archive..."
	@cd $(INITRAMFS_BUILD)/work && \
		find . -print0 | cpio --null --create --verbose --format=newc 2>/dev/null | \
		gzip -9 > ../initramfs-$(ARCH).img

	# Copy to ESP
	@cp $(INITRAMFS_BUILD)/initramfs-$(ARCH).img $(ESP_BUILD)/

	@echo "$(COLOR_GREEN)✓ Initramfs built: $(INITRAMFS_BUILD)/initramfs-$(ARCH).img$(COLOR_RESET)"
	@ls -lh $(INITRAMFS_BUILD)/initramfs-$(ARCH).img

#------------------------------------------------------------------------------
# Rootfs Build (Seed SquashFS)
#------------------------------------------------------------------------------

rootfs: dirs
	@echo "$(COLOR_YELLOW)→ Building seed rootfs (SquashFS)...$(COLOR_RESET)"

	# Create rootfs working directory
	@rm -rf $(ROOTFS_BUILD)/work
	@mkdir -p $(ROOTFS_BUILD)/work

	# Copy rootfs structure
	@echo "  Copying rootfs structure..."
	@cp -a $(ROOTFS_SRC)/* $(ROOTFS_BUILD)/work/

	# Create additional directories
	@mkdir -p $(ROOTFS_BUILD)/work/{dev,proc,sys,run,tmp}
	@mkdir -p $(ROOTFS_BUILD)/work/var/{log,cache,lib,tmp}
	@mkdir -p $(ROOTFS_BUILD)/work/usr/{bin,sbin,lib}
	@mkdir -p $(ROOTFS_BUILD)/work/etc/{network,init.d}

	# Install busybox if available
	@if [ -f /bin/busybox ]; then \
		echo "  Installing busybox utilities..."; \
		mkdir -p $(ROOTFS_BUILD)/work/bin; \
		cp /bin/busybox $(ROOTFS_BUILD)/work/bin/busybox; \
	fi

	# Create version file
	@echo "Phase Boot $(VERSION)" > $(ROOTFS_BUILD)/work/etc/phase-version
	@echo "Architecture: $(ARCH)" >> $(ROOTFS_BUILD)/work/etc/phase-version
	@echo "Build date: $$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> $(ROOTFS_BUILD)/work/etc/phase-version

	# Build SquashFS
	@echo "  Creating SquashFS image..."
	@$(MKSQUASHFS) $(ROOTFS_BUILD)/work $(ROOTFS_BUILD)/rootfs-$(ARCH).sqfs \
		-comp xz -b 1M -noappend -no-recovery 2>/dev/null || \
		$(MKSQUASHFS) $(ROOTFS_BUILD)/work $(ROOTFS_BUILD)/rootfs-$(ARCH).sqfs \
		-noappend -no-recovery

	@echo "$(COLOR_GREEN)✓ Rootfs built: $(ROOTFS_BUILD)/rootfs-$(ARCH).sqfs$(COLOR_RESET)"
	@ls -lh $(ROOTFS_BUILD)/rootfs-$(ARCH).sqfs

#------------------------------------------------------------------------------
# Full USB Image Assembly
#------------------------------------------------------------------------------

image: esp initramfs rootfs
	@echo "$(COLOR_YELLOW)→ Assembling USB image: $(IMAGE_NAME)...$(COLOR_RESET)"

	# Create empty image file
	@echo "  Creating $(IMAGE_SIZE)MB disk image..."
	@$(DD) if=/dev/zero of=$(BUILD_DIR)/$(IMAGE_NAME) bs=1M count=$(IMAGE_SIZE) status=progress 2>/dev/null || \
		$(DD) if=/dev/zero of=$(BUILD_DIR)/$(IMAGE_NAME) bs=1M count=$(IMAGE_SIZE)

	# Create GPT partition table
	@echo "  Creating GPT partition table..."
	@$(SGDISK) -Z $(BUILD_DIR)/$(IMAGE_NAME) >/dev/null 2>&1 || true
	@$(SGDISK) -n 1:0:+$(ESP_SIZE)M -t 1:ef00 -c 1:"EFI System" $(BUILD_DIR)/$(IMAGE_NAME) >/dev/null
	@$(SGDISK) -n 2:0:+$(ROOTFS_SIZE)M -t 2:8300 -c 2:"Phase Seed" $(BUILD_DIR)/$(IMAGE_NAME) >/dev/null
	@$(SGDISK) -n 3:0:+$(CACHE_SIZE)M -t 3:8300 -c 3:"Phase Cache" $(BUILD_DIR)/$(IMAGE_NAME) >/dev/null
	@$(SGDISK) -p $(BUILD_DIR)/$(IMAGE_NAME) >/dev/null

	# Set up loop device and format partitions
	@echo "  Formatting partitions (requires sudo)..."
	@echo "  $(COLOR_YELLOW)Note: The following commands require root privileges$(COLOR_RESET)"
	@bash $(SCRIPTS_DIR)/build-image.sh $(BUILD_DIR)/$(IMAGE_NAME) $(ESP_BUILD) $(ROOTFS_BUILD)/rootfs-$(ARCH).sqfs

	@echo "$(COLOR_GREEN)$(COLOR_BOLD)✓ USB image created: $(BUILD_DIR)/$(IMAGE_NAME)$(COLOR_RESET)"
	@ls -lh $(BUILD_DIR)/$(IMAGE_NAME)

#------------------------------------------------------------------------------
# QEMU Testing
#------------------------------------------------------------------------------

test-qemu-x86: ARCH=x86_64
test-qemu-x86:
	@echo "$(COLOR_YELLOW)→ Testing x86_64 image in QEMU...$(COLOR_RESET)"
	@if [ ! -f "$(BUILD_DIR)/phase-boot-x86_64.img" ]; then \
		echo "$(COLOR_YELLOW)Image not found. Building first...$(COLOR_RESET)"; \
		$(MAKE) ARCH=x86_64 all; \
	fi
	@echo "  Starting QEMU (press Ctrl+A then X to exit)..."
	@echo ""
	@$(QEMU_X86) \
		-machine q35 \
		-cpu qemu64 \
		-m 2048 \
		-bios /usr/share/ovmf/OVMF.fd \
		-drive file=$(BUILD_DIR)/phase-boot-x86_64.img,format=raw,if=virtio \
		-net nic,model=virtio \
		-net user \
		-serial stdio \
		-display none || \
	$(QEMU_X86) \
		-machine q35 \
		-cpu qemu64 \
		-m 2048 \
		-drive file=$(BUILD_DIR)/phase-boot-x86_64.img,format=raw,if=virtio \
		-net nic,model=virtio \
		-net user \
		-serial stdio \
		-display none

test-qemu-arm: ARCH=arm64
test-qemu-arm:
	@echo "$(COLOR_YELLOW)→ Testing ARM64 image in QEMU...$(COLOR_RESET)"
	@if [ ! -f "$(BUILD_DIR)/phase-boot-arm64.img" ]; then \
		echo "$(COLOR_YELLOW)Image not found. Building first...$(COLOR_RESET)"; \
		$(MAKE) ARCH=arm64 all; \
	fi
	@echo "  Starting QEMU (press Ctrl+A then X to exit)..."
	@echo ""
	@$(QEMU_ARM) \
		-machine virt \
		-cpu cortex-a57 \
		-m 2048 \
		-bios /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
		-drive file=$(BUILD_DIR)/phase-boot-arm64.img,format=raw,if=virtio \
		-net nic,model=virtio \
		-net user \
		-serial stdio \
		-display none || \
	$(QEMU_ARM) \
		-machine virt \
		-cpu cortex-a57 \
		-m 2048 \
		-drive file=$(BUILD_DIR)/phase-boot-arm64.img,format=raw,if=virtio \
		-net nic,model=virtio \
		-net user \
		-serial stdio \
		-display none

#------------------------------------------------------------------------------
# Verification
#------------------------------------------------------------------------------

verify:
	@echo "$(COLOR_YELLOW)→ Verifying build outputs...$(COLOR_RESET)"
	@echo -n "  ESP directory:     "; [ -d "$(ESP_BUILD)" ] && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗$(COLOR_RESET)"
	@echo -n "  Kernel:            "; [ -f "$(ESP_BUILD)/vmlinuz-$(ARCH)" ] && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗$(COLOR_RESET)"
	@echo -n "  Initramfs:         "; [ -f "$(INITRAMFS_BUILD)/initramfs-$(ARCH).img" ] && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗$(COLOR_RESET)"
	@echo -n "  Rootfs:            "; [ -f "$(ROOTFS_BUILD)/rootfs-$(ARCH).sqfs" ] && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗$(COLOR_RESET)"
	@echo -n "  USB Image:         "; [ -f "$(BUILD_DIR)/$(IMAGE_NAME)" ] && echo "$(COLOR_GREEN)✓$(COLOR_RESET)" || echo "$(COLOR_YELLOW)✗$(COLOR_RESET)"
	@if [ -f "$(BUILD_DIR)/$(IMAGE_NAME)" ]; then \
		echo ""; \
		echo "$(COLOR_BOLD)Image details:$(COLOR_RESET)"; \
		ls -lh $(BUILD_DIR)/$(IMAGE_NAME); \
		echo ""; \
		echo "$(COLOR_BOLD)Partition table:$(COLOR_RESET)"; \
		$(SGDISK) -p $(BUILD_DIR)/$(IMAGE_NAME) 2>/dev/null || fdisk -l $(BUILD_DIR)/$(IMAGE_NAME) 2>/dev/null || true; \
	fi

#------------------------------------------------------------------------------
# Clean
#------------------------------------------------------------------------------

clean:
	@echo "$(COLOR_YELLOW)→ Cleaning build artifacts...$(COLOR_RESET)"
	@rm -rf $(BUILD_DIR)
	@echo "$(COLOR_GREEN)✓ Clean complete$(COLOR_RESET)"

#------------------------------------------------------------------------------
# Build Helper Scripts
#------------------------------------------------------------------------------

# Create the image building script if it doesn't exist
$(SCRIPTS_DIR)/build-image.sh: dirs
	@echo "$(COLOR_YELLOW)→ Creating image build script...$(COLOR_RESET)"
	@printf '#!/bin/bash\n' > $@
	@printf 'set -e\n\n' >> $@
	@printf 'IMAGE_FILE="$$1"\n' >> $@
	@printf 'ESP_SOURCE="$$2"\n' >> $@
	@printf 'ROOTFS_SOURCE="$$3"\n\n' >> $@
	@printf 'if [ "$$EUID" -ne 0 ]; then\n' >> $@
	@printf '    echo "Requesting sudo for partition operations..."\n' >> $@
	@printf '    exec sudo "$$0" "$$@"\n' >> $@
	@printf 'fi\n\n' >> $@
	@printf 'echo "Setting up loop device..."\n' >> $@
	@printf 'LOOP_DEV=$$(losetup -f)\n' >> $@
	@printf 'losetup -P "$$LOOP_DEV" "$$IMAGE_FILE"\n\n' >> $@
	@printf '# Wait for partition devices\n' >> $@
	@printf 'sleep 1\n\n' >> $@
	@printf 'echo "Formatting ESP partition (FAT32)..."\n' >> $@
	@printf 'mkfs.vfat -F 32 -n "PHASE_ESP" "$${LOOP_DEV}p1" >/dev/null\n\n' >> $@
	@printf 'echo "Copying ESP contents..."\n' >> $@
	@printf 'MOUNT_POINT=$$(mktemp -d)\n' >> $@
	@printf 'mount "$${LOOP_DEV}p1" "$$MOUNT_POINT"\n' >> $@
	@printf 'cp -r "$$ESP_SOURCE"/* "$$MOUNT_POINT"/\n' >> $@
	@printf 'sync\n' >> $@
	@printf 'umount "$$MOUNT_POINT"\n\n' >> $@
	@printf 'echo "Creating seed partition..."\n' >> $@
	@printf 'dd if="$$ROOTFS_SOURCE" of="$${LOOP_DEV}p2" bs=1M status=none\n\n' >> $@
	@printf 'echo "Formatting cache partition (ext4)..."\n' >> $@
	@printf 'mkfs.ext4 -L "PHASE_CACHE" -q "$${LOOP_DEV}p3" >/dev/null\n\n' >> $@
	@printf 'echo "Cleaning up..."\n' >> $@
	@printf 'losetup -d "$$LOOP_DEV"\n' >> $@
	@printf 'rm -rf "$$MOUNT_POINT"\n\n' >> $@
	@printf 'echo "Image build complete!"\n' >> $@
	@chmod +x $@
	@echo "$(COLOR_GREEN)✓ Image build script created$(COLOR_RESET)"

# Ensure script exists before image target
image: $(SCRIPTS_DIR)/build-image.sh

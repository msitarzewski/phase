# Task 1 — Network Bring-up Scripts


**Agent**: Tooling Agent
**Estimated**: 5 days

#### 1.1 Wired network initialization
- [ ] Script: `boot/initramfs/scripts/net-wired.sh`
- [ ] Steps:
  - Detect wired interfaces: `ip link show` (filter `eth*`, `en*`)
  - Bring interface up: `ip link set <iface> up`
  - Request DHCP: `dhcpcd <iface> --timeout 10 --waitip`
  - Verify connectivity: `ping -c 1 -W 2 1.1.1.1`
- [ ] Retry logic: 3 attempts with 5-second backoff
- [ ] Logging: Output to console and `/tmp/network.log`

**Dependencies**: M1 Task 4.3 (network tools installed)
**Output**: Wired network bring-up script

#### 1.2 Wireless network configuration helper
- [ ] Script: `boot/initramfs/scripts/net-wifi.sh`
- [ ] Responsibilities:
  - Detect wireless interfaces: `ip link show` (filter `wl*`)
  - Prompt for SSID (if not pre-configured)
  - Prompt for passphrase (if WPA/WPA2)
  - Generate `wpa_supplicant.conf`: `/tmp/wpa_supplicant.conf`
  - Start wpa_supplicant: `wpa_supplicant -B -i <iface> -c /tmp/wpa_supplicant.conf`
  - Request DHCP: `dhcpcd <iface> --timeout 30`
- [ ] Mode behavior:
  - Internet/Local: Prompt if no wired connection
  - Private: Prompt, but warn about privacy implications

**Dependencies**: M1 Task 4.3 (wpa_supplicant installed)
**Output**: Wi-Fi configuration script

#### 1.3 Network mode dispatcher
- [ ] Script: `boot/initramfs/scripts/net-init.sh`
- [ ] Logic:
  - Parse `phase.mode` from `/proc/cmdline`
  - Try wired first (all modes)
  - If wired fails:
    - Internet/Local: Prompt for Wi-Fi
    - Private: Prompt with warning
  - Set network status: `/tmp/network.status` (up/down)
- [ ] Integration point: Called from `boot/initramfs/init`

**Dependencies**: Tasks 1.1, 1.2
**Output**: Network mode dispatcher script

#### 1.4 Fallback and troubleshooting
- [ ] Diagnostic script: `boot/initramfs/scripts/net-diag.sh`
- [ ] Outputs:
  - Interface status: `ip link show`
  - IP configuration: `ip addr show`
  - Routes: `ip route show`
  - DNS: `cat /etc/resolv.conf`
  - Connectivity test: `ping -c 3 1.1.1.1`
- [ ] Provide troubleshooting hints:
  - No DHCP lease → Check DHCP server
  - No connectivity → Check firewall, gateway
  - Wi-Fi not connecting → Check SSID, passphrase

**Dependencies**: Tasks 1.1-1.3
**Output**: Diagnostic script

---

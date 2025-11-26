#!/bin/sh
#
# Phase Boot - Network Diagnostics
# Provides troubleshooting information for network issues
#

echo "======================================"
echo "  Phase Boot Network Diagnostics"
echo "======================================"
echo ""

# Interface status
echo "=== Network Interfaces ==="
ip link show 2>/dev/null || echo "ip command not available"
echo ""

# IP configuration
echo "=== IP Configuration ==="
ip addr show 2>/dev/null || echo "ip command not available"
echo ""

# Routes
echo "=== Routing Table ==="
ip route show 2>/dev/null || echo "ip command not available"
echo ""

# DNS
echo "=== DNS Configuration ==="
if [ -f /etc/resolv.conf ]; then
    cat /etc/resolv.conf
else
    echo "No /etc/resolv.conf found"
fi
echo ""

# Connectivity tests
echo "=== Connectivity Tests ==="

echo -n "Ping 1.1.1.1 (Cloudflare DNS): "
if ping -c 1 -W 2 1.1.1.1 >/dev/null 2>&1; then
    echo "OK"
else
    echo "FAILED"
fi

echo -n "Ping 8.8.8.8 (Google DNS): "
if ping -c 1 -W 2 8.8.8.8 >/dev/null 2>&1; then
    echo "OK"
else
    echo "FAILED"
fi

# Try DNS resolution if ping works
if ping -c 1 -W 2 1.1.1.1 >/dev/null 2>&1; then
    echo -n "DNS resolution (google.com): "
    if ping -c 1 -W 2 google.com >/dev/null 2>&1; then
        echo "OK"
    else
        echo "FAILED (DNS issue)"
    fi
fi
echo ""

# DHCP status
echo "=== DHCP Status ==="
if [ -f /var/lib/dhcpcd/dhcpcd-*.lease ] 2>/dev/null; then
    echo "DHCP lease files found"
    ls -la /var/lib/dhcpcd/dhcpcd-*.lease 2>/dev/null
elif [ -f /var/run/udhcpc.*.pid ] 2>/dev/null; then
    echo "udhcpc running"
else
    echo "No DHCP lease information found"
fi
echo ""

# Network log
echo "=== Network Log ==="
if [ -f /tmp/network.log ]; then
    tail -20 /tmp/network.log
else
    echo "No network log found"
fi
echo ""

# Troubleshooting hints
echo "=== Troubleshooting ==="
echo ""

if ! ip link show | grep -q "state UP"; then
    echo "! No interfaces are UP"
    echo "  - Check cable connection"
    echo "  - Try: ip link set eth0 up"
fi

if ! ip addr show | grep -q "inet "; then
    echo "! No IPv4 addresses assigned"
    echo "  - Check DHCP server is available"
    echo "  - Try: udhcpc -i eth0"
fi

if ! ip route show | grep -q "default"; then
    echo "! No default gateway"
    echo "  - DHCP may have failed"
    echo "  - Try manual: ip route add default via <gateway>"
fi

if ! ping -c 1 -W 2 1.1.1.1 >/dev/null 2>&1; then
    echo "! Cannot reach internet"
    echo "  - Check firewall/router settings"
    echo "  - Verify gateway is reachable"
fi

echo ""
echo "======================================"
echo "  Diagnostics complete"
echo "======================================"

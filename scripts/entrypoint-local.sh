#!/bin/sh
set -e

echo "[INIT] VPN Gateway (Local Network Mode)..."

# No WireGuard setup needed for local network
# WireGuard runs in separate container

echo "[INIT] Configuring iptables (relaxed mode)..."

# Flush existing rules
iptables -F 2>/dev/null || true
iptables -t nat -F 2>/dev/null || true
ip6tables -F 2>/dev/null || true

# Allow all output (no kill-switch for local mode)
iptables -P OUTPUT ACCEPT

# Allow loopback
iptables -A OUTPUT -o lo -j ACCEPT

# Allow established/related
iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT

# === IPv6: BLOCK ALL ===
ip6tables -P INPUT DROP 2>/dev/null || true
ip6tables -P OUTPUT DROP 2>/dev/null || true
ip6tables -P FORWARD DROP 2>/dev/null || true
ip6tables -A INPUT -i lo -j ACCEPT 2>/dev/null || true
ip6tables -A OUTPUT -o lo -j ACCEPT 2>/dev/null || true

echo "[INIT] iptables configured (local mode)"
echo "[INIT] Starting VPN Gateway..."

exec /usr/local/bin/vpn-gateway 2>&1

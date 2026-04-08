#!/bin/sh
set -e

echo "[INIT] Setting up WireGuard..."

# Bring up WireGuard interface if config exists and wg-quick is available
if [ -f /app/data/wg0.conf ] && command -v wg-quick >/dev/null 2>&1; then
    echo "[INIT] Found WireGuard config and wg-quick, checking interface..."
    if ! ip link show wg0 >/dev/null 2>&1; then
        echo "[INIT] Bringing up WireGuard interface..."
        wg-quick up /app/data/wg0.conf
    else
        echo "[INFO] WireGuard interface wg0 already exists, skipping setup"
    fi
elif [ -f /app/data/wg0.conf ]; then
    echo "[WARN] WireGuard config found but wg-quick not installed, skipping WG setup"
else
    echo "[INFO] No WireGuard config found, skipping WG setup"
fi

echo "[INIT] Configuring iptables..."

# Flush existing rules
iptables -F 2>/dev/null || true
iptables -t nat -F 2>/dev/null || true
ip6tables -F 2>/dev/null || true

# === KILL SWITCH ===
iptables -P OUTPUT DROP
iptables -P FORWARD ACCEPT

# Allow loopback
iptables -A OUTPUT -o lo -j ACCEPT

# Allow WireGuard interface (only if wg0 exists, but we add the rule anyway)
iptables -A OUTPUT -o wg0 -j ACCEPT

# Allow established/related (return traffic from upstream proxies)
iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT

# Allow outgoing TCP to upstream proxies (via eth0)
iptables -A OUTPUT -o eth0 -p tcp -m state --state NEW -j ACCEPT

# Allow HTTPS to proxy sources (port 443)
iptables -A OUTPUT -o eth0 -p tcp --dport 443 -j ACCEPT

# Allow DNS to specific resolvers
iptables -A OUTPUT -o eth0 -p udp --dport 53 -d 1.1.1.1 -j ACCEPT
iptables -A OUTPUT -o eth0 -p udp --dport 53 -d 8.8.8.8 -j ACCEPT

# === DNS REDIRECT ===
# All DNS from WG clients → our built-in DNS (port 5353)
iptables -t nat -A PREROUTING -i wg0 -p udp --dport 53 -j REDIRECT --to-port 5353

# === TCP TRANSPARENT PROXY ===
# All TCP from WG clients → our transparent proxy
iptables -t nat -A PREROUTING -i wg0 -p tcp -j REDIRECT --to-port 1080

# === UDP FORWARDING ===
# Non-DNS UDP goes directly through VPN (voice calls, etc.)
iptables -t nat -A POSTROUTING -o eth0 -p udp -j MASQUERADE

# === IPv6: BLOCK ALL ===
ip6tables -P INPUT DROP 2>/dev/null || true
ip6tables -P OUTPUT DROP 2>/dev/null || true
ip6tables -P FORWARD DROP 2>/dev/null || true
ip6tables -A INPUT -i lo -j ACCEPT 2>/dev/null || true
ip6tables -A OUTPUT -o lo -j ACCEPT 2>/dev/null || true

echo "[INIT] iptables configured"

# Debug: show current routes
echo "[DEBUG] Current routing table before adding default:"
ip route || true

# Add default route via eth0 if missing (for outbound internet access)
if ! ip route list default >/dev/null 2>&1; then
    echo "[INIT] Adding default route via eth0 (gateway 172.18.0.1)"
    ip route add default via 172.18.0.1 dev eth0 || true
else
    echo "[INFO] Default route already exists"
fi

echo "[INIT] Starting VPN Gateway..."

exec /usr/local/bin/vpn-gateway

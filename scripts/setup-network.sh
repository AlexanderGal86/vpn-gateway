#!/bin/sh
# Network setup for VPN Gateway (native/systemd mode)
# Manages WireGuard interface and iptables rules
set -e

DATA_DIR="/var/lib/vpn-gateway/data"
WG_CONF="$DATA_DIR/wg0.conf"

start() {
    echo "[vpn-gateway] Setting up network..."

    # Enable IP forwarding
    sysctl -q -w net.ipv4.ip_forward=1

    # Bring up WireGuard
    if [ -f "$WG_CONF" ]; then
        if ! ip link show wg0 >/dev/null 2>&1; then
            echo "[vpn-gateway] Bringing up WireGuard..."
            wg-quick up "$WG_CONF"
        fi
    else
        echo "[vpn-gateway] WARNING: WireGuard config not found at $WG_CONF"
        echo "[vpn-gateway] Run 'vpn-gateway-setup' to generate initial config"
        exit 1
    fi

    # iptables: VPN routing only (INPUT/OUTPUT untouched)
    echo "[vpn-gateway] Configuring iptables..."

    # Detect outbound interface
    OUTIF=$(ip route show default | awk '{print $5}' | head -1)
    OUTIF=${OUTIF:-eth0}

    iptables -P FORWARD DROP 2>/dev/null || true
    iptables -A FORWARD -i wg0 -o "$OUTIF" -j ACCEPT 2>/dev/null || true
    iptables -A FORWARD -i "$OUTIF" -o wg0 -m state --state ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || true

    # NAT for WireGuard
    iptables -t nat -A POSTROUTING -s 10.13.13.0/24 -o "$OUTIF" -j MASQUERADE 2>/dev/null || true

    # Redirect DNS from WireGuard clients to Unbound
    iptables -t nat -A PREROUTING -i wg0 -p udp --dport 53 -j REDIRECT --to-port 5353 2>/dev/null || true

    # Redirect TCP traffic from WireGuard clients to proxy
    iptables -t nat -A PREROUTING -i wg0 -p tcp -j REDIRECT --to-port 1080 2>/dev/null || true

    echo "[vpn-gateway] Network ready"
}

stop() {
    echo "[vpn-gateway] Cleaning up network..."

    OUTIF=$(ip route show default | awk '{print $5}' | head -1)
    OUTIF=${OUTIF:-eth0}

    # Remove iptables rules (best-effort)
    iptables -t nat -D PREROUTING -i wg0 -p tcp -j REDIRECT --to-port 1080 2>/dev/null || true
    iptables -t nat -D PREROUTING -i wg0 -p udp --dport 53 -j REDIRECT --to-port 5353 2>/dev/null || true
    iptables -t nat -D POSTROUTING -s 10.13.13.0/24 -o "$OUTIF" -j MASQUERADE 2>/dev/null || true
    iptables -D FORWARD -i "$OUTIF" -o wg0 -m state --state ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || true
    iptables -D FORWARD -i wg0 -o "$OUTIF" -j ACCEPT 2>/dev/null || true

    # Take down WireGuard
    if ip link show wg0 >/dev/null 2>&1; then
        wg-quick down "$WG_CONF" 2>/dev/null || true
    fi

    echo "[vpn-gateway] Network cleaned up"
}

case "$1" in
    start) start ;;
    stop)  stop  ;;
    *)     echo "Usage: $0 {start|stop}"; exit 1 ;;
esac

#!/bin/sh
set -e

echo "[INIT] ==============================================="
echo "[INIT] VPN Gateway - VPS Simple Mode"
echo "[INIT] ==============================================="

# === WireGuard Setup ===
echo "[INIT] Setting up WireGuard..."

# Generate WireGuard config if not exists
if [ ! -f /app/data/wg0.conf ]; then
    echo "[INIT] Generating WireGuard config..."
    mkdir -p /app/data/wg
    
    SERVER_URL=${WG_SERVER_URL:-auto}
    
    # If WG_SERVER_URL is "auto", try to detect external IP
    if [ "$SERVER_URL" = "auto" ]; then
        # Try to get the primary IP from eth0
        SERVER_URL=$(ip -4 addr show eth0 | grep -oP '(?<=inet\s)\d+(\.\d+){3}' | head -1)
        if [ -z "$SERVER_URL" ]; then
            SERVER_URL="127.0.0.1"
        fi
        echo "[INIT] Auto-detected server IP: $SERVER_URL"
    fi
    
    SERVER_PORT=${WG_PORT:-51820}
    INTERNAL_SUBNET=10.13.13.0/24
    PEER_COUNT=${WG_PEERS:-2}
    
    # Generate server config
    umask 077
    wg genkey > /app/data/wg/server.key
    wg pubkey < /app/data/wg/server.key > /app/data/wg/server.pub
    
    cat > /app/data/wg0.conf << EOF
[Interface]
Address = 10.13.13.1/24
ListenPort = ${SERVER_PORT}
PrivateKey = $(cat /app/data/wg/server.key)
# iptables rules are managed by entrypoint, not wg-quick

EOF

    # Generate peers
    for i in $(seq 1 $PEER_COUNT); do
        wg genkey > /app/data/wg/peer${i}.key
        wg pubkey < /app/data/wg/peer${i}.key > /app/data/wg/peer${i}.pub
        CLIENT_ADDR="10.13.13.$((i+1))"
        
        cat >> /app/data/wg0.conf << EOF
[Peer]
PublicKey = $(cat /app/data/wg/peer${i}.pub)
AllowedIPs = ${CLIENT_ADDR}/32
EOF
        
    # Create client config
    cat > /app/data/wg/peer${i}/peer${i}.conf << EOF
[Interface]
PrivateKey = $(cat /app/data/wg/peer${i}.key)
Address = ${CLIENT_ADDR}/24
DNS = 10.13.13.1

[Peer]
PublicKey = $(cat /app/data/wg/server.pub)
Endpoint = ${SERVER_URL}:${SERVER_PORT}
AllowedIPs = 0.0.0.0/0
PersistentKeepalive = 25
EOF

        # Generate QR code
        if command -v qrencode >/dev/null 2>&1; then
            qrencode -t png -o /app/data/wg/peer${i}/peer${i}-qr.png < /app/data/wg/peer${i}/peer${i}.conf
            echo "[INIT] QR code generated for peer${i}"
        fi
    done
    
    echo "[INIT] WireGuard config generated with ${PEER_COUNT} peers"
else
    echo "[INIT] Using existing WireGuard config"
fi

# Bring up WireGuard
if command -v wg-quick >/dev/null 2>&1; then
    if ! ip link show wg0 >/dev/null 2>&1; then
        echo "[INIT] Bringing up WireGuard interface..."
        wg-quick up /app/data/wg0.conf || true
    else
        echo "[INFO] WireGuard interface wg0 already up"
    fi
fi

# === Unbound DNS Setup ===
echo "[INIT] Setting up Unbound DNS..."

# Create minimal Unbound config
mkdir -p /app/data/unbound
if [ ! -f /app/data/unbound/unbound.conf ]; then
    cat > /app/data/unbound/unbound.conf << EOF
server:
    port: 5353
    interface: 0.0.0.0
    access-control: 10.13.13.0/24 allow
    access-control: 127.0.0.0/8 allow
    access-control: 0.0.0.0/0 allow
    hide-identity: yes
    hide-version: yes
    use-caps-for-id: yes
    prefetch: yes
    prefetch-key: yes
    minimal-responses: yes
    qname-minimisation: yes

remote-control:
    control-enable: no
EOF
    echo "[INIT] Unbound config created"
fi

# Start Unbound in background
if command -v unbound >/dev/null 2>&1; then
    if ! pgrep -x unbound >/dev/null; then
        echo "[INIT] Starting Unbound DNS server..."
        unbound -c /app/data/unbound/unbound.conf &
        sleep 2
    else
        echo "[INFO] Unbound already running"
    fi
else
    echo "[WARN] Unbound not found, using system DNS"
fi

# === iptables — VPN traffic routing only ===
# NOTE: INPUT/OUTPUT policies are NOT touched. This runs with network_mode: host
# on a hosted VPS — the hoster manages SSH, monitoring, and management ports.
# We only control FORWARD (VPN routing) and NAT (redirect + masquerade).
echo "[INIT] Configuring iptables (VPN routing only)..."

# Enable forwarding for VPN
echo 1 > /proc/sys/net/ipv4/ip_forward 2>/dev/null || true

# FORWARD: drop by default, allow only VPN traffic
iptables -P FORWARD DROP 2>/dev/null || true
iptables -A FORWARD -i wg0 -o eth0 -j ACCEPT 2>/dev/null || true
iptables -A FORWARD -i eth0 -o wg0 -m state --state ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || true

# NAT for WireGuard
iptables -t nat -A POSTROUTING -s 10.13.13.0/24 -o eth0 -j MASQUERADE 2>/dev/null || true

# Redirect DNS from WireGuard clients to Unbound (PREROUTING only)
iptables -t nat -A PREROUTING -i wg0 -p udp --dport 53 -j REDIRECT --to-port 5353 2>/dev/null || true

# Redirect TCP traffic from WireGuard clients to proxy (PREROUTING only)
iptables -t nat -A PREROUTING -i wg0 -p tcp -j REDIRECT --to-port 1080 2>/dev/null || true

echo "[INIT] iptables configured (VPN routing only, INPUT/OUTPUT untouched)"

# === Show network info ===
echo "[INIT] Network status:"
ip addr show wg0 2>/dev/null || echo "  wg0: not configured"
echo "  Route:"
ip route | head -5

# === Start VPN Gateway ===
echo "[INIT] ==============================================="
echo "[INIT] Starting VPN Gateway..."
echo "[INIT] ==============================================="

exec /usr/local/bin/vpn-gateway
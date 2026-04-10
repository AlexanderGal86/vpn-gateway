#!/bin/bash
# WireGuard Key Generation Script
# Usage: ./scripts/generate_wg_keys.sh [peer_name] [--peers N]
#
# Generates WireGuard keys and base configs (without Endpoint).
# Endpoint is injected by net-manager at runtime (LAN/WAN variants).
#
# Options:
#   peer_name    Name of the peer (default: peer1)
#   --peers N    Generate N peers at once
#   --no-config  Generate keys only, no config files

set -e

DATA_DIR="${DATA_DIR:-./data}"
WG_DIR="$DATA_DIR/wg"
PEER_NAME=""
PEERS_COUNT=1
NO_CONFIG=false

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --peers)
            PEERS_COUNT="$2"
            shift 2
            ;;
        --no-config)
            NO_CONFIG=true
            shift
            ;;
        *)
            PEER_NAME="$1"
            shift
            ;;
    esac
done

PEER_NAME="${PEER_NAME:-peer1}"

echo "=== WireGuard Key Generation ==="
echo "Data directory: $WG_DIR"
echo "Peers to generate: $PEERS_COUNT"
echo ""

# Create directories
mkdir -p "$WG_DIR/server"
mkdir -p "$WG_DIR/templates"

# Generate server keys (if not exist)
if [ ! -f "$WG_DIR/server/privatekey" ]; then
    echo "[1/3] Generating server keys..."
    wg genkey | tee "$WG_DIR/server/privatekey" | wg pubkey > "$WG_DIR/server/publickey"
    chmod 600 "$WG_DIR/server/privatekey"
    echo "   Server keys generated"
else
    echo "[1/3] Server keys already exist"
fi

SERVER_PUBLIC=$(cat "$WG_DIR/server/publickey")

# Generate peer configs
for i in $(seq 1 $PEERS_COUNT); do
    if [ "$PEERS_COUNT" -eq 1 ]; then
        PNAME="$PEER_NAME"
    else
        PNAME="peer${i}"
    fi

    PEER_DIR="$WG_DIR/$PNAME"
    mkdir -p "$PEER_DIR"

    echo "[2/3] Generating keys for '$PNAME'..."

    # Generate peer keys
    wg genkey | tee "$PEER_DIR/privatekey-${PNAME}" | wg pubkey > "$PEER_DIR/publickey-${PNAME}"
    chmod 600 "$PEER_DIR/privatekey-${PNAME}"

    PEER_PRIVATE=$(cat "$PEER_DIR/privatekey-${PNAME}")
    PEER_PUBLIC=$(cat "$PEER_DIR/publickey-${PNAME}")

    # Calculate peer IP
    PEER_IP=$((i + 1))
    PEER_ADDRESS="10.13.13.${PEER_IP}/32"

    # Generate base peer config (no Endpoint — net-manager adds it)
    if [ "$NO_CONFIG" = false ]; then
        cat > "$PEER_DIR/${PNAME}.conf" << EOF
[Interface]
PrivateKey = $PEER_PRIVATE
Address = $PEER_ADDRESS
DNS = 10.13.13.1

[Peer]
PublicKey = $SERVER_PUBLIC
AllowedIPs = 0.0.0.0/0, ::/0
PersistentKeepalive = 25
EOF

        # Generate preshared key for extra security
        wg genpsk > "$PEER_DIR/presharedkey-${PNAME}"
        PRESHARED=$(cat "$PEER_DIR/presharedkey-${PNAME}")

        # Update peer config with preshared key
        cat > "$PEER_DIR/${PNAME}.conf" << EOF
[Interface]
PrivateKey = $PEER_PRIVATE
Address = $PEER_ADDRESS
DNS = 10.13.13.1

[Peer]
PublicKey = $SERVER_PUBLIC
PresharedKey = $PRESHARED
AllowedIPs = 0.0.0.0/0, ::/0
PersistentKeepalive = 25
EOF
    fi

    echo "   Keys generated: $PNAME (IP: 10.13.13.$PEER_IP)"
done

# Generate server wg0.conf
echo "[3/3] Generating server config (wg0.conf)..."
SERVER_PRIVATE=$(cat "$WG_DIR/server/privatekey")

cat > "$WG_DIR/wg_confs/wg0.conf" << EOF
[Interface]
Address = 10.13.13.1/24
ListenPort = 51820
PrivateKey = $SERVER_PRIVATE
PostUp = iptables -t nat -A POSTROUTING -o eth0 -j MASQUERADE
PostDown = iptables -t nat -D POSTROUTING -o eth0 -j MASQUERADE
EOF

# Add all peers to server config
for i in $(seq 1 $PEERS_COUNT); do
    if [ "$PEERS_COUNT" -eq 1 ]; then
        PNAME="$PEER_NAME"
    else
        PNAME="peer${i}"
    fi
    PEER_DIR="$WG_DIR/$PNAME"
    PEER_PUBLIC=$(cat "$PEER_DIR/publickey-${PNAME}")
    PEER_IP=$((i + 1))
    PRESHARED=$(cat "$PEER_DIR/presharedkey-${PNAME}" 2>/dev/null || echo "")

    cat >> "$WG_DIR/wg_confs/wg0.conf" << EOF

# $PNAME
[Peer]
PublicKey = $PEER_PUBLIC
$(if [ -n "$PRESHARED" ]; then echo "PresharedKey = $PRESHARED"; fi)
AllowedIPs = 10.13.13.${PEER_IP}/32
EOF
done

mkdir -p "$WG_DIR/wg_confs" 2>/dev/null || true

echo ""
echo "=== Done! ==="
echo ""
echo "Server Public Key: $SERVER_PUBLIC"
echo ""
echo "Files created in $WG_DIR/:"
echo "  server/privatekey, server/publickey  — Server keys"
for i in $(seq 1 $PEERS_COUNT); do
    if [ "$PEERS_COUNT" -eq 1 ]; then
        PNAME="$PEER_NAME"
    else
        PNAME="peer${i}"
    fi
    echo "  $PNAME/  — Keys and base config (Endpoint added by net-manager)"
done
echo ""
echo "Next step: Run 'make docker-full-up' to start net-manager"
echo "Net-manager will generate LAN/WAN configs + QR codes automatically."

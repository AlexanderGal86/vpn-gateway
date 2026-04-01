#!/bin/bash
set -e

echo "=========================================="
echo "  VPN Gateway Rust - Installer"
echo "=========================================="

# Check root
if [ "$EUID" -ne 0 ]; then 
    echo "Please run as root (sudo)"
    exit 1
fi

# Install Docker
if ! command -v docker &> /dev/null; then
    echo "[*] Installing Docker..."
    curl -fsSL https://get.docker.com | sh
    usermod -aG docker $SUDO_USER
fi

# Install Docker Compose
if ! command -v docker-compose &> /dev/null; then
    echo "[*] Installing Docker Compose..."
    apt-get update
    apt-get install -y docker-compose-plugin
fi

# Enable IP forwarding
echo "[*] Enabling IP forwarding..."
sysctl -w net.ipv4.ip_forward=1
echo "net.ipv4.ip_forward=1" > /etc/sysctl.d/99-wireguard.conf
sysctl --system

# Create directories
echo "[*] Creating directories..."
mkdir -p ~/vpn-gateway/{gateway/src,config/unbound,data/wg,scripts}
cd ~/vpn-gateway

# Download compose file
echo "[*] Downloading configuration..."
cat > docker-compose.yml << 'EOF'
# (paste docker-compose.yml content here)
EOF

# Build and start
echo "[*] Building and starting..."
docker compose up -d --build

echo ""
echo "=========================================="
echo "  Installation complete!"
echo "=========================================="
echo ""
echo "Client config location:"
echo "  ~/vpn-gateway/data/wg/peer1/peer1.conf"
echo ""
echo "Commands:"
echo "  docker compose logs -f    # View logs"
echo "  docker compose ps         # Status"
echo "  docker compose down       # Stop"
echo ""
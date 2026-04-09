#!/bin/bash
set -e

REPO="alexandergal86/vpn-gateway"
GITHUB_URL="https://github.com/$REPO"

echo "=========================================="
echo "  VPN Gateway - Installer"
echo "=========================================="
echo ""

# Check root
if [ "$EUID" -ne 0 ]; then
    echo "Please run as root: sudo bash install.sh"
    exit 1
fi

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
    x86_64) ARCH_SUFFIX="amd64" ;;
    aarch64) ARCH_SUFFIX="arm64" ;;
    *)
        echo "ERROR: Unsupported architecture: $ARCH"
        exit 1
        ;;
esac

install_docker() {
    echo ""
    echo "[*] Installing via Docker..."
    echo ""

    # Install Docker if needed
    if ! command -v docker &> /dev/null; then
        echo "[*] Installing Docker..."
        curl -fsSL https://get.docker.com | sh
        if [ -n "$SUDO_USER" ]; then
            usermod -aG docker "$SUDO_USER"
        fi
    fi

    # Install Docker Compose plugin if needed
    if ! docker compose version &> /dev/null; then
        echo "[*] Installing Docker Compose plugin..."
        apt-get update -qq
        apt-get install -y -qq docker-compose-plugin
    fi

    # Enable IP forwarding
    echo "[*] Enabling IP forwarding..."
    sysctl -q -w net.ipv4.ip_forward=1
    echo "net.ipv4.ip_forward=1" > /etc/sysctl.d/99-vpn-gateway.conf

    # Create working directory
    INSTALL_DIR="/opt/vpn-gateway"
    mkdir -p "$INSTALL_DIR"/{config,data/wg}
    cd "$INSTALL_DIR"

    # Download docker-compose.yml
    echo "[*] Downloading docker-compose.yml..."
    curl -fsSL "$GITHUB_URL/raw/main/docker-compose.yml" -o docker-compose.yml

    # Download default config if not exists
    if [ ! -f config/gateway.json ]; then
        curl -fsSL "$GITHUB_URL/raw/main/config/gateway.json" -o config/gateway.json
    fi
    if [ ! -f config/sources.json ]; then
        curl -fsSL "$GITHUB_URL/raw/main/config/sources.json" -o config/sources.json
    fi

    # Start
    echo "[*] Starting VPN Gateway..."
    docker compose up -d

    echo ""
    echo "=========================================="
    echo "  Docker installation complete!"
    echo "=========================================="
    echo ""
    echo "  Install directory: $INSTALL_DIR"
    echo ""
    echo "  Commands:"
    echo "    cd $INSTALL_DIR"
    echo "    docker compose logs -f     # View logs"
    echo "    docker compose ps          # Status"
    echo "    docker compose down        # Stop"
    echo "    docker compose pull && docker compose up -d  # Update"
    echo ""
    echo "  Client config:"
    echo "    $INSTALL_DIR/data/wg/peer1/peer1.conf"
    echo ""
}

install_native() {
    echo ""
    echo "[*] Installing native package..."
    echo ""

    # Detect package manager
    if command -v dpkg &> /dev/null; then
        PKG_TYPE="deb"
    elif command -v rpm &> /dev/null; then
        PKG_TYPE="rpm"
    else
        echo "ERROR: Neither dpkg nor rpm found."
        echo "Use Docker installation instead (option 1)."
        exit 1
    fi

    echo "[*] Detected package type: $PKG_TYPE"

    # Detect latest release
    echo "[*] Fetching latest release..."
    LATEST=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')

    if [ -z "$LATEST" ]; then
        echo "ERROR: Could not determine latest release."
        echo "Check: $GITHUB_URL/releases"
        exit 1
    fi

    echo "[*] Latest version: $LATEST"

    # Download and install package
    if [ "$PKG_TYPE" = "deb" ]; then
        PKG_NAME="vpn-gateway_${LATEST#v}_${ARCH_SUFFIX}.deb"
        PKG_URL="$GITHUB_URL/releases/download/$LATEST/$PKG_NAME"
        TMP_PKG=$(mktemp /tmp/vpn-gateway-XXXXXX.deb)
    else
        PKG_NAME="vpn-gateway-${LATEST#v}-1.x86_64.rpm"
        PKG_URL="$GITHUB_URL/releases/download/$LATEST/$PKG_NAME"
        TMP_PKG=$(mktemp /tmp/vpn-gateway-XXXXXX.rpm)
    fi

    echo "[*] Downloading $PKG_NAME..."
    if ! curl -fsSL "$PKG_URL" -o "$TMP_PKG"; then
        echo "ERROR: Failed to download package from:"
        echo "  $PKG_URL"
        echo ""
        echo "Available releases: $GITHUB_URL/releases"
        rm -f "$TMP_PKG"
        exit 1
    fi

    echo "[*] Installing package..."
    if [ "$PKG_TYPE" = "deb" ]; then
        dpkg -i "$TMP_PKG" || apt-get install -f -y
    else
        dnf install -y "$TMP_PKG" 2>/dev/null || yum install -y "$TMP_PKG"
    fi
    rm -f "$TMP_PKG"

    echo ""
    echo "=========================================="
    echo "  Native installation complete!"
    echo "=========================================="
    echo ""
    echo "  Next steps:"
    echo "    1. sudo vpn-gateway-setup [server-ip] [peers]"
    echo "    2. sudo systemctl start vpn-gateway"
    echo "    3. sudo systemctl enable vpn-gateway"
    echo ""
    echo "  Management:"
    echo "    systemctl status vpn-gateway   # Status"
    echo "    journalctl -u vpn-gateway -f   # Logs"
    echo ""
}

# Choose installation mode
echo "Choose installation method:"
echo ""
echo "  1) Docker  - All-in-one container (recommended)"
echo "               Requires: Docker"
echo ""
echo "  2) Native  - .deb/.rpm package with systemd service"
echo "               Supports: Debian/Ubuntu, CentOS/Fedora/RHEL"
echo ""
printf "Select [1/2]: "
read MODE

case "$MODE" in
    2) install_native ;;
    *) install_docker ;;
esac

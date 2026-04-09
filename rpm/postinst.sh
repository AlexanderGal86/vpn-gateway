#!/bin/sh
# RPM post-install script

# Create data directory structure
mkdir -p /var/lib/vpn-gateway/data/wg
mkdir -p /var/lib/vpn-gateway/data/unbound

# Symlink config so the binary finds config/gateway.json
if [ ! -e /var/lib/vpn-gateway/config ]; then
    ln -s /etc/vpn-gateway /var/lib/vpn-gateway/config
fi

# Reload systemd
systemctl daemon-reload

echo ""
echo "=========================================="
echo "  VPN Gateway installed successfully!"
echo "=========================================="
echo ""
echo "Next steps:"
echo "  1. sudo vpn-gateway-setup [server-ip] [peer-count]"
echo "  2. sudo systemctl start vpn-gateway"
echo "  3. sudo systemctl enable vpn-gateway"
echo ""

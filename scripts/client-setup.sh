#!/bin/bash

# Generate QR code for mobile client
if command -v qrencode &> /dev/null; then
    echo "[*] Generating QR code for peer1..."
    qrencode -t ANSI256 < ~/vpn-gateway/data/wg/peer1/peer1.conf
else
    echo "Install qrencode for QR code generation"
fi

# Display config
echo ""
echo "=========================================="
echo "  WireGuard Client Configuration"
echo "=========================================="
echo ""
cat ~/vpn-gateway/data/wg/peer1/peer1.conf
echo ""
echo "=========================================="
echo ""
echo "Setup instructions:"
echo ""
echo "Linux:   sudo cp ~/vpn-gateway/data/wg/peer1/peer1.conf /etc/wireguard/wg0.conf"
echo "         sudo wg-quick up wg0"
echo ""
echo "Windows: Import peer1.conf in WireGuard app"
echo ""
echo "macOS:   brew install wireguard-tools"
echo "         sudo cp peer1.conf /usr/local/etc/wireguard/wg0.conf"
echo "         sudo wg-quick up wg0"
echo ""
echo "Mobile:  Scan QR code above or import file"
echo ""
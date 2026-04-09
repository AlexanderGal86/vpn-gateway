#!/bin/sh
# RPM pre-uninstall script

if systemctl is-active --quiet vpn-gateway; then
    systemctl stop vpn-gateway
fi
systemctl disable vpn-gateway 2>/dev/null || true

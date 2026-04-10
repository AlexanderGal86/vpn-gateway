#!/bin/sh

# Install script command for pseudo-TTY
apt-get update -qq && apt-get install -y -qq bsdutils >/dev/null 2>&1 || true

# Use script command to capture output with PTY
script -q -c "/usr/local/bin/vpn-gateway" /tmp/vpn.log &
PID=$!
echo "[DEBUG] Started with PID $PID"

sleep 10

echo "=== Output captured ==="
cat /tmp/vpn.log

if kill -0 $PID 2>/dev/null; then
    echo "[DEBUG] Process still running"
else
    echo "[DEBUG] Process exited"
fi

sleep infinity
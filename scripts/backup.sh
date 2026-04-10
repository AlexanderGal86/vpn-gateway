#!/bin/bash

BACKUP_DIR="$HOME/vpn-gateway-backups"
DATE=$(date +%Y%m%d_%H%M%S)
mkdir -p "$BACKUP_DIR"

echo "[*] Creating backup: $DATE"

# Backup state
cp ~/vpn-gateway/data/state.json "$BACKUP_DIR/state_$DATE.json" 2>/dev/null || true

# Backup WireGuard configs
tar czf "$BACKUP_DIR/wg_$DATE.tar.gz" ~/vpn-gateway/data/wg/ 2>/dev/null || true

# Backup config
cp ~/vpn-gateway/config/sources.json "$BACKUP_DIR/sources_$DATE.json" 2>/dev/null || true

# Cleanup old backups (keep 7 days)
find "$BACKUP_DIR" -name "*.json" -mtime +7 -delete
find "$BACKUP_DIR" -name "*.tar.gz" -mtime +7 -delete

echo "[*] Backup complete: $BACKUP_DIR"
ls -la "$BACKUP_DIR"
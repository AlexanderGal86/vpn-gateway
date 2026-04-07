#!/usr/bin/env bash
set -euo pipefail

MODE="${MODE:-${1:-home-vm}}"

if [[ ! -f .env ]]; then
  cp .env.example .env
  echo "[env-init] Created .env from .env.example"
else
  echo "[env-init] .env already exists, updating missing keys only"
fi

DEFAULT_IFACE="$(ip route 2>/dev/null | awk '/default/ {print $5; exit}')"

set_kv_if_missing() {
  local key="$1"
  local value="$2"
  if ! grep -qE "^${key}=" .env; then
    echo "${key}=${value}" >> .env
    echo "[env-init] set ${key}=${value}"
  fi
}

replace_or_add() {
  local key="$1"
  local value="$2"
  if grep -qE "^${key}=" .env; then
    sed -i "s|^${key}=.*|${key}=${value}|" .env
  else
    echo "${key}=${value}" >> .env
  fi
  echo "[env-init] set ${key}=${value}"
}

set_kv_if_missing "WG_PORT" "51820"
set_kv_if_missing "WG_PEERS" "2"
set_kv_if_missing "API_PORT" "8080"

case "$MODE" in
  vps)
    set_kv_if_missing "WG_SERVER_URL" "CHANGE_ME_VPS_PUBLIC_IP_OR_DNS"
    ;;
  home-vm)
    if [[ -n "$DEFAULT_IFACE" ]]; then
      replace_or_add "NET_INTERFACE" "$DEFAULT_IFACE"
    fi
    set_kv_if_missing "LAN_SUBNET" "192.168.1.0/24"
    set_kv_if_missing "LAN_GATEWAY" "192.168.1.1"
    set_kv_if_missing "MACVLAN_IP_RANGE" "192.168.1.200/29"
    set_kv_if_missing "DOCKER_HOST_IP" ""
    set_kv_if_missing "WG_SERVER_URL" "auto"
    ;;
  home-desktop)
    set_kv_if_missing "DOCKER_HOST_IP" "127.0.0.1"
    set_kv_if_missing "WG_SERVER_URL" "auto"
    ;;
  *)
    echo "[ERR] Unknown MODE='$MODE' (expected: vps|home-vm|home-desktop)"
    exit 1
    ;;
esac

echo "[env-init] Done for MODE=$MODE"
echo "[env-init] Next: MODE=$MODE ./scripts/preflight.sh"

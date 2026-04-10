#!/usr/bin/env bash
set -euo pipefail

if [[ ! -f .env ]]; then
  cp .env.example .env
  echo "[env-init] Created .env from .env.example"
else
  echo "[env-init] .env already exists, updating missing keys only"
fi

set_kv_if_missing() {
  local key="$1"
  local value="$2"
  if ! grep -qE "^${key}=" .env; then
    echo "${key}=${value}" >> .env
    echo "[env-init] set ${key}=${value}"
  fi
}

set_kv_if_missing "WG_PORT" "51820"
set_kv_if_missing "WG_PEERS" "2"
set_kv_if_missing "API_PORT" "8080"
set_kv_if_missing "WG_SERVER_URL" "CHANGE_ME_VPS_PUBLIC_IP_OR_DNS"

echo "[env-init] Done"
echo "[env-init] Next: ./scripts/preflight.sh"

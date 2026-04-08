#!/usr/bin/env bash
set -euo pipefail

if [[ ! -f .env && -f .env.example ]]; then
  cp .env.example .env
  echo "[preflight] .env missing -> copied from .env.example"
fi

if [[ -f .env ]]; then
  # shellcheck disable=SC1091
  set -a; source .env; set +a
fi

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "[ERR] Required command not found: $1"
    exit 1
  }
}

warn() { echo "[WARN] $*"; }
info() { echo "[INFO] $*"; }

need_cmd docker

if [[ -z "${WG_SERVER_URL:-}" || "${WG_SERVER_URL:-}" == "auto" ]]; then
  warn "WG_SERVER_URL is empty/auto. Set explicit public IP or DNS."
fi

check_port() {
  local p="$1"
  if command -v ss >/dev/null 2>&1; then
    if ss -lntu | awk '{print $5}' | grep -Eq "[:.]${p}$"; then
      warn "Port ${p} is already in use on host"
    fi
  fi
}

check_port "${WG_PORT:-51820}"
check_port "${API_PORT:-8080}"

info "Validating compose file: docker-compose.yml"
docker compose config >/dev/null

info "Preflight passed"

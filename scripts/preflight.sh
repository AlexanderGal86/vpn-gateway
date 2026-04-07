#!/usr/bin/env bash
set -euo pipefail

MODE="${MODE:-${1:-}}"
if [[ -z "$MODE" ]]; then
  echo "Usage: MODE=<vps|home-vm|home-desktop> $0"
  exit 1
fi

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

case "$MODE" in
  vps)
    COMPOSE_FILES=(-f docker-compose.yml)
    if [[ -z "${WG_SERVER_URL:-}" || "${WG_SERVER_URL:-}" == "auto" ]]; then
      warn "WG_SERVER_URL is empty/auto. For VPS set explicit public IP or DNS."
    fi
    ;;
  home-vm)
    COMPOSE_FILES=(-f docker-compose-local.yml)
    need_cmd ip
    : "${NET_INTERFACE:?NET_INTERFACE is required for home-vm}"
    : "${LAN_SUBNET:?LAN_SUBNET is required for home-vm}"
    : "${LAN_GATEWAY:?LAN_GATEWAY is required for home-vm}"
    : "${MACVLAN_IP_RANGE:?MACVLAN_IP_RANGE is required for home-vm}"

    if ! ip link show "${NET_INTERFACE}" >/dev/null 2>&1; then
      echo "[ERR] NET_INTERFACE='${NET_INTERFACE}' does not exist on host"
      exit 1
    fi
    ;;
  home-desktop)
    COMPOSE_FILES=(-f docker-compose-local.yml -f docker-compose-dev.yml)
    info "Docker Desktop mode: macvlan checks are skipped"
    ;;
  *)
    echo "[ERR] Unknown MODE='$MODE' (expected: vps|home-vm|home-desktop)"
    exit 1
    ;;
esac

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
if [[ "$MODE" != "vps" ]]; then
  check_port "8088"
fi

info "Validating compose files: ${COMPOSE_FILES[*]}"
docker compose "${COMPOSE_FILES[@]}" config >/dev/null

info "Preflight passed for MODE=$MODE"

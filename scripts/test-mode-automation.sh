#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

backup_env() {
  if [[ -f .env ]]; then
    cp .env .env.bak.ci
  fi
}

restore_env() {
  if [[ -f .env.bak.ci ]]; then
    mv .env.bak.ci .env
  else
    rm -f .env
  fi
}

backup_env
trap restore_env EXIT

bash -n scripts/env-init.sh
bash -n scripts/preflight.sh

MODE=vps ./scripts/env-init.sh
MODE=vps ./scripts/preflight.sh

MODE=home-vm ./scripts/env-init.sh
MODE=home-vm ./scripts/preflight.sh

MODE=home-desktop ./scripts/env-init.sh
MODE=home-desktop ./scripts/preflight.sh

make -n up MODE=vps >/dev/null
make -n up MODE=home-vm >/dev/null
make -n up MODE=home-desktop >/dev/null
make -n down MODE=vps >/dev/null
make -n status-all MODE=home-desktop >/dev/null

echo "Mode automation checks passed"

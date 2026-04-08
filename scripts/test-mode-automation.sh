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

if ! command -v docker >/dev/null 2>&1; then
  echo "[WARN] docker is not installed; skipping runtime mode automation checks"
  exit 0
fi

./scripts/env-init.sh
./scripts/preflight.sh

make -n docker-up >/dev/null
make -n docker-down >/dev/null

echo "Mode automation checks passed"

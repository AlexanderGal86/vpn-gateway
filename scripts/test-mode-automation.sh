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
  rm -rf .test-bin
}

setup_mocks() {
  mkdir -p .test-bin

  cat > .test-bin/docker <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  echo "Docker version 26.0.0, build mocked"
  exit 0
fi
if [[ "${1:-}" == "compose" && "${2:-}" == "version" ]]; then
  echo "Docker Compose version v2.0.0-mocked"
  exit 0
fi
if [[ "${1:-}" == "compose" ]]; then
  # Pretend compose config is valid in tests
  exit 0
fi
echo "mocked docker: unsupported args: $*" >&2
exit 1
SH

  cat > .test-bin/ip <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "route" ]]; then
  echo "default via 172.17.0.1 dev eth0"
  exit 0
fi
if [[ "${1:-}" == "link" && "${2:-}" == "show" ]]; then
  # Support `ip link show <iface>`
  echo "2: ${3:-eth0}: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500"
  exit 0
fi
echo "mocked ip: unsupported args: $*" >&2
exit 1
SH

  cat > .test-bin/ss <<'SH'
#!/usr/bin/env bash
set -euo pipefail
# No busy ports in mocked environment
exit 0
SH

  chmod +x .test-bin/docker .test-bin/ip .test-bin/ss
  export PATH="$ROOT_DIR/.test-bin:$PATH"
}

backup_env
trap restore_env EXIT
setup_mocks

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

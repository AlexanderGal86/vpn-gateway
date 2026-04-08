.PHONY: help build test run clean lint fmt fmt-fix check bench geoip-update geoip-update-dbip docker-up docker-down docker-logs docker-local-up docker-local-down docker-full-up docker-full-down docker-dev-up docker-dev-down status backup update client shell test-connection wg-keygen wg-show-configs env-init preflight up down status-all test-modes

help:
	@echo "VPN Gateway Makefile"
	@echo ""
	@echo "Core targets:"
	@echo "  build              - Build release binary"
	@echo "  test               - Run tests"
	@echo "  run                - Run locally"
	@echo "  clean              - Clean build artifacts"
	@echo "  lint               - Run clippy linter"
	@echo "  fmt                - Check code formatting"
	@echo "  fmt-fix            - Auto-fix formatting"
	@echo "  bench              - Run benchmarks (criterion)"
	@echo "  check              - Run lint + fmt + test"
	@echo ""
	@echo "Docker targets:"
	@echo "  docker-up          - Start Docker containers (VPS mode)"
	@echo "  docker-down        - Stop Docker containers (VPS mode)"
	@echo "  docker-local-up    - Start Docker containers (VM behind NAT mode)"
	@echo "  docker-local-down  - Stop Docker containers (VM behind NAT mode)"
	@echo "  docker-full-up     - Alias: start VM behind NAT stack (with net-manager + UPnP)"
	@echo "  docker-full-down   - Alias: stop VM behind NAT stack"
	@echo "  docker-dev-up      - Start VM behind NAT stack with Docker Desktop override"
	@echo "  docker-dev-down    - Stop dev stack"
	@echo "  docker-logs        - Show Docker logs"
	@echo ""
	@echo "  env-init           - Initialize/update .env for MODE (vps|home-vm|home-desktop)"
	@echo "  preflight          - Validate env/network/compose for MODE"
	@echo "  up                 - Unified startup for MODE"
	@echo "  down               - Unified shutdown for MODE"
	@echo "  status-all         - Health + metrics (+net-manager for home modes)"
	@echo "  test-modes         - Run mode automation checks (env-init/preflight)"
	@echo ""
	@echo "Utility targets:"
	@echo "  status             - Show container status and proxy count"
	@echo "  backup             - Backup state and configs"
	@echo "  update             - Pull latest and rebuild"
	@echo "  client             - Show WireGuard client QR code"
	@echo "  shell              - Open shell in gateway container"
	@echo "  test-connection    - Test proxy connection"
	@echo "  wg-keygen          - Generate WireGuard keys for new peers"
	@echo "  wg-show-configs    - Show generated client configs"
	@echo ""
	@echo "GeoIP targets:"
	@echo "  geoip-update       - Download GeoLite2-City (free, ~68MB)"
	@echo "  geoip-update-dbip  - Download DB-IP City Lite (compact, ~19MB)"

build:
	cargo build --release

test:
	cargo test

run:
	mkdir -p data
	RUST_LOG=vpn_gateway=debug cargo run

clean:
	cargo clean

lint:
	cargo clippy -- -D warnings

fmt:
	cargo fmt -- --check

fmt-fix:
	cargo fmt

bench:
	cargo bench --bench proxy_bench

check: lint fmt test

geoip-update:
	@echo "Downloading GeoLite2-City database..."
	@mkdir -p data
	@if command -v curl >/dev/null 2>&1; then \
		echo "Using curl..."; \
		curl -L -o data/GeoLite2-City.mmdb.gz \
			"https://cdn.jsdelivr.net/npm/geolite2-city/GeoLite2-City.mmdb.gz" \
			&& gzip -d data/GeoLite2-City.mmdb.gz \
			&& echo "Done! Saved to data/GeoLite2-City.mmdb"; \
	elif command -v wget >/dev/null 2>&1; then \
		echo "Using wget..."; \
		wget -O data/GeoLite2-City.mmdb.gz \
			"https://cdn.jsdelivr.net/npm/geolite2-city/GeoLite2-City.mmdb.gz" \
			&& gzip -d data/GeoLite2-City.mmdb.gz \
			&& echo "Done! Saved to data/GeoLite2-City.mmdb"; \
	else \
		echo "Error: curl or wget required"; \
	fi

geoip-update-dbip:
	@echo "Downloading DB-IP City Lite database..."
	@mkdir -p data
	@if command -v curl >/dev/null 2>&1; then \
		echo "Using curl..."; \
		curl -L -o data/dbip-city-lite.mmdb.gz \
			"https://cdn.jsdelivr.net/npm/dbip-city-lite/dbip-city-lite.mmdb.gz" \
			&& gzip -d data/dbip-city-lite.mmdb.gz \
			&& echo "Done! Saved to data/dbip-city-lite.mmdb"; \
	elif command -v wget >/dev/null 2>&1; then \
		echo "Using wget..."; \
		wget -O data/dbip-city-lite.mmdb.gz \
			"https://cdn.jsdelivr.net/npm/dbip-city-lite/dbip-city-lite.mmdb.gz" \
			&& gzip -d data/dbip-city-lite.mmdb.gz \
			&& echo "Done! Saved to data/dbip-city-lite.mmdb"; \
	else \
		echo "Error: curl or wget required"; \
	fi

docker-up:
	docker compose -f docker-compose.yml up -d --build
	@echo "Waiting for services..."
	@sleep 5
	@echo "=== Service Status (VPS mode) ==="
	@docker compose -f docker-compose.yml ps
	@echo ""
	@echo "=== Gateway Health ==="
	@curl -s http://localhost:8080/health 2>/dev/null || echo "Gateway API not yet ready"

docker-down:
	docker compose -f docker-compose.yml down

docker-local-up:
	docker compose -f docker-compose-local.yml up -d --build
	@echo "Waiting for services..."
	@sleep 5
	@echo "=== Service Status (VM behind NAT mode) ==="
	@docker compose -f docker-compose-local.yml ps
	@echo ""
	@echo "=== net-manager Status ==="
	@curl -s http://localhost:8088/status 2>/dev/null || echo "net-manager API not yet ready"
	@echo ""
	@echo "=== Gateway Health ==="
	@curl -s http://localhost:8080/health 2>/dev/null || echo "Gateway API not yet ready"

docker-local-down:
	docker compose -f docker-compose-local.yml down

docker-full-up:
	@$(MAKE) docker-local-up

docker-full-down:
	docker compose -f docker-compose-local.yml down

docker-dev-up:
	docker compose -f docker-compose-local.yml -f docker-compose-dev.yml up -d --build
	@echo "Waiting for services..."
	@sleep 5
	@docker compose -f docker-compose-local.yml -f docker-compose-dev.yml ps

docker-dev-down:
	docker compose -f docker-compose-local.yml -f docker-compose-dev.yml down

docker-logs:
	docker compose logs -f

# Unified mode commands (MODE=vps|home-vm|home-desktop)
env-init:
	@MODE=$${MODE:-home-vm} ./scripts/env-init.sh

preflight:
	@MODE=$${MODE:-home-vm} ./scripts/preflight.sh

up: preflight
	@if [ "$${MODE:-home-vm}" = "vps" ]; then \
		$(MAKE) docker-up; \
	elif [ "$${MODE:-home-vm}" = "home-vm" ]; then \
		$(MAKE) docker-local-up; \
	elif [ "$${MODE:-home-vm}" = "home-desktop" ]; then \
		$(MAKE) docker-dev-up; \
	else \
		echo "Unknown MODE=$${MODE:-home-vm}. Use vps|home-vm|home-desktop"; exit 1; \
	fi

down:
	@if [ "$${MODE:-home-vm}" = "vps" ]; then \
		$(MAKE) docker-down; \
	elif [ "$${MODE:-home-vm}" = "home-vm" ]; then \
		$(MAKE) docker-local-down; \
	elif [ "$${MODE:-home-vm}" = "home-desktop" ]; then \
		$(MAKE) docker-dev-down; \
	else \
		echo "Unknown MODE=$${MODE:-home-vm}. Use vps|home-vm|home-desktop"; exit 1; \
	fi

status-all:
	@echo "=== Gateway Health ==="
	@curl -s http://localhost:8080/health 2>/dev/null || echo "Gateway API unavailable"
	@echo ""
	@echo "=== Proxy Metrics ==="
	@curl -s http://localhost:8080/api/metrics 2>/dev/null || echo "Metrics unavailable"
	@if [ "$${MODE:-home-vm}" != "vps" ]; then \
		echo ""; \
		echo "=== net-manager Status ==="; \
		curl -s http://localhost:8088/status 2>/dev/null || echo "net-manager unavailable"; \
	fi

test-modes:
	@./scripts/test-mode-automation.sh

status:
	docker compose ps
	@echo ""
	@echo "Proxy count:"
	@curl -s http://localhost:8080/api/metrics 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin).get('total_proxies','N/A'))" 2>/dev/null || echo "API unavailable"

backup:
	@bash scripts/backup.sh

update:
	git pull 2>/dev/null || true
	docker compose pull
	docker compose up -d --build

client:
	@bash scripts/client-setup.sh

shell:
	docker compose exec vpn-gateway sh

test-connection:
	@echo "Testing connection through proxy..."
	@curl --socks5 localhost:1080 http://ifconfig.me 2>/dev/null || echo "Test failed"

wg-keygen:
	@bash scripts/generate_wg_keys.sh

wg-show-configs:
	@echo "=== Generated Client Configs ==="
	@find data/clients -name "*.conf" -type f 2>/dev/null | sort | while read f; do \
		echo "--- $$f ---"; \
		cat "$$f"; \
		echo ""; \
	done
	@echo ""
	@echo "=== QR Codes ==="
	@find data/clients -name "*.png" -type f 2>/dev/null | sort

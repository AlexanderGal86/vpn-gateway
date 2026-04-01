.PHONY: help build test run clean geoip-update geoip-update-dbip docker-up docker-down docker-logs docker-local-up docker-local-down docker-full-up docker-full-down docker-dev-up docker-dev-down status backup update client shell test-connection wg-keygen wg-show-configs

help:
	@echo "VPN Gateway Makefile"
	@echo ""
	@echo "Core targets:"
	@echo "  build              - Build release binary"
	@echo "  test               - Run tests"
	@echo "  run                - Run locally"
	@echo "  clean              - Clean build artifacts"
	@echo ""
	@echo "Docker targets:"
	@echo "  docker-up          - Start Docker containers (VPS mode)"
	@echo "  docker-down        - Stop Docker containers (VPS mode)"
	@echo "  docker-local-up    - Start Docker containers (Local network mode)"
	@echo "  docker-local-down  - Stop Docker containers (Local network mode)"
	@echo "  docker-full-up     - Start full stack (VPS + net-manager + UPnP)"
	@echo "  docker-full-down   - Stop full stack"
	@echo "  docker-dev-up      - Start full stack (dev mode, no macvlan)"
	@echo "  docker-dev-down    - Stop dev stack"
	@echo "  docker-logs        - Show Docker logs"
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
	docker compose up -d --build
	@echo "Waiting for services..."
	@sleep 5
	@docker compose logs -f

docker-down:
	docker compose down

docker-local-up:
	docker compose -f docker-compose-local.yml up -d --build
	@echo "Waiting for services..."
	@sleep 5
	@docker compose -f docker-compose-local.yml logs -f

docker-local-down:
	docker compose -f docker-compose-local.yml down

docker-full-up:
	docker compose -f docker-compose.yml up -d --build
	@echo "Waiting for services..."
	@sleep 5
	@echo "=== Service Status ==="
	@docker compose ps
	@echo ""
	@echo "=== net-manager Status ==="
	@curl -s http://localhost:8088/status 2>/dev/null || echo "net-manager API not yet ready"
	@echo ""
	@echo "=== Gateway Health ==="
	@curl -s http://localhost:8080/health 2>/dev/null || echo "Gateway API not yet ready"

docker-full-down:
	docker compose -f docker-compose.yml down

docker-dev-up:
	docker compose -f docker-compose.yml -f docker-compose-dev.yml up -d --build
	@echo "Waiting for services..."
	@sleep 5
	@docker compose -f docker-compose.yml -f docker-compose-dev.yml ps

docker-dev-down:
	docker compose -f docker-compose.yml -f docker-compose-dev.yml down

docker-logs:
	docker compose logs -f

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

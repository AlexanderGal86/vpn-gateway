.PHONY: help build test run clean lint fmt fmt-fix check bench docker-up docker-down docker-logs status backup update client shell test-connection wg-keygen wg-show-configs

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
	@echo "  docker-up          - Start Docker container"
	@echo "  docker-down        - Stop Docker container"
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

docker-up:
	docker compose up -d
	@echo "Waiting for services..."
	@sleep 5
	@echo "=== Service Status ==="
	@docker compose ps

docker-down:
	docker compose down

docker-logs:
	docker compose logs -f

status:
	docker compose ps
	@echo ""
	@echo "Proxy count:"
	@curl -s http://10.13.13.1:8080/api/metrics 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin).get('total_proxies','N/A'))" 2>/dev/null || echo "API unavailable"

backup:
	@bash scripts/backup.sh

update:
	docker compose pull
	docker compose up -d

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

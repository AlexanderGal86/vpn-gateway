# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
make build              # cargo build --release
make test               # cargo test
make lint               # cargo clippy -- -D warnings
make fmt                # cargo fmt -- --check
make fmt-fix            # cargo fmt (auto-fix)
make check              # lint + fmt + test (full CI check)
make run                # RUST_LOG=vpn_gateway=debug cargo run (creates data/ dir)
make clean              # cargo clean
make bench              # cargo bench --bench proxy_bench
```

### Docker Commands

```bash
make docker-up          # Start Docker container (docker-compose.yml)
make docker-down        # Stop Docker container
make docker-logs        # Show Docker logs
```

### Utility Commands

```bash
make status             # Show container status and proxy count
make client             # Show WireGuard client QR code
make shell              # Open shell in gateway container
make test-connection    # Test proxy connection via SOCKS5
make wg-keygen          # Generate WireGuard keys for new peers
make backup             # Backup state and configs
```

Run a single test: `cargo test <test_name>`

Config path override: `CONFIG_PATH=path/to/config.json cargo run`

## CI/CD (GitHub Actions)

GitHub Actions runs `make check` (clippy + fmt + test) automatically on every push to `main` and on PRs. **Before running tests locally, check if CI already ran them** — this saves time and tokens.

- **CI workflow** (`.github/workflows/ci.yml`): fmt → clippy → test → bench compile check → build+push Docker image to `ghcr.io/alexandergal86/vpn-gateway:latest` (push-to-main only, gated on tests passing)
- **Release workflow** (`.github/workflows/release.yml`): triggered by `v*` tags, builds Linux binary and pushes versioned Docker image (`:X.Y.Z`, `:X.Y`, `:latest`) to `ghcr.io`
- **Dependabot**: weekly PRs for Cargo, pip, and GitHub Actions dependency updates

To create a release: `git tag v1.1.0 && git push --tags`

Distribution is Docker-only. End users run `docker compose up -d` with a `docker-compose.yml` that pulls from `ghcr.io` — no local build, no .deb/.rpm packages.

## Architecture

Rust (tokio async) transparent TCP/UDP proxy gateway that routes traffic through a dynamic pool of free proxy servers. Runs on Linux with iptables redirecting WireGuard client traffic to the gateway.

### Deployment

Single-container deployment with `network_mode: host`:
- `docker-compose.yml` — single container, all-in-one
- WireGuard + Unbound DNS + Gateway + iptables in one process
- Entrypoint: `scripts/entrypoint-simple.sh`
- API bound to `10.13.13.1` (WireGuard interface only)

### Startup Sequence (4-level fast-start in `src/main.rs`)

1. **Level 0 (instant)**: Load persisted proxy state from `data/state.json`
2. **Level 1 (fast)**: Bootstrap from top 3 sources, fast-probe all proxies (3s timeout). TCP proxy starts accepting connections immediately (waits on `first_ready` if pool empty)
3. **Level 2 (background)**: Full refresh from all sources in `config/sources.json`, then periodic refresh loop
4. **Level 3 (continuous)**: Health check loop + state persistence loop (every 300s)

### Module Structure

- **`src/pool/`** — Proxy pool management
  - `state.rs` — `SharedState` (DashMap-based), banned list, geo-index, `exclude_countries` filter, `with_config()` constructor
  - `proxy.rs` — Proxy entry with EWMA latency scoring and circuit breaker
  - `source_manager.rs` — Fetches proxies from 27 hardcoded sources + `config/sources.json`
  - `health_checker.rs` — 3-stage health check (TCP + CONNECT + TLS validation) with MITM detection
  - `persistence.rs` — Save/load `data/state.json`
  - `sticky_sessions.rs` — Client IP to proxy affinity
  - `connection_pool.rs` — Optional TCP connection reuse
  - `geo_ip.rs` — GeoIP via external API with DashMap cache
  - `metrics.rs` — Prometheus-format metrics with TYPE/HELP headers

- **`src/proxy/`** — Traffic handling
  - `transparent.rs` — SO_ORIGINAL_DST + TCP relay (the main proxy listener on :1080)
  - `upstream.rs` — HTTP CONNECT and SOCKS5 upstream proxy protocols
  - `sniff.rs` — TLS SNI extraction from ClientHello
  - `udp.rs` — UDP relay on :1081, configurable DNS upstream

- **`src/api/web.rs`** — Axum 0.8 HTTP API on :8080 (/health, /metrics, /api/proxies, ban/unban), bound to `api_bind` (default `10.13.13.1`)
- **`src/config.rs`** — JSON config with hot-reload support, `ConfigManager` with Notify-based shutdown

### Scripts

| Script | Purpose |
|--------|---------|
| `entrypoint-simple.sh` | Docker entrypoint: WG + Unbound + iptables + Gateway |
| `client-setup.sh` | WireGuard client config helper |
| `backup.sh` | Backup state and configs |
| `generate_wg_keys.sh` | Generate WireGuard key pairs |

### Key Design Patterns

- All state flows through `SharedState` (clone of Arc-wrapped DashMap collections)
- Proxy selection uses EWMA-weighted latency with circuit breaker pattern + `exclude_countries` filtering
- **Two-tier proxy pool**: proxies marked `tls_clean` (true/false/unknown) — HTTPS traffic (port 443) uses only TLS-clean proxies, HTTP uses any
- **MITM detection**: 3-stage health check validates TLS certificates through proxy tunnel using rustls + webpki-roots
- Single-pass top-N selection (`collect_top_n`) — O(n) scan, O(10) memory per selection
- Bounded concurrency: TCP connections (Semaphore from config `max_connections`), UDP tasks (Semaphore, 1000), GeoIP lookups (Semaphore, 20)
- Pool size enforced via `max_proxies` config (AtomicUsize in SharedState)
- Per-source proxy cap (500) prevents rogue source flooding
- Sticky session TTL uses AtomicU64 (lock-free reads)
- Connection pool: liveness check runs outside mutex to avoid blocking
- EWMA latency clamped to [0, 60000] with NaN/infinity guard
- Config lives in `config/gateway.json`, proxy sources in `config/sources.json`
- Persistent state in `data/state.json` (auto-saved periodically)
- Target platform is Linux (uses `nix` crate for SO_ORIGINAL_DST, iptables for traffic redirection)
- iptables: only FORWARD + NAT rules (INPUT/OUTPUT untouched — VPS hoster manages those)

### Configuration

`config/gateway.json`:
```json
{
  "gateway_port": 1080,
  "api_port": 8080,
  "api_bind": "10.13.13.1",
  "udp_port": 1081,
  "max_proxies": 10000,
  "max_connections": 10000,
  "health_check_interval": 30,
  "source_update_interval": 300,
  "exclude_countries": ["RU"],
  "dns_upstream": "10.13.13.1:53"
}
```

### Default Ports

| Port | Service |
|------|---------|
| 1080 | Transparent TCP proxy |
| 1081 | UDP relay |
| 5353 | Unbound DNS |
| 8080 | Gateway API/metrics (bound to 10.13.13.1) |
| 51820/udp | WireGuard |

### Docker

Single compose file: `docker-compose.yml` — single container with `network_mode: host`. It pulls a prebuilt image from `ghcr.io/alexandergal86/vpn-gateway:latest` (built by CI on every push to main). End-user install is just:

```bash
curl -O https://raw.githubusercontent.com/AlexanderGal86/vpn-gateway/main/docker-compose.yml
docker compose up -d
```

For development: `make docker-up` / `make docker-down` (same flow, uses the pulled image).

### Data Directory

```
data/
├── wg/              # WireGuard keys and configs
├── wg0.conf         # WireGuard server config
├── unbound/         # Unbound DNS config
└── state.json       # Proxy pool state (gateway)
```

> For full deployment history, see `DEVELOPMENT_HISTORY.md`.

### Future Work

#### Tests
- [x] Integration tests for persistence module (requires temp directory setup)
- [x] Integration tests for transparent proxy (requires TCP listener + SO_ORIGINAL_DST)
- [x] Integration tests for upstream handshake (requires mock HTTP/SOCKS5 proxy)
- [x] Benchmarks: `cargo bench` for `collect_top_n()`, EWMA scoring, and hot paths

#### Features
- [x] Country exclusion filter (`exclude_countries` in config + `select_best()` filtering)
- [x] Single-container deployment (WireGuard + Unbound + Gateway)
- [x] MITM detection — 3-stage TLS validation, two-tier proxy pool (`tls_clean`)
- [x] API bound to WireGuard interface (`10.13.13.1`) — no auth needed
- [ ] IPv6 SO_ORIGINAL_DST support (`sockaddr_in6` in `transparent.rs`)
- [ ] `preferred_countries` implementation (only `exclude_countries` is done)

#### Technical Debt
- [x] Config watcher proper lifetime management (replace 60s sleep loop in `config.rs`)
- [x] Hardcoded DNS upstream `10.13.13.1:53` in `udp.rs` (move to config)
- [x] Removed broken multi-container architecture (iptables conflicts)

#### Enhancements
- [x] Geo-index for O(1) country-based proxy selection (`HashMap<country, Vec<proxy_key>>` in `state.rs`)
- [x] GeoIP lookup cache (`DashMap` in `geo_ip.rs`) to eliminate redundant API requests
- [x] Prometheus typed metrics — add `# TYPE` / `# HELP` headers to `/metrics` output in `metrics.rs`
- [x] Expanded proxy sources from 11 to 27 for better pool coverage

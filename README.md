# VPN Gateway

Transparent TCP/UDP proxy gateway with a dynamic pool of free proxy servers, WireGuard VPN tunneling, and DNS leak protection. Built in Rust for high performance and reliability.

## Overview

VPN Gateway automatically discovers, validates, and rotates through 1000+ free proxy servers from 27 public sources. TCP traffic from WireGuard clients is transparently proxied through the best-performing servers, selected via EWMA latency scoring and circuit breaker patterns. UDP traffic (including DNS) is relayed through a dedicated channel with Unbound DNS resolver for leak prevention.

MITM proxies (common in free proxy pools) are automatically detected via TLS certificate validation and marked — HTTPS traffic is routed only through TLS-clean proxies, while HTTP traffic uses any available proxy to maximize pool utilization.

### Key Features

- **Transparent proxying** — iptables REDIRECT + `SO_ORIGINAL_DST` for zero-config client setup
- **Smart proxy selection** — EWMA-weighted latency scoring with Top-N random selection
- **Circuit breaker** — Escalating backoff (60s → 300s → 3600s → permanent ban) for failing proxies
- **MITM detection** — 3-stage TLS validation (TCP + CONNECT + certificate check via rustls)
- **Two-tier proxy pool** — TLS-clean proxies for HTTPS, any proxy for HTTP (pool never empties)
- **4-level fast startup** — Instant state restore → fast bootstrap → full refresh → continuous health checks
- **Protocol support** — HTTP CONNECT and SOCKS5 upstream, TLS SNI extraction for routing
- **Connection pooling** — Optional TCP connection reuse to reduce handshake overhead
- **Sticky sessions** — TTL-based client-to-proxy affinity
- **GeoIP filtering** — Country-based proxy selection via API or local MaxMind/DB-IP database
- **Hot-reload config** — File watcher with automatic config reload (no restart needed)
- **Prometheus metrics** — Native `/metrics` endpoint for monitoring
- **WireGuard integration** — Auto-configured VPN with QR code client provisioning
- **UPnP port forwarding** — Automatic router configuration for remote access
- **Per-source caps** — Max 500 proxies per source to prevent pool flooding
- **Bounded concurrency** — Semaphore-based limits on connections, UDP tasks, and GeoIP lookups

---

## Architecture

### Docker Service Topology

Single container with `network_mode: host`:

```
  ┌────────────────────────────────────────────────────────────────┐
  │                    VPS Host (network_mode: host)               │
  │                                                                │
  │  Internet ◄──── :51820/udp (WireGuard)                        │
  │                       │                                        │
  │                  wg0: 10.13.13.0/24                            │
  │                       │                                        │
  │            iptables PREROUTING:                                 │
  │              TCP → :1080 (proxy)                               │
  │              UDP:53 → :5353 (DNS)                              │
  │            FORWARD: wg0↔eth0 only                              │
  │                       │                                        │
  │  ┌────────────────────┼────────────────────────────────────┐   │
  │  │  vpn-gateway (Rust, single container)                   │   │
  │  │  ├── :1080   Transparent TCP Proxy                      │   │
  │  │  ├── :1081   UDP Relay                                  │   │
  │  │  ├── :5353   Unbound DNS                                │   │
  │  │  └── :8080   REST API (bound to 10.13.13.1)             │   │
  │  └─────────────────────────────────────────────────────────┘   │
  └────────────────────────────────────────────────────────────────┘
```

### Traffic Flow

```
  WireGuard Client                VPN Gateway                    Internet
  ──────────────                  ───────────                    ────────
        │                              │                             │
        │  TCP connection              │                             │
        ├─────────────────────────────►│                             │
        │  (iptables REDIRECT :1080)   │                             │
        │                              │  1. SO_ORIGINAL_DST         │
        │                              │  2. TLS SNI extraction      │
        │                              │  3. Sticky session lookup   │
        │                              │  4. Select best proxy       │
        │                              │     (EWMA + Top-N)          │
        │                              │  5. HTTP CONNECT / SOCKS5   │
        │                              ├────────────────────────────►│
        │                              │  6. Bidirectional relay     │
        │◄─────────────────────────────┤◄────────────────────────────│
        │                              │                             │
        │  UDP / DNS                   │                             │
        ├─────────────────────────────►│                             │
        │  (iptables REDIRECT :1081)   │  Direct relay + Unbound    │
        │◄─────────────────────────────┤◄────────────────────────────│
```

### Proxy Lifecycle

```
  ┌──────────────┐     ┌──────────────┐     ┌──────────────┐     ┌──────────────┐
  │  Discovery   │────►│  Validation  │────►│   Active     │────►│   Retired    │
  │              │     │  (3 stages)  │     │              │     │              │
  │ 27 public    │     │ 1. TCP conn  │     │ EWMA scoring │     │ Circuit      │
  │ source lists │     │ 2. CONNECT   │     │ Top-N select │     │ breaker      │
  │ + custom     │     │ 3. TLS cert  │     │ Health check │     │ escalation   │
  │ sources.json │     │    check     │     │ loop (30s)   │     │ or stale     │
  └──────────────┘     └──────────────┘     └──────────────┘     └──────────────┘
         │                    │                    │                     │
         │               tls_clean?                │                     │
         │              ┌────┴────┐                │                     │
         │              │true│false│                │                     │
         │              └─┬──┴──┬─┘                │                     │
         │         HTTPS ok  HTTP only             │                     │
         │              ┌──────────────┐           │                     │
         │              │  Persisted   │◄──────────┘                     │
         │              │  state.json  │           (every 300s)          │
         │              │  (Level 0    │                                 │
         └──────────────│   restore)   │─────────── banned list ────────┘
                        └──────────────┘
```

---

## Technology Stack

### Core (Rust)

| Crate | Version | Purpose |
|-------|---------|---------|
| **tokio** | 1.x | Async runtime (full features: rt-multi-thread, io, net, time, sync, macros) |
| **axum** | 0.7 | HTTP API framework (REST endpoints, JSON, routing) |
| **dashmap** | 6.x | Lock-free concurrent HashMap (proxy pool state) |
| **reqwest** | 0.12 | HTTP client with rustls-tls (source fetching, health checks) |
| **serde** / **serde_json** | 1.x | Serialization/deserialization (config, state, API) |
| **tokio::sync** | — | Semaphore, RwLock, Notify, mpsc channels |
| **futures** | 0.3 | FuturesUnordered for concurrent source fetching |
| **bytes** | 1.x | Efficient byte buffer manipulation |

### Networking & System

| Crate / Tech | Purpose |
|--------------|---------|
| **nix** 0.29 | `SO_ORIGINAL_DST` socket option (transparent proxy) |
| **libc** 0.2 | Low-level `getsockopt` for original destination extraction |
| **tower-http** 0.6 | CORS middleware for API |
| **iptables** | Traffic redirection (REDIRECT target for TCP/UDP) |

### Observability & Reliability

| Crate | Purpose |
|-------|---------|
| **tracing** 0.1 | Structured logging (spans, events, levels) |
| **tracing-subscriber** 0.3 | Log formatting with env-filter (`RUST_LOG`) |
| **chrono** 0.4 | Timestamps for proxy scoring and session TTL |
| **anyhow** 1.x | Ergonomic error handling (application-level) |
| **thiserror** 2.x | Derive-based error types (library-level) |

### Configuration & State

| Crate | Purpose |
|-------|---------|
| **notify** 6.x | Filesystem watcher for config hot-reload |
| **parking_lot** 0.12 | Faster Mutex/RwLock for connection pool |
| **rand** 0.8 | Weighted random proxy selection |

### Build & Optimization

| Setting | Value | Purpose |
|---------|-------|---------|
| `opt-level` | 3 | Maximum optimization |
| `lto` | fat | Link-time optimization (cross-crate inlining) |
| `codegen-units` | 1 | Single codegen unit for better optimization |
| `panic` | abort | No unwinding overhead |
| `strip` | true | Remove debug symbols from release binary |

### Python (net-manager sidecar)

| Package | Version | Purpose |
|---------|---------|---------|
| **Flask** | 3.1 | HTTP server for config distribution (:8088) |
| **miniupnpc** | 2.2 | UPnP IGD port forwarding to router |
| **qrcode[pil]** | 8.x | QR code generation for WireGuard client configs |
| **Jinja2** | 3.1 | Template rendering for WireGuard configs |

### Infrastructure

| Technology | Purpose |
|------------|---------|
| **Docker** 24.0+ | Containerization |
| **Docker Compose** v2 | Multi-service orchestration |
| **WireGuard** | VPN tunnel (linuxserver/wireguard image) |
| **Unbound** | Recursive DNS resolver (DNS leak prevention) |
| **macvlan** | Docker network driver for LAN IP acquisition |
| **bridge** | Internal Docker network (172.20.0.0/24) |

### Protocols

| Protocol | Usage |
|----------|-------|
| **HTTP CONNECT** | Upstream proxy tunneling (RFC 7231) |
| **SOCKS5** | Upstream proxy tunneling (RFC 1928) |
| **TLS 1.2/1.3** | SNI extraction from ClientHello for routing |
| **WireGuard** | VPN tunnel (Noise protocol framework) |
| **DNS over UDP** | Unbound recursive resolution |
| **UPnP IGD** | Automatic router port forwarding |
| **DHCP** | net-manager LAN IP acquisition |

---

## Quick Start

### Prerequisites

- **Docker** 24.0+ with Docker Compose v2
- **Linux host** (iptables required for transparent proxying)
- **Open port**: UDP 51820 (WireGuard)

### Install (3 lines)

```bash
mkdir vpn-gateway && cd vpn-gateway
curl -O https://raw.githubusercontent.com/AlexanderGal86/vpn-gateway/main/docker-compose.yml
docker compose up -d
```

That's it — the prebuilt image is pulled from `ghcr.io/alexandergal86/vpn-gateway:latest`. No local build, no cloning.

### WireGuard client configs

After the first start, peer configs are generated in `./data/wg/`:

```bash
cat ./data/wg/peer1/peer1.conf        # config file
ls  ./data/wg/peer1/peer1-qr.png      # QR code for mobile
```

Scan the QR code with the WireGuard mobile app, or import the `.conf` file on desktop.

### Verify

```bash
# Check gateway health (API is bound to the WireGuard interface)
curl http://10.13.13.1:8080/health

# Follow logs
docker compose logs -f
```

### Update

```bash
docker compose pull && docker compose up -d
```

---

## Usage

### Local Development (without Docker)

```bash
# Clone for development only (end users don't need this)
git clone https://github.com/alexandergal86/vpn-gateway.git
cd vpn-gateway

# Build release binary
make build

# Run with debug logging (creates data/ directory)
make run

# Run tests
make test

# Lint + format check + tests
make check
```

Override config path:

```bash
CONFIG_PATH=path/to/custom.json cargo run
```

### Docker Deployment

Single-container deployment with `network_mode: host`, pulling the prebuilt image from `ghcr.io`:

```bash
docker compose up -d        # Start (pulls latest image automatically)
docker compose down         # Stop
docker compose logs -f      # View logs
docker compose pull && docker compose up -d   # Update
```

### Configuration

#### config/gateway.json

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
  "geoip_path": "data/GeoLite2-City.mmdb",
  "state_path": "data/state.json",
  "sources_path": "config/sources.json",
  "connection_pool_max_idle": 60,
  "connection_pool_max_per_proxy": 10,
  "enable_connection_pool": false,
  "sticky_session_ttl": 300,
  "enable_sticky_sessions": false
}
```

All fields have sane defaults. The config supports hot-reload — changes are picked up automatically.

#### config/sources.json

```json
{
  "sources": [
    "https://api.proxyscrape.com/v2/?request=getproxies&protocol=http&timeout=5000&country=all",
    "https://raw.githubusercontent.com/TheSpeedX/PROXY-List/master/http.txt",
    "https://raw.githubusercontent.com/monosans/proxy-list/main/proxies/http.txt",
    "https://api.proxyscrape.com/v2/?request=getproxies&protocol=socks5&timeout=5000&country=all",
    "https://raw.githubusercontent.com/TheSpeedX/PROXY-List/master/socks5.txt"
  ]
}
```

27 built-in sources are used as fallback if the file is missing or malformed.

### API Reference

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/health` | Health check with proxy counts and uptime |
| `GET` | `/api/metrics` | JSON metrics (pool size, latency stats, bans) |
| `GET` | `/metrics` | Prometheus-format metrics |
| `GET` | `/api/proxies` | List active proxies with latency, country, status |
| `POST` | `/api/proxy/add` | Add proxy manually: `{"host":"1.2.3.4","port":8080}` |
| `POST` | `/api/proxy/ban/:key` | Ban proxy (key = `host:port`) |
| `POST` | `/api/proxy/unban/:key` | Unban proxy |
| `GET` | `/api/network-status` | LAN/WAN IP, UPnP status (from net-manager) |
| `GET` | `/api/wg/peers` | WireGuard peer list |

#### Examples

```bash
# Health check
curl -s http://localhost:8080/health | jq .
# {"status":"ok","total_proxies":847,"healthy_proxies":312,"banned_proxies":15,...}

# List top proxies by latency
curl -s http://localhost:8080/api/proxies | jq '.[0:5]'

# Add a custom proxy
curl -X POST http://localhost:8080/api/proxy/add \
  -H 'Content-Type: application/json' \
  -d '{"host":"1.2.3.4","port":8080,"protocol":"http"}'

# Ban a misbehaving proxy
curl -X POST http://localhost:8080/api/proxy/ban/1.2.3.4:8080

# Prometheus scrape
curl -s http://localhost:8080/metrics
# vpn_proxies_total 847
# vpn_proxies_healthy 312
# vpn_proxies_banned 15
```

### GeoIP Setup

```bash
# Option 1: GeoLite2-City (~68MB, more detailed)
make geoip-update

# Option 2: DB-IP City Lite (~19MB, compact)
make geoip-update-dbip
```

Then set `geoip_path` in `config/gateway.json`:

```json
{
  "geoip_path": "data/GeoLite2-City.mmdb",
  "preferred_countries": ["US", "DE", "NL"]
}
```

Without a local database, GeoIP falls back to the `geo.wp-statistics.com` API.

### WireGuard Client Setup

```bash
# Generate keys for a new peer
./scripts/generate_wg_keys.sh peer1

# Generate multiple peers
./scripts/generate_wg_keys.sh --peers 3

# Keys only (no config files)
./scripts/generate_wg_keys.sh --no-config peer1
```

Generated configs are placed in `data/clients/`:
- `peer1-lan.conf` — for clients on the same LAN
- `peer1-wan.conf` — for remote clients (uses UPnP-discovered external IP)
- `peer1-lan.png` / `peer1-wan.png` — QR codes for mobile

### Maintenance

```bash
# View container status and proxy count
make status

# View logs
make docker-logs

# Open shell in gateway container
make shell

# Backup state and configs
make backup

# Update to latest version
make update
```

---

## Startup Sequence

The gateway uses a 4-level fast-start strategy to minimize time-to-first-connection:

| Level | Timing | Action |
|-------|--------|--------|
| **0** | Instant | Load persisted proxy state from `data/state.json` |
| **1** | ~3-5s | Bootstrap from top 3 sources (20 proxies each), fast probe (3s timeout) |
| **2** | Background | Full refresh from all sources in `config/sources.json` |
| **3** | Continuous | Health check loop (30s) + state persistence (300s) |

If the pool is empty at startup, the TCP listener waits on a `Notify` signal until the first healthy proxy becomes available.

Proxies from `state.json` with `last_success < 1 hour` are marked "presumed alive" and used immediately while fresh validation runs in background. Priority order: verified (low latency) → presumed alive → unchecked (just loaded).

---

## Circuit Breaker

The gateway uses an escalating circuit breaker to handle failing proxies:

| Consecutive Failures | Action |
|---------------------|--------|
| 1 | Continue using (jitter added to score) |
| 3 | Score penalty +150 (deprioritized in selection) |
| 5 | Circuit OPEN: disabled for 60 seconds |
| 10 | Disabled for 300 seconds (5 min) |
| 20 | Disabled for 3600 seconds (1 hour) |
| 50 | Permanently removed from pool |

After the cooldown period, the proxy is re-tested. If it passes health check, the failure counter resets.

---

## Failure Scenarios

### Proxy dies mid-session
- Gateway detects EOF/error on upstream connection
- Proxy marked as failed (`record_fail` → circuit breaker)
- Current TCP request fails (can't retry — TLS state is lost)
- Next request automatically routes to a different proxy
- Browser auto-retry usually succeeds transparently

### All proxies in pool die
1. Fall back to "presumed alive" proxies from `state.json`
2. If none available — emergency `fast_probe` (20 random from source list)
3. If still nothing — new connections wait on `Notify` (10s timeout)
4. Background: health checker continues scanning, source manager fetches fresh lists

---

## Latency Estimates

| Path | Expected Latency |
|------|-----------------|
| Direct VPN (no proxy) | ~40-80ms |
| Free proxy through gateway | ~400-1500ms |
| UDP through VPN (DNS, VoIP) | ~50-100ms |

### Free Proxy Statistics

| Metric | Value |
|--------|-------|
| Proxies in public lists | 5,000-8,000 |
| Actually working | 20-30% |
| Average latency (working) | 500-2,000ms |
| Health check timeout | 3-15 seconds |
| Average proxy lifetime | 10 min - 24 hours |
| Source list refresh | Every 1-5 minutes |

---

## Entrypoint & iptables

The Docker entrypoint (`scripts/entrypoint-simple.sh`) configures the network stack:

1. **WireGuard setup** — Generates keys and `wg0.conf` if not exists, brings up `wg0` interface
2. **Unbound DNS** — Starts recursive DNS resolver on `:5353`
3. **FORWARD policy** — `DROP` by default, allow only `wg0↔eth0` (VPN traffic)
4. **NAT** — MASQUERADE for WireGuard subnet, PREROUTING REDIRECT for TCP→`:1080` and DNS→`:5353`
5. **No INPUT/OUTPUT changes** — VPS hoster manages SSH, monitoring, and management ports

**Important**: Runs with `network_mode: host`, so iptables rules apply to the host. INPUT/OUTPUT policies are deliberately left untouched.

---

## Ports

| Port | Protocol | Service |
|------|----------|---------|
| 1080 | TCP | Transparent TCP proxy (gateway) |
| 1081 | UDP | UDP relay (gateway) |
| 5353 | UDP | Unbound DNS resolver |
| 8080 | TCP | REST API and Prometheus metrics (bound to 10.13.13.1) |
| 51820 | UDP | WireGuard VPN tunnel |

---

## Data Directory

```
data/
├── wg/                  # WireGuard keys and peer configs
│   ├── server.key/pub
│   └── peer1/peer1.conf, peer1-qr.png
├── wg0.conf             # WireGuard server config (auto-generated)
├── unbound/             # Unbound DNS config
└── state.json           # Proxy pool state (auto-saved every 300s)
```

---

## Server Requirements

| Requirement | Minimum | Recommended |
|-------------|---------|-------------|
| **CPU** | 1 vCPU | 2 vCPU |
| **RAM** | 512 MB | 1 GB |
| **Disk** | 10 GB | 20 GB |
| **OS** | Ubuntu 22.04+ / Debian 12+ | Ubuntu 24.04 LTS |
| **Docker** | 24.0+ | Latest stable |
| **Open ports** | UDP 51820 | UDP 51820 |

---

## Makefile Reference

```bash
# Core
make build              # cargo build --release
make test               # Run all tests
make run                # Run locally with debug logging
make clean              # cargo clean
make lint               # clippy -D warnings
make fmt                # Check formatting
make fmt-fix            # Auto-fix formatting
make check              # lint + fmt + test
make bench              # Run benchmarks

# Docker
make docker-up          # Start container
make docker-down        # Stop container
make docker-logs        # Follow logs

# Utilities
make status             # Container status + proxy count
make backup             # Backup state and configs
make update             # Pull latest image and restart
make client             # WireGuard client QR code
make shell              # Shell in gateway container
make test-connection    # Test SOCKS5 proxy
make wg-keygen          # Generate WireGuard keys
make wg-show-configs    # Show client configs
```

---

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `nix` crate won't compile | Use Docker (requires Linux kernel headers) |
| `NET_ADMIN` capability error | Add `--cap-add=NET_ADMIN` to Docker or run as root |
| Port already in use | Change port in `config/gateway.json` (hot-reload supported) |
| WireGuard won't start | Check `wg-quick` installation and `wg0.conf` syntax |
| DNS leaks | Verify iptables REDIRECT rules + Unbound is running |
| No proxies loading | Check internet access in container; falls back to `state.json` |
| `ERR_CERT_AUTHORITY_INVALID` | MITM proxy in use — gateway auto-filters these for HTTPS |
| High latency | Enable `exclude_countries` to filter by geography |
| Connection drops | Increase `max_connections` or enable `connection_pool` |

---

## Future Work

### Tests
- [x] Integration tests for persistence module (requires temp directory setup)
- [x] Integration tests for transparent proxy (requires TCP listener + SO_ORIGINAL_DST)
- [x] Integration tests for upstream handshake (requires mock HTTP/SOCKS5 proxy)
- [x] Benchmarks: `cargo bench` for `collect_top_n()`, EWMA scoring, and hot paths

### Features
- [x] Country exclusion filter (`exclude_countries` in config + `select_best()` filtering)
- [x] Single-container deployment (WireGuard + Unbound + Gateway)
- [x] MITM detection — 3-stage TLS validation, two-tier proxy pool
- [x] API bound to WireGuard interface (`10.13.13.1`)
- [ ] IPv6 SO_ORIGINAL_DST support (`sockaddr_in6` in `transparent.rs`)
- [ ] `preferred_countries` implementation (only `exclude_countries` is done)

### Technical Debt
- [x] Config watcher proper lifetime management (replace 60s sleep loop in `config.rs`)
- [x] Hardcoded DNS upstream `10.13.13.1:53` in `udp.rs` (move to config)
- [x] Removed broken multi-container architecture

### Enhancements
- [x] Geo-index for O(1) country-based proxy selection (`HashMap<country, Vec<proxy_key>>` in `state.rs`)
- [x] GeoIP lookup cache (`DashMap` in `geo_ip.rs`) to eliminate redundant API requests
- [x] Prometheus typed metrics — add `# TYPE` / `# HELP` headers to `/metrics` output in `metrics.rs`
- [x] Expanded proxy sources from 11 to 27

---

## License

Apache 2.0

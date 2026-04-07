# VPN Gateway

Transparent TCP/UDP proxy gateway with a dynamic pool of free proxy servers, WireGuard VPN tunneling, and DNS leak protection. Built in Rust for high performance and reliability.

## Overview

## Operational docs

- Full operations manual (RU): `docs/OPERATIONS_MANUAL.ru.md`
- Architecture rethink (RU): `docs/ARCHITECTURE_RETHINK.ru.md`

## Current deployment modes (actual)

Use unified mode-aware commands:

```bash
make env-init MODE=vps|home-vm|home-desktop
make up MODE=vps|home-vm|home-desktop
make status-all MODE=vps|home-vm|home-desktop
make down MODE=vps|home-vm|home-desktop
```

Mode mapping:
- `vps` → `docker-compose.yml`
- `home-vm` → `docker-compose-local.yml` (net-manager + macvlan)
- `home-desktop` → `docker-compose-local.yml` + `docker-compose-dev.yml`

CI note:
- `.github/workflows/ci-mode-tests.yml` runs `make test` and `./scripts/test-mode-automation.sh`.

---

VPN Gateway automatically discovers, validates, and rotates through 1000+ free proxy servers from public lists. TCP traffic from WireGuard clients is transparently proxied through the best-performing servers, selected via EWMA latency scoring and circuit breaker patterns. UDP traffic (including DNS) is relayed through a dedicated channel with Unbound DNS resolver for leak prevention.

### Key Features

- **Transparent proxying** — iptables REDIRECT + `SO_ORIGINAL_DST` for zero-config client setup
- **Smart proxy selection** — EWMA-weighted latency scoring with Top-N random selection
- **Circuit breaker** — Escalating backoff (60s → 300s → 3600s → permanent ban) for failing proxies
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

```
                    ┌──────────────────────────────────────────────────────────────┐
                    │                    Docker Host                               │
                    │                                                              │
 Internet ◄────────┤  ┌─────────────────────────────────────────────────────────┐  │
   :51820/udp       │  │  wireguard (linuxserver/wireguard)                     │  │
                    │  │  ├── wg0: 10.13.13.0/24 (VPN subnet)                  │  │
                    │  │  ├── iptables REDIRECT → :1080 (TCP) / :1081 (UDP)    │  │
                    │  │  │                                                     │  │
                    │  │  │  ┌─────────────────────────────────────────────┐    │  │
                    │  │  │  │  vpn-gateway (Rust)  [network_mode: service]│    │  │
                    │  │  │  │  ├── :1080  Transparent TCP Proxy           │    │  │
                    │  │  │  │  ├── :1081  UDP Relay                       │    │  │
                    │  │  │  │  └── :8080  REST API / Metrics              │    │  │
                    │  │  │  └─────────────────────────────────────────────┘    │  │
                    │  │  │  ┌─────────────────────────────────────────────┐    │  │
                    │  │  │  │  unbound (DNS)       [network_mode: service]│    │  │
                    │  │  │  │  └── :53   Recursive DNS resolver           │    │  │
                    │  │  │  └─────────────────────────────────────────────┘    │  │
                    │  │  └─────────────────────────────────────────────────────┘  │
                    │  │                        │                                  │
                    │  │              vpn-internal (bridge 172.20.0.0/24)          │
                    │  │                        │                                  │
                    │  │  ┌─────────────────────┴──────────────────────────────┐   │
                    │  │  │  net-manager (Python)                              │   │
                    │  │  │  ├── :8088  Config server (Flask)                  │   │
                    │  │  │  ├── UPnP IGD port forwarding                     │   │
                    │  │  │  ├── DHCP IP acquisition                          │   │
                    │  │  │  └── WireGuard config + QR code generation        │   │
                    │  │  └────────────────────────────────────────────────────┘   │
                    │  │                        │                                  │
                    │  │              ext_net (macvlan on physical NIC)            │
                    │  │                        │                                  │
                    │  └────────────────────────┼──────────────────────────────────┘
                    │                           │                                  │
                    └───────────────────────────┼──────────────────────────────────┘
                                                │
                                           LAN / Router
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
  │              │     │              │     │              │     │              │
  │ 11+ public   │     │ Fast probe   │     │ EWMA scoring │     │ Circuit      │
  │ source lists │     │ (3s timeout) │     │ Top-N select │     │ breaker      │
  │ + custom     │     │ TCP connect  │     │ Health check │     │ escalation   │
  │ sources.json │     │ + HTTP test  │     │ loop (30s)   │     │ or stale     │
  └──────────────┘     └──────────────┘     └──────────────┘     └──────────────┘
         │                                         │                     │
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

### 1. Clone and configure

```bash
git clone https://github.com/alexandergal86/vpn-gateway.git
cd vpn-gateway
cp .env.example .env
```

Edit `.env` to match your network:

```bash
# Physical NIC (ip link show)
NET_INTERFACE=eth0

# Your LAN settings
LAN_SUBNET=192.168.1.0/24
LAN_GATEWAY=192.168.1.1

# IP range outside DHCP pool for macvlan
MACVLAN_IP_RANGE=192.168.1.200/29

# Number of WireGuard peers to generate
WG_PEERS=2
```

### 2. Deploy

```bash
# VPS/public IP
make up MODE=vps

# Home Linux VM behind NAT (with net-manager + macvlan)
make up MODE=home-vm

# Docker Desktop (macvlan override)
make up MODE=home-desktop
```

### 3. Connect WireGuard clients

```bash
# Show QR codes for mobile clients
make client

# Or view generated configs
make wg-show-configs
```

Scan the QR code with the WireGuard mobile app, or import the `.conf` file on desktop.

### 4. Verify

```bash
# Check gateway health
curl http://localhost:8080/health

# Check proxy count
make status

# Test connection through proxy
make test-connection
```

---

## Usage

### Local Development (without Docker)

```bash
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

### Docker Deployment Modes

| Mode | Command | Use Case |
|------|---------|----------|
| **Full** | `make docker-full-up` | Production VPS with UPnP + macvlan |
| **Dev** | `make docker-dev-up` | Development without macvlan |
| **Local** | `make docker-local-up` | Local network, no net-manager |
| **VPS** | `make docker-up` | Basic VPS deployment |

### Configuration

#### config/gateway.json

```json
{
  "gateway_port": 1080,
  "api_port": 8080,
  "udp_port": 1081,
  "max_proxies": 5000,
  "max_connections": 10000,
  "health_check_interval": 30,
  "source_update_interval": 300,
  "preferred_countries": ["US", "DE", "NL"],
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

11 built-in sources are used as fallback if the file is missing or malformed.

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

The Docker entrypoint (`scripts/entrypoint.sh`) configures the network stack:

1. **WireGuard setup** — Brings up `wg0` interface (if `wg0.conf` exists)
2. **Kill switch** — `iptables OUTPUT DROP` with exceptions for `lo`, `wg0`, and established connections
3. **DNS redirect** — UDP port 53 → Unbound (:5353)
4. **TCP redirect** — All TCP traffic → gateway (:1080)
5. **IPv6 block** — Full `ip6tables` DROP to prevent leaks

This ensures all client traffic is either proxied (TCP) or tunneled (UDP), with no direct internet leaks.

---

## net-manager (Python Sidecar)

Located in `services/net-manager/`, this service handles network configuration:

| Module | Purpose |
|--------|---------|
| `net_manager.py` | Main orchestrator — IP monitoring, UPnP renewal loop (30s poll) |
| `upnp_client.py` | miniupnpc wrapper — UPnP IGD port forwarding to router |
| `config_generator.py` | WireGuard config generation — LAN + WAN variants per peer |
| `web_server.py` | Flask HTTP server (:8088) — config download + QR codes |

### What it does
1. Acquires a real LAN IP via macvlan + DHCP
2. Discovers router via UPnP IGD and forwards WireGuard port
3. Detects external IP via `GetExternalIPAddress()`
4. Generates WireGuard configs (LAN variant with local IP, WAN variant with external IP)
5. Creates QR codes for mobile client setup
6. Serves configs via HTTP on port 8088
7. Monitors for IP changes every 30 seconds, regenerates configs on change

> **Note**: macvlan doesn't work on Docker Desktop (Windows/Mac). Use `make docker-dev-up` which replaces macvlan with host networking.

---

## Ports

| Port | Protocol | Service |
|------|----------|---------|
| 1080 | TCP | Transparent TCP proxy (gateway) |
| 1081 | UDP | UDP relay (gateway) |
| 8080 | TCP | REST API and Prometheus metrics (gateway) |
| 8088 | TCP | Config server (net-manager) |
| 51820 | UDP | WireGuard VPN tunnel |
| 53 | UDP | Unbound DNS resolver |

---

## Data Directory

```
data/
├── wg/                  # WireGuard server config (auto-generated)
├── clients/             # Generated client configs + QR codes
│   ├── peer1-lan.conf
│   ├── peer1-wan.conf
│   ├── peer1-lan.png
│   └── peer1-wan.png
├── state.json           # Proxy pool state (auto-saved every 300s)
└── network-status.json  # Current IPs, UPnP status (net-manager)
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
make test               # Run all tests (85 tests)
make run                # Run locally with debug logging
make clean              # cargo clean
make lint               # clippy -D warnings
make fmt                # Check formatting
make fmt-fix            # Auto-fix formatting
make check              # lint + fmt + test

# Docker
make docker-up          # VPS mode
make docker-down        # Stop VPS
make docker-local-up    # Local network mode
make docker-local-down  # Stop local
make docker-full-up     # Full stack (VPS + net-manager + UPnP)
make docker-full-down   # Stop full stack
make docker-dev-up      # Dev mode (no macvlan)
make docker-dev-down    # Stop dev
make docker-logs        # Follow logs

# Utilities
make status             # Container status + proxy count
make backup             # Backup state and configs
make update             # Pull latest + rebuild
make client             # WireGuard client QR code
make shell              # Shell in gateway container
make test-connection    # Test SOCKS5 proxy
make wg-keygen          # Generate WireGuard keys
make wg-show-configs    # Show client configs

# GeoIP
make geoip-update       # GeoLite2-City (~68MB)
make geoip-update-dbip  # DB-IP City Lite (~19MB)
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
| macvlan not working | Ensure `NET_INTERFACE` in `.env` matches `ip link show` output |
| UPnP fails | Router may not support IGD; use manual port forwarding |
| High latency | Enable `preferred_countries` to filter by geography |
| Connection drops | Increase `max_connections` or enable `connection_pool` |

---

## Future Work

### Tests
- [x] Integration tests for persistence module (requires temp directory setup)
- [x] Integration tests for transparent proxy (requires TCP listener + SO_ORIGINAL_DST)
- [x] Integration tests for upstream handshake (requires mock HTTP/SOCKS5 proxy)
- [x] Benchmarks: `cargo bench` for `collect_top_n()`, EWMA scoring, and hot paths

### Features
- [ ] IPv6 SO_ORIGINAL_DST support (`sockaddr_in6` in `transparent.rs`)
- [ ] DDNS integration for WAN configs (alternative to UPnP external IP)
- [ ] API authentication (currently relies on WireGuard-only network access)

### Technical Debt
- [x] Config watcher proper lifetime management (replace 60s sleep loop in `config.rs`)
- [x] Python path traversal sanitization in `web_server.py` (net-manager)
- [x] Hardcoded DNS upstream `10.13.13.1:53` in `udp.rs` (move to config)

### Enhancements
- [x] Geo-index for O(1) country-based proxy selection (`HashMap<country, Vec<proxy_key>>` in `state.rs`)
- [x] GeoIP lookup cache (`DashMap` in `geo_ip.rs`) to eliminate redundant API requests
- [x] Prometheus typed metrics — add `# TYPE` / `# HELP` headers to `/metrics` output in `metrics.rs`

---

## License

Apache 2.0

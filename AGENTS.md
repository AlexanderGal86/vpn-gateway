# VPN Gateway — Agent Operational Guide

## Project Status
- **Core Functionality**: Implemented, tested, Docker-deployed
- **Build Status**: Clean compile, 0 clippy warnings
- **Runtime**: Proxy :1080, UDP :1081, API :8080, net-manager :8088
- **Tests**: 85/85 passing
- **Lines of Code**: ~2200 Rust + ~600 Python
- **Language**: Rust (Edition 2021), Tokio async runtime

---

## Architecture Overview

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

### TCP Request Flow
1. Client connected via WireGuard tunnel
2. Client opens TCP connection to target IP
3. `iptables REDIRECT` → gateway :1080
4. Gateway extracts original destination via `SO_ORIGINAL_DST`
5. Gateway extracts domain from TLS SNI (fallback: HTTP Host header)
6. Gateway checks sticky session for client IP
7. Gateway selects upstream proxy (weighted random from top-10, EWMA-scored)
8. Gateway performs CONNECT/SOCKS5 handshake with upstream proxy
9. Gateway relays data bidirectionally (`copy_bidirectional`, 300s idle timeout)

### UDP Flow
- TCP traffic goes through proxy pool; UDP goes directly through WireGuard VPN
- Free proxies don't support UDP ASSOCIATE (RFC 1928)
- UDP via VPN: ~50-100ms latency
- DNS routed to Unbound at 10.13.13.1:53

---

## Module Structure

### `src/pool/` — Proxy Pool Management
| File | Purpose | Tests |
|------|---------|-------|
| `state.rs` | `SharedState` (DashMap), banned list, `collect_top_n` selection, pool size limit | 14 |
| `proxy.rs` | Proxy entry with EWMA (α=0.2), circuit breaker, NaN/Inf guard | 12 |
| `source_manager.rs` | 11 sources + sources.json, parallel loading, per-source cap (500), retry + rate limit | 6 |
| `health_checker.rs` | 2-stage check (TCP connect + HTTP CONNECT), GeoIP semaphore (20) | 4 |
| `persistence.rs` | Atomic save/load data/state.json (tmp + rename) | TODO |
| `sticky_sessions.rs` | Client IP → proxy affinity, lock-free TTL (AtomicU64) | 5 |
| `connection_pool.rs` | Optional TCP connection reuse, liveness check outside mutex | 2 |
| `geo_ip.rs` | GeoIP via API (geo.wp-statistics.com), lazy lookup on verify | 2 |
| `metrics.rs` | Prometheus-format metrics, country breakdown | — |

### `src/proxy/` — Traffic Handling
| File | Purpose | Tests |
|------|---------|-------|
| `transparent.rs` | SO_ORIGINAL_DST + TCP relay, Semaphore-bounded connections, 300s idle timeout | TODO |
| `upstream.rs` | HTTP CONNECT + SOCKS5 upstream handshake | 5 |
| `sniff.rs` | TLS SNI from ClientHello (safe `from_utf8`), HTTP Host header | 9 |
| `udp.rs` | UDP relay on :1081, Semaphore-bounded (1000 tasks), DNS → Unbound | 4 |

### `src/api/web.rs` — Axum HTTP API on :8080 (13 tests)
### `src/config.rs` — JSON config with hot-reload via `notify` crate (8 tests)
### `src/main.rs` — Entry point, 4-level startup, graceful shutdown with JoinHandle tracking

---

## Startup Sequence (4-Level Fast-Start)

| Level | Timing | Description |
|-------|--------|-------------|
| **0: Instant** | 0s | Load persisted state from `data/state.json`. Proxies with `last_success < 1h` marked "presumed alive" |
| **1: Fast Probe** | 3-8s | Bootstrap from top 3 sources, probe first 20 from each (60 total, parallel, 3s timeout). 2-stage verification (TCP + HTTP CONNECT) |
| **2: Background Scan** | 30-60s | Full refresh from all sources in `config/sources.json`, deduplicate → ~6000 proxies, check in batches of 100 |
| **3: Continuous** | ongoing | Health check loop (5s/30s) + state persistence (every 300s) + config hot-reload |

**Result**: From container start to serving first client: **3-6 seconds**

### "Presumed Alive" Logic
Proxies from state.json with `last_success < 1 hour` are marked as presumed alive:
1. Priority: Verified proxies with low latency
2. Fallback: PresumedAlive (from state.json)
3. Last resort: Unchecked (just loaded from source)

### Early Return in Health Check
Each working proxy is added to pool immediately via `pool.first_ready.notify_waiters()`. Client requests arriving before first proxy found wait on this notify (10s timeout).

---

## Circuit Breaker Timeouts

| Fails | Action |
|-------|--------|
| 1 | Continue using (jitter in score) |
| 3 | Score increased by +150 (low priority) |
| 5 | Circuit OPEN: disabled for 60 seconds |
| 10 | Disabled for 300 seconds |
| 20 | Disabled for 3600 seconds (1 hour) |
| 50 | Remove from pool (dead proxy) |

---

## Key Design Patterns

- All state flows through `SharedState` (clone of Arc-wrapped DashMap collections)
- Single-pass top-N selection (`collect_top_n`) — O(n) scan, O(10) memory per selection
- Proxy selection uses EWMA-weighted latency with circuit breaker pattern
- Bounded concurrency: TCP (Semaphore from `max_connections`), UDP (Semaphore, 1000), GeoIP (Semaphore, 20)
- Pool size enforced via `max_proxies` config (AtomicUsize in SharedState)
- Per-source proxy cap (500) prevents rogue source flooding
- Sticky session TTL uses AtomicU64 (lock-free reads, Relaxed ordering)
- Connection pool: liveness check runs outside mutex to avoid blocking
- EWMA latency clamped to [0, 60000] with NaN/infinity guard
- Config lives in `config/gateway.json`, proxy sources in `config/sources.json`
- Persistent state in `data/state.json` (auto-saved periodically)
- Target platform is Linux (uses `nix` crate for SO_ORIGINAL_DST, iptables for traffic redirection)
- Graceful shutdown: all tasks tracked via JoinHandle, aborted on Ctrl+C, state saved before exit

---

## Latency Estimates

| Path | Latency |
|------|---------|
| Direct VPN (no proxy) | ~40-80ms |
| Paid proxy (Oxylabs etc.) | ~100-300ms |
| Free proxy through gateway | ~400-1500ms |
| UDP through VPN (VoIP) | ~50-100ms |

---

## Free Proxy Metrics (from research)

| Metric | Value |
|--------|-------|
| Proxies in public lists | 5000-8000 |
| Actually working | 20-30% |
| Average latency (working) | 500-2000ms |
| Max timeout during check | 5-15 seconds |
| Uptime (stays alive) | ~70% |
| Lifetime | 10 min - 24 hours |
| Source list load speed | 1-3 seconds |
| Source update frequency | every 1-5 minutes |

---

## Docker

### Three Modes
- `docker-compose.yml` — Full stack: WireGuard + Unbound + Gateway + net-manager (macvlan)
- `docker-compose-local.yml` — Local network mode (WireGuard + Gateway + Unbound)
- `docker-compose-dev.yml` — Dev override for Docker Desktop (no macvlan)

### Container Architecture (4 services)
```yaml
services:
  wireguard:          # lscr.io/linuxserver/wireguard, port 51820/udp
  vpn-gateway:        # Custom Rust build, network_mode: service:wireguard
  unbound:            # mvance/unbound, network_mode: service:wireguard
  net-manager:        # Python sidecar, macvlan for UPnP + DHCP
```

### Networks
- `vpn-internal` (172.20.0.0/24) — inter-container communication
- `ext_net` (macvlan) — LAN access for net-manager

### Entrypoint Logic
1. Conditional WireGuard setup (if wg0.conf exists and wg-quick available)
2. iptables kill-switch (OUTPUT DROP, allow lo + wg0 + established)
3. DNS redirect (UDP:53 → Unbound:5353)
4. TCP redirect (all TCP → gateway:1080)
5. IPv6 full block

---

## net-manager (Python Sidecar)

Located in `services/net-manager/`:
- `net_manager.py` — Main orchestrator, UPnP + DHCP polling loop
- `upnp_client.py` — miniupnpc wrapper for port forwarding
- `config_generator.py` — WireGuard config + QR code generation (LAN + WAN variants)
- `web_server.py` — Flask HTTP server for config download (:8088)

### Responsibilities
1. DHCP monitoring — detect own IP via macvlan interface
2. UPnP port forwarding — add/refresh mapping for WireGuard port
3. External IP discovery — GetExternalIPAddress() via UPnP
4. IP change detection — poll every 30s
5. Config generation — LAN + WAN configs + QR codes per peer
6. Config serving — HTTP endpoint on :8088

### Windows Docker Desktop Compatibility
macvlan doesn't work on Docker Desktop. Dev override (`docker-compose-dev.yml`) replaces macvlan with host networking.

---

## Error Handling & Failure Scenarios

### Proxy dies mid-session
- Gateway detects EOF/error on upstream side
- Marks proxy as failed (record_fail)
- Current request fails (can't retry at TCP level — TLS state lost)
- Next requests automatically use different proxy
- Browser auto-retry usually succeeds

### All proxies in pool die
1. Switch to presumed_alive proxies
2. If none — emergency fast_probe (20 random from source list)
3. If still nothing — return 502 to client
4. Background: health_checker continues scanning, source_manager loads fresh lists

---

## Configuration

### config/gateway.json
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

### config/sources.json
11 public sources (HTTP + SOCKS5), refresh interval: 5 minutes. Per-source cap: 500 proxies.

### .env.example
```
NET_INTERFACE=eth0
LAN_SUBNET=192.168.1.0/24
LAN_GATEWAY=192.168.1.1
MACVLAN_IP_RANGE=192.168.1.200/29
WG_PORT=51820
WG_PEERS=2
```

---

## Build & Run Commands

```bash
make build              # cargo build --release
make test               # cargo test (85 tests)
make run                # RUST_LOG=vpn_gateway=debug cargo run
make clean              # cargo clean
make lint               # cargo clippy -- -D warnings
make fmt                # cargo fmt -- --check
make fmt-fix            # cargo fmt
make check              # lint + fmt + test (full CI check)
make docker-local-up    # Docker Compose for local network mode
make docker-local-down  # Stop local containers
make docker-full-up     # Full stack: VPS + net-manager + UPnP
make docker-full-down   # Stop full stack
make docker-dev-up      # Dev mode (no macvlan, for Docker Desktop)
make docker-dev-down    # Stop dev stack
make geoip-update       # Download GeoLite2-City (~68MB)
make geoip-update-dbip  # Download DB-IP City Lite (~19MB)
make wg-keygen          # Generate WireGuard keys
make wg-show-configs    # Show generated client configs
make status             # Container status + proxy count
make backup             # Backup state and configs
make update             # Pull latest + rebuild
make client             # Show WireGuard client QR code
make shell              # Open shell in gateway container
make test-connection    # Test proxy connection
```

Run a single test: `cargo test <test_name>`
Config path override: `CONFIG_PATH=path/to/config.json cargo run`

---

## API Endpoints

```
GET  /health              → {"status":"ok","total_proxies":N,...}
GET  /api/metrics         → JSON metrics
GET  /metrics             → Prometheus format
GET  /api/proxies         → [{host,port,latency_ms,...},...]
POST /api/proxy/add       → {"host":"1.2.3.4","port":8080}
POST /api/proxy/ban/:key  → ban proxy
POST /api/proxy/unban/:key → unban proxy
GET  /api/network-status  → LAN/WAN IP, UPnP status (from net-manager)
GET  /api/wg/peers        → list WireGuard peers
```

---

## Test Coverage

| Module | Tests | Coverage |
|--------|-------|----------|
| `pool::state` | 14 | ✅ Insert, select, ban/unban, weighted random, pool limit, collect_top_n, full lifecycle |
| `pool::proxy` | 12 | ✅ EWMA, circuit breaker, score, serialization, NaN/Inf guard |
| `api::web` | 13 | ✅ Health, metrics, proxies, add/ban/unban, prometheus format, network-status |
| `proxy::sniff` | 9 | ✅ TLS SNI, HTTP Host, CONNECT, edge cases |
| `config` | 8 | ✅ Defaults, JSON parse, partial config, hot-reload, malformed JSON |
| `pool::source_manager` | 6 | ✅ Parse valid/invalid, file loading, per-source cap |
| `pool::sticky_sessions` | 5 | ✅ Set/get, expiry, multi-session, clear |
| `proxy::upstream` | 5 | ✅ HTTP header parsing |
| `proxy::udp` | 4 | ✅ DNS detection, edge cases |
| `pool::health_checker` | 4 | ✅ Empty batch, unreachable proxies |
| `pool::connection_pool` | 2 | ⚠️ Creation only (module unused) |
| `pool::geo_ip` | 2 | ⚠️ Creation only (API not mocked) |
| `pool::persistence` | 0 | ❌ Requires temp directory |
| `proxy::transparent` | 0 | ❌ Requires TCP listener |
| **Total** | **85** | **100% sync modules** |

---

## GeoIP

Uses API geo.wp-statistics.com:
```bash
curl https://geo.wp-statistics.com/8.8.8.8?format=json
```

Local database download:
```bash
make geoip-update        # GeoLite2-City (~68MB)
make geoip-update-dbip   # DB-IP City Lite (~19MB)
```

Lazy lookup: GeoIP resolved asynchronously when proxy passes health check. Bounded by semaphore (20 concurrent).

---

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `nix` doesn't compile | Use Docker (requires Linux kernel headers) |
| NET_ADMIN unavailable | Docker needs `--cap-add=NET_ADMIN` |
| Port already in use | Change port in config/gateway.json (hot-reload supported) |
| state.json save fails | Check data/ directory is writable |
| WireGuard won't start | Verify wg-quick installed and wg0.conf exists |
| DNS leaks | Verify iptables REDIRECT rule + Unbound running |
| Transparent proxy fails | Container needs NET_ADMIN capability |
| Sources fail in Docker | Expected — fallback to state.json proxies |
| macvlan not working | Check NET_INTERFACE in .env matches `ip link show` |
| UPnP fails | Router may not support IGD; use manual port forwarding |

---

## Future Work

### Tests
- [ ] Integration tests for persistence module (requires temp directory setup)
- [ ] Integration tests for transparent proxy (requires TCP listener + SO_ORIGINAL_DST)
- [ ] Integration tests for upstream handshake (requires mock HTTP/SOCKS5 proxy)
- [ ] Benchmarks: `cargo bench` for `collect_top_n()`, EWMA scoring, and hot paths

### Features
- [ ] IPv6 SO_ORIGINAL_DST support (`sockaddr_in6` in `transparent.rs`)
- [ ] DDNS integration for WAN configs (alternative to UPnP external IP)
- [ ] API authentication (currently relies on WireGuard-only network access)

### Technical Debt
- [x] Config watcher proper lifetime management (replace 60s sleep loop in `config.rs`)
- [x] Python path traversal sanitization in `web_server.py` (net-manager)
- [x] Hardcoded DNS upstream `10.13.13.1:53` in `udp.rs` (move to config)

### Enhancements
- [ ] Geo-index for O(1) country-based proxy selection (`HashMap<country, Vec<proxy_key>>` in `state.rs`)
- [ ] GeoIP lookup cache (`DashMap` in `geo_ip.rs`) to eliminate redundant API requests
- [ ] Prometheus typed metrics — add `# TYPE` / `# HELP` headers to `/metrics` output in `metrics.rs`

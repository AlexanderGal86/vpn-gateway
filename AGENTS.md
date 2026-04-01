# VPN Gateway — Agent Operational Guide

## Project Status
- **Core Functionality**: Implemented, tested, Docker-deployed
- **Build Status**: Clean compile, 0 warnings
- **Runtime**: Proxy :1080, UDP :1081, API :8080, net-manager :8088
- **Tests**: 72/72 passing
- **Lines of Code**: ~2200 Rust + ~600 Python
- **Language**: Rust (Edition 2021), Tokio async runtime

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                DOCKER CONTAINER                      │
│                                                      │
│  WireGuard (wg0, 10.13.13.1)                       │
│       │                                              │
│       │ iptables REDIRECT                            │
│       ▼                                              │
│  Rust Gateway (:1080)                                │
│       │                                              │
│       ├─ SNI sniffing (извлечь домен из TLS)         │
│       ├─ Sticky Sessions (привязка клиента к прокси) │
│       ├─ ProxyPool (DashMap, EWMA scoring)          │
│       ├─ Upstream handshake (CONNECT / SOCKS5)       │
│       └─ copy_bidirectional (relay данных)           │
│       │                                              │
│       ▼                                              │
│  Free Proxy (1000+ из публичных списков) → Internet │
│                                                      │
│  UDP Relay (:1081) — DNS → Unbound (10.13.13.1:53) │
│  Web API (:8080) — мониторинг                        │
│  net-manager (:8088) — UPnP + DHCP + QR configs     │
└─────────────────────────────────────────────────────┘
```

### TCP Request Flow
1. Клиент подключён через WireGuard
2. Клиент открывает TCP-соединение к IP сайта
3. `iptables REDIRECT` → gateway :1080
4. Gateway получает оригинальный IP через `SO_ORIGINAL_DST`
5. Gateway извлекает домен из TLS SNI (fallback: HTTP Host header)
6. Gateway проверяет sticky session
7. Gateway выбирает upstream proxy (weighted random из top-10)
8. Gateway выполняет CONNECT/SOCKS5 handshake
9. Gateway пересылает данные (zero-copy `copy_bidirectional`, 300s idle timeout)

### UDP Flow
- TCP через proxy pool, UDP напрямую через WireGuard VPN
- Free proxy не поддерживают UDP ASSOCIATE (RFC 1928)
- UDP через VPN: ~50-100ms latency
- DNS routed to Unbound at 10.13.13.1:53

---

## Module Structure

### `src/pool/` — Proxy Pool Management
| File | Purpose | Tests |
|------|---------|-------|
| `state.rs` | `SharedState` (DashMap), banned list, weighted random selection | 11 |
| `proxy.rs` | Proxy entry with EWMA (α=0.2), circuit breaker | 11 |
| `source_manager.rs` | 11 sources + sources.json, parallel loading, retry + rate limit | 6 |
| `health_checker.rs` | 2-stage check (TCP connect + HTTP CONNECT), adaptive loop | 4 |
| `persistence.rs` | Atomic save/load data/state.json (tmp + rename) | TODO |
| `sticky_sessions.rs` | Client IP → proxy affinity, TTL-based, runtime TTL update | 5 |
| `connection_pool.rs` | Optional TCP connection reuse (disabled by default) | 2 |
| `geo_ip.rs` | GeoIP via API (geo.wp-statistics.com), lazy lookup on verify | 2 |
| `metrics.rs` | Prometheus-format metrics, country breakdown | — |

### `src/proxy/` — Traffic Handling
| File | Purpose | Tests |
|------|---------|-------|
| `transparent.rs` | SO_ORIGINAL_DST + TCP relay, 300s idle timeout | TODO |
| `upstream.rs` | HTTP CONNECT + SOCKS5 upstream handshake | 5 |
| `sniff.rs` | TLS SNI from ClientHello, HTTP Host header | 9 |
| `udp.rs` | UDP relay on :1081, DNS → Unbound | 4 |

### `src/api/web.rs` — Axum HTTP API on :8080 (5 tests)
### `src/config.rs` — JSON config with hot-reload via `notify` crate (6 tests)
### `src/main.rs` — Entry point, graceful shutdown with JoinHandle tracking

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

## Latency Estimates

| Path | Latency |
|------|---------|
| Direct VPN (no proxy) | ~40-80ms |
| Paid proxy (Oxylabs etc.) | ~100-300ms |
| Free proxy through gateway | ~400-1500ms |
| UDP through VPN (VoIP) | ~50-100ms |

---

## Default Ports

| Port | Service |
|------|---------|
| 1080 | Transparent TCP proxy |
| 1081 | UDP relay |
| 8080 | API/metrics |
| 8088 | net-manager config server |
| 51820/udp | WireGuard |

---

## Build & Run Commands

```bash
make build              # cargo build --release
make test               # cargo test (72 tests)
make run                # RUST_LOG=vpn_gateway=debug cargo run
make clean              # cargo clean
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

## Key Design Patterns

- All state flows through `SharedState` (clone of Arc-wrapped DashMap collections)
- Proxy selection uses weighted random from top-10 (EWMA-weighted latency + circuit breaker)
- Config lives in `config/gateway.json`, proxy sources in `config/sources.json`
- Persistent state in `data/state.json` (auto-saved every 300s)
- Target platform is Linux (uses `nix` crate for SO_ORIGINAL_DST, iptables for traffic redirection)
- UDP routed directly through VPN (free proxies don't support UDP ASSOCIATE)
- Graceful shutdown: all tasks tracked via JoinHandle, aborted on Ctrl+C, state saved before exit

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

### WireGuard Key Generation
```bash
./scripts/generate_wg_keys.sh peer1
./scripts/generate_wg_keys.sh --peers 3
./scripts/generate_wg_keys.sh --no-config peer1
```

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
- `config_generator.py` — WireGuard config + QR code generation
- `web_server.py` — Flask HTTP server for config download

### Responsibilities
1. DHCP monitoring — detect own IP via macvlan interface
2. UPnP port forwarding — add/refresh mapping for WireGuard port
3. External IP discovery — GetExternalIPAddress() via UPnP
4. IP change detection — poll every 30s
5. Config generation — LAN + WAN configs + QR codes
6. Config serving — HTTP endpoint on :8088

### Windows Docker Desktop Compatibility
macvlan doesn't work on Docker Desktop. Dev override (`docker-compose-dev.yml`) replaces macvlan with host networking.

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

Lazy lookup: GeoIP resolved asynchronously when proxy passes health check.

---

## Configuration

### config/gateway.json
```json
{
  "gateway_port": 1080,
  "api_port": 8080,
  "udp_port": 1081,
  "max_connections": 10000,
  "enable_connection_pool": false,
  "sources_path": "config/sources.json"
}
```

### config/sources.json
11 public sources (HTTP + SOCKS5), refresh interval: 5 minutes.

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

## Test Coverage

| Module | Tests | Coverage |
|--------|-------|----------|
| `pool::proxy` | 11 | ✅ EWMA, circuit breaker, score, serialization |
| `pool::state` | 11 | ✅ Insert, select, ban/unban, weighted random |
| `proxy::sniff` | 9 | ✅ TLS SNI, HTTP Host, CONNECT, edge cases |
| `config` | 6 | ✅ Defaults, JSON parse, partial config |
| `pool::sticky_sessions` | 5 | ✅ Set/get, expiry, multi-session, clear |
| `pool::source_manager` | 6 | ✅ Parse valid/invalid, file loading |
| `api::web` | 5 | ✅ Health, metrics, proxies, network-status |
| `proxy::upstream` | 5 | ✅ HTTP header parsing |
| `proxy::udp` | 4 | ✅ DNS detection, edge cases |
| `pool::health_checker` | 4 | ✅ Empty batch, unreachable proxies |
| `pool::connection_pool` | 2 | ⚠️ Creation only (module unused) |
| `pool::geo_ip` | 2 | ⚠️ Creation only (API not mocked) |
| `pool::persistence` | 0 | ❌ Requires temp directory |
| `proxy::transparent` | 0 | ❌ Requires TCP listener |
| **Total** | **72** | **100% sync modules** |

---

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `nix` doesn't compile | Use Docker (requires Linux) |
| NET_ADMIN unavailable | Docker needs `--cap-add=NET_ADMIN` |
| Port already in use | Change port in config/gateway.json |
| state.json save fails | Check data/ directory is writable |
| WireGuard won't start | Verify wg-quick installed and wg0.conf exists |
| DNS leaks | Verify iptables REDIRECT rule + Unbound running |
| Transparent proxy fails | Container needs NET_ADMIN capability |
| Sources fail in Docker | Expected — fallback to state.json proxies |

---

## Completed Tasks
- [x] Fix persistence.rs directory creation issue
- [x] Build and run the project successfully
- [x] Verify state.json is being saved and loaded
- [x] Check API endpoints are accessible
- [x] Implement WireGuard setup in entrypoint.sh
- [x] Implement DNS solution (Unbound in docker-compose)
- [x] Prepare Docker Compose stack
- [x] Add source_manager load from config/sources.json
- [x] Add Prometheus /metrics endpoint
- [x] Add Ban/Unban API
- [x] Implement UDP relay
- [x] Integrate Sticky Sessions
- [x] Create Makefile with geoip-update
- [x] Implement GeoIP via API (geo.wp-statistics.com)
- [x] Add local network mode (docker-compose-local.yml)
- [x] Add auto-detect IP in generate_wg_keys.sh
- [x] Make Connection Pool optional
- [x] Docker deployment tested and working
- [x] Fix config watcher — notify crate integrated
- [x] Fix UDP relay — route to Unbound (10.13.13.1:53)
- [x] Fix double-counting in health_checker
- [x] Fix graceful shutdown — JoinHandle tracking
- [x] Fix dead code warnings (0 warnings)
- [x] Add idle timeout to copy_bidirectional (300s)
- [x] Add rate limiting + retry on source loading
- [x] Add weighted random proxy selection (top-N)
- [x] Add GeoIP lazy lookup on proxy verification
- [x] Add 2-stage health check (TCP + HTTP CONNECT)
- [x] Add net-manager container (UPnP + DHCP + QR)
- [x] Add docker-compose-dev.yml for Docker Desktop
- [x] Expand test suite to 72 tests
- [x] Consolidate all scripts into scripts/ directory
- [x] Add .dockerignore

## Pending Tasks
- [ ] Integration tests for persistence (requires temp directory)
- [ ] Integration tests for transparent proxy (requires TCP listener)
- [ ] Integration tests for upstream handshake (requires mock proxy)
- [ ] DDNS integration for WAN configs (optional enhancement)
- [ ] API authentication (optional, WG-only access is sufficient for now)

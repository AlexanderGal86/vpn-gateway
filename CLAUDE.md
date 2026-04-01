# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
make build              # cargo build --release
make test               # cargo test
make run                # RUST_LOG=vpn_gateway=debug cargo run (creates data/ dir)
make clean              # cargo clean
make docker-up          # Docker Compose for VPS deployment (4 services)
make docker-local-up    # Docker Compose for local network mode
make docker-down        # Stop VPS containers
make docker-local-down  # Stop local containers
make status             # Show container status and proxy count
make client             # Show WireGuard client QR code
make shell              # Open shell in gateway container
make test-connection    # Test proxy connection via SOCKS5
```

Run a single test: `cargo test <test_name>`

Config path override: `CONFIG_PATH=path/to/config.json cargo run`

## Architecture

Rust (tokio async) transparent TCP/UDP proxy gateway that routes traffic through a dynamic pool of free proxy servers. Runs on Linux with iptables redirecting WireGuard client traffic to the gateway.

### 4-Service Docker Architecture

```
ext_net (macvlan) ──── net-manager (Python: UPnP, DHCP, config gen, :8088)
                            │
vpn-internal (bridge) ──────┤
                            │
                       wireguard (:51820/udp, wg0: 10.13.13.0/24)
                            ├── vpn-gateway (Rust, :1080 TCP proxy, :8080 API)
                            └── unbound (DNS, :53)
```

- **vpn-internal** (bridge 172.20.0.0/24): inter-service communication
- **ext_net** (macvlan on physical NIC): net-manager gets real LAN IP via DHCP for UPnP
- vpn-gateway and unbound share wireguard's network namespace (`network_mode: service:wireguard`)

### Startup Sequence (4-level fast-start in `src/main.rs`)

1. **Level 0 (instant)**: Load persisted proxy state from `data/state.json`
2. **Level 1 (fast)**: Bootstrap from top 3 sources, fast-probe all proxies (3s timeout). TCP proxy starts accepting connections immediately (waits on `first_ready` if pool empty)
3. **Level 2 (background)**: Full refresh from all sources in `config/sources.json`, then periodic refresh loop
4. **Level 3 (continuous)**: Health check loop + state persistence loop (every 300s)

### Module Structure

- **`src/pool/`** — Proxy pool management
  - `state.rs` — `SharedState` (DashMap-based), banned list, the central state object passed everywhere
  - `proxy.rs` — Proxy entry with EWMA latency scoring and circuit breaker
  - `source_manager.rs` — Fetches proxies from 11 hardcoded sources + `config/sources.json`
  - `health_checker.rs` — Fast probe and continuous health check loop
  - `persistence.rs` — Save/load `data/state.json`
  - `sticky_sessions.rs` — Client IP to proxy affinity
  - `connection_pool.rs` — Optional TCP connection reuse
  - `geo_ip.rs` — GeoIP via external API (geo.wp-statistics.com)
  - `metrics.rs` — Prometheus-format metrics

- **`src/proxy/`** — Traffic handling
  - `transparent.rs` — SO_ORIGINAL_DST + TCP relay (the main proxy listener on :1080)
  - `upstream.rs` — HTTP CONNECT and SOCKS5 upstream proxy protocols
  - `sniff.rs` — TLS SNI extraction from ClientHello
  - `udp.rs` — UDP relay on :1081

- **`src/api/web.rs`** — Axum HTTP API on :8080 (/health, /metrics, /api/proxies, ban/unban)
- **`src/config.rs`** — JSON config with hot-reload support, `ConfigManager` with file watching

- **`services/net-manager/`** — Python sidecar for network management
  - `upnp_client.py` — UPnP IGD port forwarding
  - `config_generator.py` — WireGuard client config generation (LAN + WAN variants, QR codes)
  - `web_server.py` — Flask HTTP server for config distribution (:8088)
  - `net_manager.py` — Main loop: IP monitoring, UPnP renewal, config regeneration

### Key Design Patterns

- All state flows through `SharedState` (clone of Arc-wrapped DashMap collections)
- Proxy selection uses EWMA-weighted latency with circuit breaker pattern
- Config lives in `config/gateway.json`, proxy sources in `config/sources.json`
- Persistent state in `data/state.json` (auto-saved periodically)
- Target platform is Linux (uses `nix` crate for SO_ORIGINAL_DST, iptables for traffic redirection)
- net-manager generates two WG configs per peer: LAN (local IP) and WAN (external IP via UPnP)

### Default Ports

| Port | Service |
|------|---------|
| 1080 | Transparent TCP proxy |
| 1081 | UDP relay |
| 8080 | Gateway API/metrics |
| 8088 | net-manager config server |
| 51820/udp | WireGuard |

### Docker

Two compose files: `docker-compose.yml` (production with macvlan for UPnP) and `docker-compose-local.yml` (local dev with host networking for net-manager). Configure via `.env` file (NET_INTERFACE, LAN_SUBNET, LAN_GATEWAY, DOCKER_HOST_IP, WG_PEERS).

### Data Directory

```
data/
├── wg/              # WireGuard server config (linuxserver image)
├── clients/         # Generated LAN/WAN configs + QR codes (net-manager)
├── state.json       # Proxy pool state (gateway)
└── network-status.json  # Current IPs, UPnP status (net-manager)
```

# Session Log - 2026-04-10

## Task
Запустить сервис и проверить работоспособность. Настроить доступ к API/dashboard только с WireGuard клиентов и локально.

## Fixes Applied

### 1. Dashboard folder in Dockerfile
Added missing `COPY dashboard ./dashboard` to Dockerfile.

### 2. Rustls CryptoProvider panic
Build succeeded but container crashed with rustls panic:
```
Could not automatically determine the process-level CryptoProvider from Rustls crate features.
Call CryptoProvider::install_default() before this point
```
**Fix**: Added `ring` feature to rustls in Cargo.toml:
```toml
rustls = { version = "0.23", default-features = false, features = ["std", "tls12", "ring"] }
```
**Fix**: Added CryptoProvider initialization in main.rs:
```rust
let _ = rustls::crypto::ring::default_provider().install_default();
```

### 3. API/Dashboard access from WireGuard
API was accessible only from WireGuard IP (10.13.13.1), not from WireGuard clients.
**Cause**: iptables REDIRECT rule was redirecting ALL TCP traffic from wg0 to proxy, including port 8080.
**Fix**: Changed iptables rules to redirect only ports 80 and 443 (not 8080):
```bash
# Redirect TCP traffic from WireGuard clients to proxy, excluding API port 8080
iptables -t nat -A PREROUTING -i wg0 -p tcp --dport 80 -j REDIRECT --to-port 1080
iptables -t nat -A PREROUTING -i wg0 -p tcp --dport 443 -j REDIRECT --to-port 1080
```

Added INPUT rules for WireGuard access:
```bash
iptables -A INPUT -i wg0 -p tcp --dport 8080 -j ACCEPT
```

### 4. Metrics API missing TLS counts
API `/api/metrics` returned 0 for mitm_proxies and tls_clean_proxies.
**Fix**: Added fields to MetricsResponse and handler:
- `tls_clean_proxies: state.tls_clean_count()`
- `mitm_proxies: state.tls_dirty_count()`

### 5. Dashboard not showing TLS counts
Dashboard showed "--" for TLS Clean and TLS MITM.
**Cause**: ProxyInfo struct didn't include `tls_clean` field.
**Fix**: Added `tls_clean: Option<bool>` to ProxyInfo and included in API response.

## Final Status
- Container: `vpn-gateway` - running
- API: `http://10.13.13.1:8080` - accessible from WireGuard clients
- Localhost: `http://127.0.0.1:8080` - accessible locally
- External: BLOCKED
- Proxies: ~5000 total, ~44 TLS-Clean, ~195 MITM

## Files Modified
- `Dockerfile` - added dashboard copy
- `Cargo.toml` - added ring feature
- `src/main.rs` - added crypto provider init
- `src/api/web.rs` - added tls_clean_proxies and mitm_proxies to metrics, added tls_clean to ProxyInfo
- `scripts/entrypoint-simple.sh` - fixed iptables redirect rules

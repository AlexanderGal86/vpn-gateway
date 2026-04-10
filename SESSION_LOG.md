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

---

# Session Log - 2026-04-10 (Build & Distribution)

## Task
Привести проект к финальному рабочему состоянию. Подготовить к публикации на GitHub как самодостаточный дистрибутив.

## Context
Рабочая версия (`vpn-gateway-old`) использовалась как source of truth. Новый деплой (`vpn-gateway`) собирался из исходников с исправлениями.

## Fixes Applied

### 1. DNS upstream port (CRITICAL)
`src/config.rs` и `config/gateway.json`: дефолт изменён с `10.13.13.1:53` на `10.13.13.1:5353`.

**Причина**: Unbound слушает на порту 5353 (порт 53 занят `systemd-resolved` на хосте). UDP relay отправлял DNS-запросы в `systemd-resolved` вместо Unbound — DNS не работал.

### 2. Unbound interface binding
`scripts/entrypoint-simple.sh`: при генерации `unbound.conf` изменено `interface: 0.0.0.0` → `interface: 10.13.13.1`.

**Причина**: Unbound на `0.0.0.0` слушал публично через eth0, что небезопасно.

### 3. procps package in Dockerfile
Добавлен пакет `procps` в runtime образ.

**Причина**: `pgrep -x unbound` в entrypoint падало с `pgrep: not found`.

### 4. curl package in Dockerfile
Добавлен пакет `curl` в runtime образ.

**Причина**: Необходим для автоматического скачивания GeoIP базы.

### 5. GeoIP auto-download
В `scripts/entrypoint-simple.sh` добавлена секция:
- Скачивание `GeoLite2-City.mmdb` при первом запуске (если файл отсутствует)
- Обновление если файл старше 7 дней
- Фоновый цикл еженедельного обновления

Источник: `https://github.com/P3TERX/GeoLite.mmdb/raw/download/GeoLite2-City.mmdb`

### 6. .gitignore — WireGuard private keys
Добавлены исключения:
```
/data/wg0.conf
/data/wg/peer*/
/data/wg/templates/
/data/peer*.conf
```

**Причина**: Эти файлы содержат приватные ключи WireGuard и не должны попадать в публичный репозиторий.

### 7. config/gateway.json
Добавлены явные поля:
- `"dns_upstream": "10.13.13.1:5353"` — чтобы не зависеть от дефолта в коде
- `"geoip_path": "data/GeoLite2-City.mmdb"` — путь к автоскачанной базе

## Final Status
- Container: `vpn-gateway` — running
- WireGuard: `wg0` at `10.13.13.1/24`
- Unbound DNS: `10.13.13.1:5353` ✓
- GeoIP: loaded from `data/GeoLite2-City.mmdb` ✓
- Proxy pool: ~4000 proxies, ~10 TLS-clean
- API: `http://10.13.13.1:8080` (WireGuard) + `http://127.0.0.1:8080` (localhost)
- External API: BLOCKED

## Files Modified
- `src/config.rs` — dns_upstream default → 5353
- `scripts/entrypoint-simple.sh` — Unbound interface, GeoIP auto-download, procps check
- `Dockerfile` — added curl, procps
- `config/gateway.json` — added dns_upstream, geoip_path
- `.gitignore` — added WireGuard key exclusions

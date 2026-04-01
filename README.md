# VPN Gateway

Transparent TCP proxy gateway с динамическим пулом free прокси, WireGuard VPN и защитой от DNS-утечек.

## Что это

Rust-приложение, которое проксирует TCP-трафик через пул бесплатных прокси (1000+ из публичных списков), автоматически проверяя их работоспособность и выбирая лучшие по latency. UDP-трафик идёт напрямую через WireGuard VPN.

## Быстрый старт

```bash
# Локальная разработка
make build && make run

# Docker (локальная сеть)
make docker-local-up

# Docker (VPS с WireGuard + net-manager)
make docker-full-up
```

## Статус

| Компонент | Статус |
|-----------|--------|
| Компиляция | ✅ Работает, 0 warnings |
| Тесты | ✅ 72/72 |
| API | ✅ :8080 |
| Proxy pool | ✅ :1080, 2-stage verification |
| UDP relay | ✅ :1081, Unbound DNS |
| GeoIP | ✅ API + lazy lookup |
| Sticky Sessions | ✅ TTL-based |
| Connection Pool | ✅ (отключаемый) |
| WireGuard | ✅ Auto-detect IP |
| Docker | ✅ Протестировано |
| net-manager | ✅ UPnP + DHCP + config gen |
| Config hot-reload | ✅ notify crate |
| Weighted proxy selection | ✅ Top-N random |
| Idle timeout | ✅ 300s на relay |
| Source retry + rate limit | ✅ |

## Makefile

```bash
make help              # Справка
make build             # Сборка (cargo build --release)
make test              # Тесты
make run               # Локальный запуск
make clean             # Очистка
make geoip-update      # Скачать GeoLite2-City (~68MB)
make geoip-update-dbip # Скачать DB-IP City Lite (~19MB)
make docker-up         # Docker (VPS режим)
make docker-down       # Остановить VPS
make docker-local-up   # Docker (локальная сеть)
make docker-local-down # Остановить локальный
make docker-full-up    # Полный стек (VPS + net-manager + UPnP)
make docker-full-down  # Остановить полный стек
make docker-dev-up     # Dev режим (без macvlan)
make docker-dev-down   # Остановить dev
make wg-keygen         # Генерация WireGuard ключей
make wg-show-configs   # Показать конфиги клиентов
make status            # Статус контейнеров + proxy count
make backup            # Бэкап state и конфигов
make update            # Pull + rebuild
make client            # QR код WireGuard
make shell             # Shell в контейнере
make test-connection   # Тест через прокси
```

## WireGuard

```bash
# Генерация ключей (автоопределение IP)
./scripts/generate_wg_keys.sh peer1

# Несколько пиров
./scripts/generate_wg_keys.sh --peers 3

# Только ключи, без конфигов
./scripts/generate_wg_keys.sh --no-config peer1
```

## API Endpoints

```
GET  /health              → {"status":"ok","total_proxies":N,...}
GET  /api/metrics         → JSON метрики
GET  /metrics             → Prometheus формат
GET  /api/proxies         → [{host,port,latency_ms,...},...]
POST /api/proxy/add       → {"host":"1.2.3.4","port":8080}
POST /api/proxy/ban/:key  → забанить прокси
POST /api/proxy/unban/:key → разбанить прокси
GET  /api/network-status  → LAN/WAN IP, UPnP статус (от net-manager)
GET  /api/wg/peers        → список WireGuard пиров
```

## GeoIP

```bash
# API lookup
curl https://geo.wp-statistics.com/8.8.8.8?format=json

# Скачать базу локально
make geoip-update        # GeoLite2-City (~68MB)
make geoip-update-dbip   # DB-IP City Lite (~19MB)
```

## Конфигурация

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
11 публичных источников (HTTP + SOCKS5), обновление каждые 5 минут.

## Архитектура

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
│  UDP Relay (:1081) — DNS и UDP                      │
│  Web API (:8080) — мониторинг                        │
│  net-manager (:8088) — UPnP + DHCP + QR             │
└─────────────────────────────────────────────────────┘
```

## Технологии

- **Rust** (Edition 2021), **Tokio** async runtime
- **DashMap** — lock-free concurrent state
- **reqwest** (rustls-tls) — HTTP client
- **Axum** — Web API
- **tracing** — логирование
- **nix** — SO_ORIGINAL_DST (Linux)
- **notify** — config file watching
- **rand** — weighted random proxy selection
- **Python** (net-manager) — UPnP, DHCP, QR generation

## Требования к серверу

- **VPS**: 1 vCPU, 512 MB RAM, 10 GB диск
- **OS**: Ubuntu 22.04+ или Debian 12+
- **Открытые порты**: UDP 51820 (WireGuard)
- **Docker**: 24.0+ и Docker Compose v2

## Troubleshooting

| Ошибка | Решение |
|--------|---------|
| `nix` не компилируется | Используйте Docker |
| NET_ADMIN недоступен | Требуется `--cap-add=NET_ADMIN` |
| Port already in use | Измените порт в config/gateway.json |
| WireGuard не стартует | Проверьте wg-quick и wg0.conf |
| DNS утечки | Проверьте iptables REDIRECT + Unbound |
| Прокси не загружаются | Контейнер без интернета — fallback на state.json |

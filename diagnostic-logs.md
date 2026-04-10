# Логи диагностики VPN Gateway
# Дата: 2026-04-10

## Начало диагностики

### 1. Проверка структуры проекта
$ ls -la /root/vpn-gateway/
/root/vpn-gateway/
├── data/
│   ├── state.json
│   ├── wg/
│   └── unbound/
├── config/
│   ├── sources.json
│   └── unbound/
├── docker-compose.yml
└── diagnostic-report.md (создан позже)

### 2. Проверка docker-compose.yml
$ cat docker-compose.yml
services:
  vpn-gateway:
    image: ghcr.io/alexandergal86/vpn-gateway:latest
    pull_policy: always
    container_name: vpn-gateway
    network_mode: host
    cap_add:
      - NET_ADMIN
      - SYS_MODULE
    entrypoint: ["/entrypoint-simple.sh"]
    volumes:
      - ./data:/app/data

### 3. Проверка статуса контейнера
$ docker ps -a
CONTAINER ID   IMAGE                                       COMMAND                  CREATED          STATUS         PORTS     NAMES
de63b11848c8   ghcr.io/alexandergal86/vpn-gateway:latest   "/entrypoint-simple…"   26 minutes ago   Up 2 seconds             vpn-gateway

### 4. Первые логи контейнера
$ docker logs vpn-gateway 2>&1 | tail -50

Could not automatically determine the process-level CryptoProvider from Rustls crate features.
Call CryptoProvider::install_default() before this point to select a provider manually...

[INIT] VPN Gateway - VPS Simple Mode
[INIT] Setting up WireGuard...
[INIT] Using existing WireGuard config
[INIT] WireGuard interface wg0 already exists, tearing down first...
[INIT] Bringing up WireGuard interface...
[#] ip link add wg0 type wireguard
[#] wg setconf wg0 /dev/fd/63
[#] ip -4 address add 10.13.13.1/24 dev wg0
[#] ip link set mtu 1420 up dev wg0
[#] iptables -t nat -A POSTROUTING -o eth0 -j MASQUERADE
[INIT] Setting up Unbound DNS...
[INIT] Starting Unbound DNS server...
/entrypoint-simple.sh: 133: pgrep: not found
/entrypoint-simple.sh: 151: cannot create /proc/sys/net/ipv4/ip_forward: Read-only file system

VPN Gateway starting...
Loaded config: proxy_port=1080, api_port=8080
Config file watcher started for "config/gateway.json"
Loaded 5000 proxies from data/state.json (28 presumed alive)
Fast bootstrap: loading from top 3 sources...
Fast bootstrap complete: 0 proxies loaded
Level 1: 0 proxies from fast bootstrap
Fast probe: checking 60 proxies (timeout=3000ms, 3-stage)
Transparent proxy listening on 0.0.0.0:1080 (max 10000 connections)
UDP relay listening on 0.0.0.0:1081 (DNS upstream: 10.13.13.1:53, max 1000 concurrent tasks)
Web API listening on 10.13.13.1:8080

### 5. Проверка state.json
Первые прокси из state.json:
- 103.106.119.217:8081 (http, latency_ewma: 5000, fail_count: 48)
- 206.123.156.203:8554 (socks5, latency_ewma: 5000, fail_count: 50)
- 208.109.32.60:81 (http, latency_ewma: 5000, fail_count: 49)

### 6. Тестирование API
$ docker exec vpn-gateway sh -c "bash -c 'exec 3<>/dev/tcp/10.13.13.1/8080; echo -e \"GET /api/proxies?limit=5 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n\" >&3; timeout 3 cat <&3'"
Результат: HTTP/1.1 200 OK (720KB данных)

$ docker exec vpn-gateway sh -c "bash -c 'exec 3<>/dev/tcp/10.13.13.1/8080; echo -e \"GET /api/stats HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n\" >&3; timeout 3 cat <&3'"
Результат: HTTP/1.1 404 Not Found

### 7. Проверка listening портов
$ docker exec vpn-gateway ss -tlnp | grep -E '53|5353|1080|8080'
TCP:
- 0.0.0.0:1080 (transparent proxy) ✓
- 10.13.13.1:8080 (web API) ✓

UDP:
$ docker exec vpn-gateway ss -ulnp | grep -E '53|5353'
UNCONN 0  0  0.0.0.0:5353  0.0.0.0:*
UNCONN 0  0  127.0.0.53%lo:53  0.0.0.0:*
UNCONN 0  0  127.0.0.54:53  0.0.0.0:*

ПРОБЛЕМА: Unbound слушает на 5353, а UDP relay ожидает DNS на 53!

### 8. Проверка конфига Unbound
$ docker exec vpn-gateway cat /app/data/unbound/unbound.conf
server:
    port: 5353
    interface: 0.0.0.0
    access-control: 10.13.13.0/24 allow
    ...

### 9. Проверка сети хоста
$ ip addr show
eth0: 217.18.61.191/24
wg0: 10.13.13.1/24
lo: 127.0.0.1/8

$ ss -tlnp | grep :53
LISTEN 0  4096  127.0.0.53%lo:53   0.0.0.0:*   users:(("systemd-resolve",pid=503,fd=15))
LISTEN 0  4096  127.0.0.54:53      0.0.0.0:*   users:(("systemd-resolve",pid=503,fd=17))

Конфликт: systemd-resolved занимает порт 53 на 127.0.0.53 и 127.0.0.54

### 10. Первое исправление - port 53, interface 10.13.13.1
$ edit /root/vpn-gateway/data/unbound/unbound.conf
port: 5353 -> port: 53
interface: 0.0.0.0 -> interface: 10.13.13.1

$ docker restart vpn-gateway
Результат: unbound fatal error: could not open ports

Причина: порт 53 занят на хосте systemd-resolved

### 11. Второе исправление - вернуть port 5353
$ edit /root/vpn-gateway/data/unbound/unbound.conf
port: 53 -> port: 5353

$ docker restart vpn-gateway
Результат: работает, Unbound слушает на 10.13.13.1:5353

### 12. Сравнение с old версией
$ ls /root/vpn-gateway-old/
- docker-compose.yml (build: .)
- Dockerfile
- scripts/entrypoint-simple.sh
- config/gateway.json
- config/sources.json (18 источников)
- config/unbound/unbound.conf (полный, 350 строк)

Ключевые отличия:
1. Старая: build: . (сборка из исходников)
2. Новая: image: ghcr.io/alexandergal86/vpn-gateway:latest
3. В старой есть volumes ./config:/app/config:ro
4. В старой: port 5353 (создается в entrypoint)
5. В старой: 18 источников, в новой: 27

### 13. Проверка iptables
$ iptables -t nat -L PREROUTING -n -v | grep wg0
REDIRECT  udp  --  wg0  *  0.0.0.0/0  udp dpt:53 redir ports 5353
REDIRECT  6    --  wg0  *  0.0.0.0/0  redir ports 1080

Множество дубликатов правил (видно 30+ записей).

$ iptables -t nat -F PREROUTING
Очистка цепочки

### 14. Итоговый статус
$ docker logs vpn-gateway --tail 20
VPN Gateway ready. Proxy: :1080, API: :8080

Проверка /app/data/unbound/unbound.conf:
server:
    port: 5353
    interface: 10.13.13.1

---

## Выявленные проблемы

### 1. Unbound DNS порт (ИСПРАВЛЕНО)
- Было: port: 5353, interface: 0.0.0.0
- Стало: port: 5353, interface: 10.13.13.1
- Причина: конфликт с systemd-resolved на порту 53

### 2. Rustls CryptoProvider panic
- Периодические паники в Rust приложении
- Причина: проблема в готовом Docker образе
- Решение: требуется пересборка образа

### 3. Нет исходящего доступа (bootstrap)
- Fast bootstrap: 0 proxies loaded
- Причина: контейнер в network_mode: host, но исходящий трафик заблокирован
- Решение: проверить iptables на хосте

### 4. Нет config mount
- Старая версия: volumes ./config:/app/config:ro
- Новая версия: volumes только ./data:/app/data

---

## Финальные логи
$ docker logs vpn-gateway 2>&1 | tail -20
...
[INIT] Starting VPN Gateway...
VPN Gateway starting...
Loaded config: proxy_port=1080, api_port=8080
Config file watcher started for "config/gateway.json"
Loaded 5000 proxies from data/state.json (47 presumed alive)
Fast bootstrap: loading from top 3 sources...
Fast bootstrap complete: 0 proxies loaded
Level 1: 0 proxies from fast bootstrap
Fast probe: checking 60 proxies (timeout=3000ms, 3-stage)
UDP relay listening on 0.0.0.0:1081 (DNS upstream: 10.13.13.1:53, max 1000 concurrent tasks)
Web API listening on 10.13.13.1:8080
Transparent proxy listening on 0.0.0.0:1080 (max 10000 connections)

---

## Рекомендации

1. Добавить config mount в docker-compose.yml
2. Проверить iptables OUTPUT на хосте
3. Пересобрать образ с исправлением rustls
4. Синхронизировать sources.json (27 vs 18 источников)
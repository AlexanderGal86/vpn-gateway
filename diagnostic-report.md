# Диагностика VPN Gateway - Отчет

## Дата диагностики: 2026-04-10

---

## 1. Конфигурация системы

### docker-compose.yml (ТЕКУЩАЯ ВЕРСИЯ)
```yaml
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
    environment:
      - RUST_LOG=${LOG_LEVEL:-info}
      - WG_SERVER_URL=${WG_SERVER_URL:-auto}
      - WG_PORT=${WG_PORT:-51820}
      - WG_PEERS=${WG_PEERS:-2}
    ulimits:
      nofile: 1048576
    restart: unless-stopped
```

---

## 2. СРАВНЕНИЕ ВЕРСИЙ (vpn-gateway-old vs текущая)

### Источник образа
| Аспект | Старая (vpn-gateway-old) | Новая (текущая) |
|--------|---------------------------|-----------------|
| Тип | `build: .` - собирается из исходников | `image: ghcr.io/alexandergal86/vpn-gateway:latest` - готовый образ |
| Dockerfile | Есть в проекте | Не в проекте (из registry) |
| Entrypoint | scripts/entrypoint-simple.sh | /entrypoint-simple.sh (в образе) |

### volumes
| Аспект | Старая | Новая |
|--------|--------|-------|
| data | `./data:/app/data` | `./data:/app/data` |
| config | `./config:/app/config:ro` | ❌ **Отсутствует!** |

**Проблема:** В новой версии конфиг не монтируется из host, а берется из образа. Это может привести к рассинхронизации.

**Решение:** Добавить в docker-compose.yml:
```yaml
volumes:
  - ./data:/app/data
  - ./config:/app/config:ro
```

### Unbound конфиг
| Аспект | Старая (entrypoint, создается при старте) | Новая (data/unbound/unbound.conf) |
|--------|-------------------------------|----------------------------------|
| Port | 5353 | 5353 (согласовано с iptables) |
| Interface | 0.0.0.0 | 10.13.13.1 (избегает конфликта с systemd-resolved) |

**Примечание:** В старой версии Unbound конфиг создается в entrypoint.sh при запуске контейнера. В новой версии конфиг читается из `/app/data/unbound/unbound.conf`.

### iptables правила redirect (внутри контейнера)
| Аспект | Старая | Новая |
|--------|--------|-------|
| DNS redirect | `--to-port 5353` | `--to-port 5353` ✅ |
| TCP redirect | `--to-port 1080` | `--to-port 1080` ✅ |

### sources.json
| Аспект | Старая | Новая |
|--------|--------|-------|
| Количество источников | 18 | 27 |
| max_proxies | 10000 | 10000 |

### gateway.json
| Аспект | Старая | Новая |
|--------|--------|-------|
| Поля | gateway_port, api_port, udp_port, max_proxies, max_connections, health_check_interval, source_update_interval, exclude_countries | Только базовые поля, без расширенных настроек пула соединений |
| preferred_countries | Нет | Нет (не реализовано) |

---

## 3. Сетевая конфигурация хоста

### Интерфейсы
```
eth0: 217.18.61.191/24 (шлюз 217.18.61.1)
wg0: 10.13.13.1/24
lo: 127.0.0.1/8
```

### Таблица маршрутизации
```
default via 217.18.61.1 dev eth0 proto dhcp
10.13.13.0/24 dev wg0 proto kernel
217.18.61.0/24 dev eth0 proto kernel
```

### iptables - INPUT
```
ACCEPT     tcp  --  0.0.0.0/0   0.0.0.0/0   tcp dpt:8080
ACCEPT     udp  --  0.0.0.0/0   0.0.0.0/0   udp dpt:1081
ACCEPT     udp  --  0.0.0.0/0   0.0.0.0/0   udp dpt:1080
ACCEPT     udp  --  0.0.0.0/0   0.0.0.0/0   udp dpt:51820
```

### iptables -t nat- PREROUTING
```
REDIRECT   udp  --  wg0    *     0.0.0.0/0   udp dpt:53 redir ports 5353  ✅
REDIRECT   6    --  wg0    *     0.0.0.0/0   redir ports 1080           ✅
```

---

## 4. Состояние сервисов в контейнере

### Проверка listening портов
```
TCP:
- 0.0.0.0:1080 (transparent proxy) ✓
- 10.13.13.1:8080 (web API) ✓

UDP:
- 10.13.13.1:5353 (Unbound DNS) ✓ (согласовано с iptables)
```

### API endpoints тестирование
```
GET /api/proxies?limit=5 -> 200 OK (720KB данных)
GET /api/stats -> 404 Not Found
GET /api/status -> 404 Not Found
GET / -> 404 Not Found
```

---

## 5. Логи приложения

### Успешный запуск
```
[INIT] VPN Gateway - VPS Simple Mode
[INIT] WireGuard interface wg0 already exists
[INIT] WireGuard up at 10.13.13.1/24
[INIT] Unbound DNS server started
[INIT] iptables configured

VPN Gateway starting...
Loaded config: proxy_port=1080, api_port=8080
Loaded 5000 proxies from data/state.json (46 presumed alive)
Transparent proxy listening on 0.0.0.0:1080
Web API listening on 10.13.13.1:8080
UDP relay listening on 0.0.0.0:1081 (DNS upstream: 10.13.13.1:53)

VPN Gateway ready. Proxy: :1080, API: :8080
```

### Проблемы в логах
```
1. Fast bootstrap: 0 proxies loaded (проблема с исходящим HTTPS)
2. "Failed to fetch https://www.proxy-list.download/api/v1/get?type=http"
3. Panics: "Could not automatically determine the process-level CryptoProvider"
```

---

## 6. Выявленные проблемы

### Проблема 1: Нет исходящего доступа к интернету из контейнера (bootstrap)

**Симптомы:**
- `Fast bootstrap complete: 0 proxies loaded`
- `Failed to fetch https://www.proxy-list.download/...`
- Нет curl/wget в контейнере для тестирования
- Исходящие TCP соединения не работают

**Причина:** Контейнер работает в network_mode: host, но исходящий трафик заблокирован (вероятно, iptables на host).

**Решение:**
```bash
# На host:
iptables -I OUTPUT -j ACCEPT
```

### Проблема 2: Rustls CryptoProvider panic

**Симптомы:**
```
thread 'tokio-rt-worker' panicked at rustls-0.23.37/src/crypto/mod.rs:249:14
Could not automatically determine the process-level CryptoProvider
```

**Причина:** Внутренняя проблема библиотеки rustls в готовом образе.

**Решение:** Требуется пересборка образа с правильной конфигурацией rustls features (использовать ring вместо aws-lc-rs или наоборот).

### Проблема 3: Нет mount config в docker-compose

**Симптомы:**
- Новая версия: volumes только `./data:/app/data`
- Старая версия: volumes `./data:/app/data` + `./config:/app/config:ro`

**Решение:**
```yaml
volumes:
  - ./data:/app/data
  - ./config:/app/config:ro
```

---

## 7. Файлы конфигурации

### /app/data/unbound/unbound.conf (текущий)
```conf
server:
    port: 5353
    interface: 10.13.13.1
    access-control: 10.13.13.0/24 allow
    access-control: 127.0.0.0/8 allow
    access-control: 0.0.0.0/0 allow
    hide-identity: yes
    hide-version: yes
    use-caps-for-id: yes
    prefetch: yes
    prefetch-key: yes
    minimal-responses: yes
    qname-minimisation: yes

remote-control:
    control-enable: no
```

**Примечание:** Port 5353 согласован с iptables правилами redirect внутри контейнера.

---

## 8. Команды для диагностики

### Проверка состояния контейнера
```bash
docker ps -a
docker logs vpn-gateway --tail 100
docker exec vpn-gateway ss -tlnp
docker exec vpn-gateway ss -ulnp
```

### Тестирование API
```bash
docker exec vpn-gateway bash -c 'exec 3<>/dev/tcp/10.13.13.1/8080; echo -e "GET /api/proxies?limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n" >&3; cat <&3'
```

### Тестирование прокси
```bash
curl -x http://localhost:1080 http://ifconfig.me
```

### Проверка сети хоста
```bash
ip route
iptables -L -n -v
iptables -t nat -L -n -v
curl --max-time 10 https://api.proxyscrape.com/v2/?request=getproxies
```

---

## 9. Рекомендуемые действия

### Срочные

1. **Добавить mount config в docker-compose.yml:**
   ```yaml
   volumes:
     - ./data:/app/data
     - ./config:/app/config:ro
   ```

2. **Проверить iptables на хосте:**
   ```bash
   iptables -I OUTPUT -j ACCEPT
   ```

### Долгосрочные

1. Пересобрать Docker образ с исправлением rustls
2. Добавить curl/wget в образ для диагностики
3. Настроить мониторинг прокси пула
4. Синхронизировать конфиг sources.json между версиями

---

## 10. Статус сервиса

| Компонент | Статус | Заметки |
|-----------|--------|---------|
| WireGuard (wg0) | ✅ Работает | 10.13.13.1/24 |
| Transparent Proxy (:1080) | ✅ Работает | Принимает соединения |
| Web API (:8080) | ✅ Работает | /api/proxies работает |
| Unbound DNS (:5353) | ✅ Работает | слушает 10.13.13.1:5353 |
| UDP Relay (:1081) | ✅ Работает | DNS upstream доступен |
| Прокси пул | ⚠️ Частично | 5000 в пуле, требуется проверка |
| Bootstrap источники | ❌ Не работают | Нет исходящего HTTPS |
| Rustls | ⚠️ Нестабилен | Периодические panics |

---

## 11. Различия в реализации между версиями

### Сборка образа
| Компонент | Старая версия | Новая версия |
|-----------|---------------|--------------|
| Base image | debian:bookworm-slim | ghcr.io/alexandergal86/vpn-gateway:latest |
| Rust toolchain | rust:latest (многослойная сборка) | Готовый бинарник |
| Размер образа | ~1GB (full rust) | Меньше (оптимизировано) |

### Entrypoint
| Аспект | Старая | Новая |
|--------|--------|-------|
| Unbound конфиг | Создается при старте в /app/data/unbound/unbound.conf | Читается из существующего файла |
| Port Unbound | 5353 | 5353 |

### Config mount
| Аспект | Старая | Новая |
|--------|--------|-------|
| config/ mount | Да (`./config:/app/config:ro`) | Нет |

---

## 12. Рекомендации по миграции

1. **Добавить config mount** в docker-compose.yml для согласования конфигов
2. **Синхронизировать sources.json** - в новой версии 27 источников против 18 в старой
3. **Проверить исходящий трафик** - проблема с bootstrap влияет на работу прокси пула
4. **Рассмотреть пересборку образа** - для исправления проблемы с rustls
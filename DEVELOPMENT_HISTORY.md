# VPN Gateway — История разработки

## Дата: 8 апреля 2026
## Сервер: 217.18.61.191 (VPS)

---

## Начальное состояние

Исходный код — полноценный Docker-стек с:
- WireGuard (linuxserver/wireguard)
- VPN Gateway (Rust прокси)
- Unbound (DNS)
- net-manager (Python, UPnP + DHCP)

**Проблема:** Архитектура с `network_mode: service:wireguard` не работала:
- Не было default route через VPN
- iptables kill-switch блокировал исходящий трафик
- Сложная сетевая изоляция

---

## Решение: VPS Simple Mode

Создана упрощённая архитектура с одним контейнером:

### Файлы созданы:
- `docker-compose-vps-simple.yml` — один контейнер, network_mode: host
- `scripts/entrypoint-simple.sh` — WireGuard + Unbound + Gateway + iptables

### Проблемы и решения:

#### 1. WireGuard ключи не совпадали
**Симптом:** Клиент постоянно посылал handshake, сервер не отвечал
**Причина:** При перезапуске контейнера создавались новые ключи, но старая конфигурация оставалась
**Решение:** Удалять старые ключи перед перезапуском контейнера, использовать единые ключи в volume

#### 2. Unbound не запускался
**Симптом:** DNS не работал на клиенте
**Причина:** Конфиг содержал неподдерживаемую опцию `fast-server-permissive`
**Решение:** Удалить эту опцию из unbound.conf

#### 3. Выбор страны (исключение RU)
**Проблема:** Не было функционала исключения стран
**Решение:** Добавлено в `src/config.rs` и `src/pool/state.rs`:
- Новое поле `exclude_countries: Vec<String>` в Config
- Фильтрация в `select_best()` по country
- Автоматическое применение при hot-reload конфига

---

## Текущая конфигурация

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
  "exclude_countries": ["RU"]
}
```

### iptables правила (в entrypoint-simple.sh)
- FORWARD ACCEPT для wg0 ↔ eth0
- NAT MASQUERADE для 10.13.13.0/24
- REDIRECT UDP:53 → 5353 (Unbound)
- REDIRECT TCP → 1080 (Gateway прокси)

---

## WireGuard пиры

| Peer | IP | Файл | QR-код |
|------|-----|------|--------|
| peer1 | 10.13.13.2 | data/wg/peer1/peer1.conf | peer1-qr.png |
| peer2 | 10.13.13.3 | data/wg/peer2/peer2.conf | peer2-qr.png |

---

## API эндпоинты

- `GET /health` — статус
- `GET /api/metrics` — метрики
- `GET /api/proxies` — список прокси

---

## Команды управления

```bash
# Пересборка
docker compose -f docker-compose-vps-simple.yml build

# Перезапуск
docker restart vpn-gateway

# Логи
docker logs vpn-gateway

# Проверка WireGuard
docker exec vpn-gateway wg show

# Проверка API
curl http://localhost:8080/health
```

---

## Известные ограничения

1. При перезапуске контейнера ключи генерируются заново (нужно сохранять volume)
2. GeoIP база загружается асинхронно — страны определяются не сразу
3. preferred_countries не реализован (только exclude_countries)

---

## Следующие шаги

- Добавить persistent storage для ключей (чтобы survive перезагрузки)
- Реализовать preferred_countries
- Добавить DDNS для динамического IP
# VPN Gateway — полный эксплуатационный мануал (RU)

Этот документ нужен, чтобы убрать «магическое мышление» вокруг проекта:
- что запускать,
- в каком режиме,
- какие переменные обязательны,
- какие есть подводные камни.

---

## 1) Два режима без двусмысленности

### Режим A: VPS (публичный IP)
Файл: `docker-compose.yml`

**Когда использовать**
- есть публичный IP или домен;
- не нужен UPnP;
- не нужна автогенерация LAN/WAN конфигов через sidecar.

**Что стартует**
- `wireguard`
- `vpn-gateway` (в namespace wireguard)
- `unbound`

**Команда**
```bash
make docker-up
```

---

### Режим B: VM/Home за NAT
Файл: `docker-compose-local.yml`

**Когда использовать**
- сервер дома/в локалке за роутером NAT;
- нужен UPnP-проброс;
- нужна веб-раздача клиентских конфигов/QR.

**Что добавляется**
- `net-manager` + `ext_net` (macvlan)

**Команда**
```bash
make docker-local-up
```

---

## 2) Источник правды по WireGuard peer'ам

`./data/wg` — это **источник правды** peer-ключей/конфигов (`wireguard` контейнер).

`net-manager` НЕ изменяет `./data/wg`, он монтирует его read-only:
- `./data/wg:/wg-config:ro`
- пишет производные артефакты в `./data/clients`.

Итого:
- пользователи/peers живут в `data/wg`;
- раздаваемые «удобные» конфиги/QR живут в `data/clients`.

---

## 3) Минимальная настройка `.env`

### Для VPS
Обязательно:
- `WG_SERVER_URL=<публичный IP или DNS>`
- `WG_PORT=51820` (или ваш)
- `WG_PEERS=<сколько initial peers генерировать>`

Опционально:
- `API_PORT=8080`

### Для NAT/VM
Обязательно дополнительно:
- `NET_INTERFACE=eth0` (или ваш uplink интерфейс)
- `LAN_SUBNET`, `LAN_GATEWAY`
- `MACVLAN_IP_RANGE` (вне DHCP-пула роутера)

Опционально:
- `DOCKER_HOST_IP` (если автоопределение LAN IP нестабильно)

---

## 4) Операционный runbook

### Старт/стоп
### Автоматизированный запуск (рекомендуется)
```bash
make env-init MODE=vps           # или home-vm / home-desktop
make up MODE=vps                 # внутри вызовет preflight
make status-all MODE=vps
```

Режимы:
- `vps`
- `home-vm` (локальный Linux/VM)
- `home-desktop` (Docker Desktop)

```bash
make docker-up           # VPS
make docker-down

make docker-local-up     # NAT
make docker-local-down
```

### Быстрая диагностика
```bash
curl -s http://localhost:8080/health
curl -s http://localhost:8080/api/metrics
curl -s http://localhost:8088/status   # только NAT-режим
```

### Где смотреть артефакты
- `data/wg/peerN/peerN.conf` — базовые peer конфиги
- `data/clients/peerN/*-lan.conf` / `*-wan.conf` — net-manager конфиги
- `data/clients/network-status.json` — актуальный сетевой статус

---

## 5) Подводные камни (важно)

1. **`WG_SERVER_URL=auto` на VPS** может дать не тот endpoint для клиентов.
   - Лучше всегда задавать явный IP/DNS.

2. **macvlan не работает на Docker Desktop (Windows/macOS)**.
   - Используйте dev override (`docker-compose-dev.yml`).

3. **UPnP может быть недоступен на роутере**.
   - Тогда порт-форвард настраивать вручную.

4. **Смена WAN IP**
   - в NAT-режиме net-manager это отслеживает и перевыпускает WAN-конфиги,
   - в VPS-режиме это ваша ответственность (обычно решается DDNS).

5. **Непрозрачная деградация при пустом proxy pool**
   - health API может быть живым, но прокси недоступны;
   - смотрите `/api/metrics` и число `available_proxies`.

6. **Права и capabilities**
   - `NET_ADMIN` обязателен для iptables/WireGuard логики.

7. **Смешивание режимов**
   - не запускайте одновременно VPS и NAT compose на одних и тех же портах.

---

## 6) Чеклист перед продом

- [ ] Выбран только один режим (VPS или NAT)
- [ ] Проверены порты: UDP `WG_PORT`, TCP `API_PORT`
- [ ] Для VPS задан явный `WG_SERVER_URL`
- [ ] Для NAT корректно выставлены `NET_INTERFACE`, `LAN_SUBNET`, `LAN_GATEWAY`, `MACVLAN_IP_RANGE`
- [ ] `curl /health` и `curl /api/metrics` отвечают
- [ ] Клиентский конфиг импортируется в WireGuard без ручных правок


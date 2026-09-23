# План интеграции «сетевой игры» в VoidLauncher

(исследование и архитектура MVP — см. `rooms-tailscale-report.md` и `rooms-tailscale-architecture.md`)

Статус: **РЕАЛИЗОВАНО**. Функция доступна в интерфейсе как «Сетевая игра»
(вкладка бокового меню). Код: `src-tauri/src/rooms/`, `src-tauri/src/commands/rooms.rs`,
`src/pages/Rooms.tsx`, `src/stores/roomStore.ts`.

---

## 1. Компоненты и где они встраиваются

Аудит показал: менеджер команд, события, логи игры, keyring-хранилище и
`is_allowed_download_host` уже дают всё, что нужно. Ни одной новой внешней
зависимости не требуется (reqwest, tokio, sha2, winreg, keyring уже в Cargo.toml).

| # | Компонент | Где живёт (Rust) | Точки интеграции в существующем коде |
|---|-----------|------------------|--------------------------------------|
| 1 | **TailscaleManager** | новый `rooms/tailscale.rs` | `download.rs` (хост-аллоулист `pkgs.tailscale.com`), `winreg` (чтение политик `HKLM\SOFTWARE\Policies\Tailscale`), `std::process` (msiexec, CLI) |
| 2 | **RoomStateManager** | новый `rooms/room_state.rs` | `events.rs` (`APP_HANDLE`, emit-хелперы), persist в `data_dir/rooms/rooms.json`, генерация RoomID |
| 3 | **PeerDiscovery** | новый `rooms/peer_discovery.rs` | парсинг `tailscale status --json` (serde, уже есть), tokio-задача с отменой (паттерн как в `playtime`/`launcher.rs`), `events` |
| 4 | **MinecraftConnector** | новый `rooms/minecraft_connector.rs` | `game_logs.rs` (текущая сессия лога — парсинг «Local game hosted on port N»), `launch.rs` (доп. аргументы `--server/--port`), `commands/launcher.rs` (`cmd_launch_game`) |
| 5 | **RoomUI** | фронтенд: новый `src/pages/Rooms.tsx` + `roomStore.ts` | `Sidebar.tsx` (пункт меню), `App.tsx` (маршрут), `useGameEvents.ts` (по образцу — `useRoomEvents.ts`), `lib/i18n` |
| 6 | **Внутренний модуль VoidLink** | новый `rooms/void_link.rs` | веб-мост «лаунчер↔лаунчер» по tailnet (см. п. 4 раздела ниже); занимает место удалённого `voidlink.rs` как внутренний модуль |

Решение по подключению фронтенда к бэкенду — уже устоявшийся в проекте паттерн:
команды `invoke('cmd_...')` + события `listen('...')` + zustand-сторы.

---

## 2. Поток Tailscale (один раз, потом авто)

1. **Проверка** `cmd_rooms_get_status` → есть ли `C:\Program Files\Tailscale\tailscale.exe`,
   работает ли служба, вошёл ли пользователь (`tailscale status --json` → `Self`).
2. **Установка** (если нет): `cmd_rooms_install` — скачать MSI (хост
   `https://pkgs.tailscale.com/stable/tailscale-setup-*-amd64.msi`, sha256 из
   `.sha256`, сверка как у живого теста), тихая установка
   `msiexec /i <msi> /qn TS_SERVICE_UI_PREFERENCES={"no"}...` (молчаливая).
3. **Вход** `cmd_rooms_login`: `tailscale login` — печатает URL вида
   `https://login.tailscale.com/a/...`; лаунчер показывает его (как код MS-входа),
   открывает в браузере через `tauri-plugin-opener`, поллит `tailscale status`
   до появления `Self`. Сам ключ авторизации лаунчер НЕ хранит — интерактивный вход.
4. **Tailscale auth key не используется** — в лаунчере нет и не будет секретов tailscale.

---

## 3. Команды и события (API frontend ↔ Rust)

### Команды (новый `commands/rooms.rs`, регистрация в `lib.rs`)
- `cmd_rooms_get_status` → `{ installed, service_running, logged_in, version, tailnet, login_name }`
- `cmd_rooms_install` → прогресс через события; `cmd_rooms_login` → `{ auth_url }`
- `cmd_rooms_create { name }` → `RoomInfo { room_id, role: Host }` (генерация вида `ABCD-1234`)
- `cmd_rooms_join { room_id }` → `RoomInfo { role: Guest, host_endpoint?, status }`
- `cmd_rooms_leave` → сброс роли (реюзабельная шара СОХРАНЯЕТСЯ)
- `cmd_rooms_get_peers` → `Vec<PeerInfo>`
- `cmd_rooms_get_state` → полный снимок (для переживания перезагрузки фронтенда)
- `cmd_rooms_revoke_guest { peer_key }` → открывает страницу админ-консоли (API нет)
- `cmd_launch_game` — расширяется необязательными `server_address` / `server_port`
  (обычный запуск не меняется; гость джоинит через тот же путь)

### События (добавление payload-структур в `events.rs`)
- `room_state_changed` — сменилась роль/статус комнаты
- `room_peer_update` — список гостей/хоста обновился (поллинг `status`)
- `room_minecraft_ready` — хост: мир открыт, endpoint `dns:port` готов
- `room_minecraft_pending` — статусы «ждём порт from лога» и т.п.
- Переиспользуются `log_message`, `game_started`, `launch_complete` как есть.

---

## 4. Внутренний модуль VoidLink: как гость узнаёт порт мира

Проблема: порт «Open to LAN» случаен и известен только стороне хоста, а backend-а нет.

Решение — лёгкий веб-мост между двумя лаунчерами поверх tailnet:

- **Хост**: лаунчер держит мини-HTTP-листенер на tailnet-IP хоста, порт `48888`
  (bind на `100.x.y.z`, НЕ `0.0.0.0`; фаервол `Tailscale-In`, Domain/Private,
  пропускает входящие к любому процессу — проверено живым тестом).
  Отвечает на `GET /room/status?code=<RoomID>` → `{ room_id, mc_port|null, player }`.
- **Гость**: PeerDiscovery уже нашёл хост; poll `http://<host-ip>:48888/room/status?code=<RoomID>`
  → как только `mc_port != null` → «Присоединиться» становится активной.
- **Fallback**: кнопка «Подключиться вручную» — копирование строки `хост:порт` в чат.
  (изоляция комнат на одном ПК — не обещаем; RoomID = прикладной пароль моста.)

Мини-листенер реализуется на `tokio::TcpListener` + ручной разбор GET (~40 строк),
без новых зависимостей. При необходимости позже заменяется на `tiny_http`/`axum`.

---

## 5. MinecraftConnector (абстрактный слой)

- Трейт `MinecraftDiscovery { detect_mc_port(), discover_host_endpoint(), async probe_tcp() }`.
- **Реализация v1 (MVP)**: парсинг игрового лога (текущая сессия из `game_logs.rs`,
  строка `Local game hosted on port N` / `Started LAN server, port N`); endpoint
  хоста из tailnet-IP/DNS хоста (PeerDiscovery); TCP-probe на `ip:port`.
- **Будущие реализации** (в тот же трейт, UI не меняется): `--quickPlayMultiplayer`
  (1.19+), встроенный сервер (Fabric/Forge `ServerLifecycle`), выделенные серверы.
- **Присоединение**: хост открывает мир как обычно (Open to LAN). Гостевой запуск —
  тот же `launch_minecraft` + доп.аргументы `--server <ip> --port <port>`
  (legacy). Для 1.19+ автоматический `--server` НЕ работает (аргументы перенесены
  в quickPlayMultiplayer) — требуется compatibility-тест на целевых версиях;
  до подтверждения MVP ведёт гостя на экран «Direct Connect/Multiplayer».

---

## 6. Данные на диске

- `data_dir/rooms/rooms.json` — роль (host/guest/none), RoomID, host-параметры
  (peer_key, tailnet DNS/IP), кэш endpoint, список гостей/комнат. Ре-юзабельная
  шара между сессиями сохраняется (revoke только при закрытии доступа).
- Политики Tailscale — в реестре (`HKLM\SOFTWARE\Policies\Tailscale`), состояние
  и идентификация — в самом Tailscale (`C:\ProgramData\Tailscale`). Лаунчер их не дублирует.
- Изменяются `lib.rs` (AppState += `room_state`), `tauri.conf.json` — правок CSP
  не ожидается (внешние URL открываются внешним браузером через opener).

---

## 7. Жизненный цикл и sequence diagrams (текстовые)

### A. Первый запуск HOST
```
UI(Rooms) → cmd_rooms_get_status
RoomStateManager → TailscaleManager: exe? служба? Self в status --json?
→ {installed:false, logged_in:false} → UI: мастер «Настроить комнаты»
UI → cmd_rooms_install → скачать MSI (alлоулист) → sha256 → msiexec /qn
   → события room_install_progress → UI-прогресс
UI → cmd_rooms_login → tailscale login → {auth_url} → UI открывает браузер
   → полл tailscale status (tokio) → Self появился → {logged_in:true}
RoomStateManager: persist readiness → события room_state_changed
UI: «Готово». Кнопка «Создать комнату».
```

### B. Первый запуск GUEST
```
UI(Rooms) → cmd_rooms_get_status → мастер установки/входа (как A, без «Создать»)
После входа: гость должен принять Machine Sharing хоста (один раз):
  хост создаёт инвайт в admin console (вручную) → URL → гость открывает в браузере
  → принимает. После этого у гостя в tailnet появляется машина хоста.
UI: статус «Готово. Введите код комнаты».
```

### C. Создание комнаты (HOST)
```
UI → cmd_rooms_create {name} → RoomStateManager: генерирует RoomID "ABCD-1234"
  → role=Host → persist rooms.json → событие room_state_changed
  → (экспериментально) tailscale set --hostname "<roomid>-host"
UI: показывает RoomID + инструкцию «как пригласить друга» (admin console,
Machine Sharing, инвайт-URL) — кнопки «Копировать RoomID» / «Открыть справку».
PeerDiscovery: периодический поллинг status --json (фоновый tokio-задачa)
```

### D. Подключение к комнате (GUEST)
```
UI → cmd_rooms_join {room_id:"ABCD-1234"} → RoomStateManager: role=Guest,
  persist → событие room_state_changed
PeerDiscovery (гость): поллинг status --json → ищем shared-peer (машина хоста):
  - по hostname-префиксу комнаты (если хост включил) ЛИБО
  - единственная машина, к которой у нас прямой доступ по Machine Sharing
  → host_ip, host_dns → persist → событие room_peer_update: «Хост найден 100.x.y.z»
Пока хоста нет (офлайн/не принята шара) → статус «Хост пока недоступен»,
  комнатная изоляция = прикладной уровень, ничего не обещаем.
```

### E. HOST запускает Minecraft LAN-мир
```
UI(Комнаты) → выбор инстанса → cmd_launch_game (обычный путь; close_on_launch
  в режиме комнаты отключён — лаунчер остаётся открытым)
Minecraft: меню → «Открыть для LAN» → в лог: "Local game hosted on port N"
MinecraftConnector (host): читает текущую игровую сессию (game_logs) →
  детектит порт N → endpoint = "<host_dns>:N" → событие room_minecraft_ready
UI(host): «Мир открыт: abcd1234-host.tailnet.ts.net:45565» + «Копировать».
VoidLink (host): теперь на /room/status отдаёт mc_port=N.
```

### F. GUEST присоединяется
```
PeerDiscovery → UI(гость): кнопка «Присоединиться» активна
UI → cmd_launch_game {instance_name, server_address:<host_dns>, server_port:N}
commands/launcher.rs → launch.rs: добавляет --server/--port (для версий 1.19-;
  для новее — инструкция/quickPlayMultiplayer после compatibility-теста)
→ game_started / launch_complete (существующие события работают как обычно)
Игровой трафик — только гостевой клиент ↔ host:port через tailnet (direct/DERP).
```

### G. Выход из комнаты (GUEST)
```
UI → cmd_rooms_leave → RoomStateManager: role=None, room_id=null → persist
  → событие room_state_changed → PeerDiscovery: поллинг остановлен
Шара (доступ к машине хоста) СОХРАНЯЕТСЯ — revoke не выполняется.
```

### H. Закрытие доступа гостя (HOST)
```
UI(host) → «Удалить гостя» → cmd_rooms_revoke_guest {peer_key}
TailscaleManager: открыть в браузере страницу машины/шары в admin console
  (публичного API share-инвайтов нет — подтверждено исследованием)
UI: инструкция «в браузере нажмите Revoke». Peer исчезает из status у гостя
  → room_peer_update у гостя: «Хост недоступен».
```

---

## 8. Список файлов

### Новые (Rust)
- `src-tauri/src/rooms/mod.rs` — типы (RoomRole, RoomInfo, PeerInfo, RoomState) + mod-декларации
- `src-tauri/src/rooms/tailscale.rs` — TailscaleManager (detect/install/login/status/policy readback/set hostname)
- `src-tauri/src/rooms/room_state.rs` — RoomStateManager (create/join/leave, RoomID, persist rooms.json)
- `src-tauri/src/rooms/peer_discovery.rs` — PeerDiscovery (поллинг status --json, host-resolution, tokio-задача)
- `src-tauri/src/rooms/void_link.rs` — внутренний VoidLink-модуль (mini-HTTP host + client poll, код-чекаут RoomID)
- `src-tauri/src/rooms/minecraft_connector.rs` — MinecraftConnector + трейт MinecraftDiscovery (лог-парсер порта, probe, join-args)

### Изменяемые (Rust)
- `src-tauri/src/lib.rs` — `mod rooms;`, `AppState.room_state`, регистрация команд
- `src-tauri/src/commands/mod.rs` — `pub mod rooms;`
- `src-tauri/src/events.rs` — payload-структуры комнат + emit-хелперы
- `src-tauri/src/launch.rs` — необязательные `--server/--port` в `launch_minecraft`
- `src-tauri/src/commands/launcher.rs` — passthrough `server_address/server_port`
- `src-tauri/src/download.rs` — аллоулист `pkgs.tailscale.com` в `is_host_allowed`
- `src-tauri/src/config.rs` — хелпер `rooms_dir()` (минимальное)

### Новые (frontend)
- `src/pages/Rooms.tsx`
- `src/stores/roomStore.ts`
- `src/hooks/useRoomEvents.ts`
- `src/components/rooms/RoomSetupView.tsx`, `RoomHostView.tsx`, `RoomGuestView.tsx`,
  `RoomShareHelpModal.tsx` + мелкие (`RoomStatusBadge`, `RoomJoinButton`)

### Изменяемые (frontend)
- `src/components/Sidebar.tsx` — пункт меню «Комнаты»
- `src/App.tsx` — маршрут `rooms`
- `src/lib/i18n/en.ts`, `src/lib/i18n/ru.ts` — строки (на «вы», по правилам UI)

### Cargo.toml / tauri.conf.json
- Новых зависимостей нет (reqwest/tokio/sha2/winreg/keyring уже есть).
- `tauri.conf.json`: правки не ожидаются (внешние URL — через opener, внешний браузер).

---

## 9. Политика секретов (обязательно)

- Никаких Tailscale auth-ключей и файлов `*.key` в проекте. Вход — интерактивный
  (device flow через браузер); Tailscale хранит свой стейт у себя в `ProgramData`.
- Login-URL содержит одноразовый токен — показывать ТОЛЬКО в UI, в логи/команды
  не печатать (правило redaction, как `--accessToken` в `launch.rs`).
- MSI-даунлоад — через существующий аллоулист + проверку sha256 (как в живом тесте).
- RoomID — прикладная величина (пароль VoidLink-моста), в git/логи не попадает.

---

## 10. Ограничения и что осталось проверить (не блокеры проектирования)

- Авто-johnin через `--server/--port` работает для версий 1.12.x–1.18.x; для
  1.19+ нужен compatibility-тест `--quickPlayMultiplayer` (записан в отчёт).
- Cross-network проверка жизненного цикла share (direct vs DERP) — нужен второй участник.
- revoke гостя и создание инвайтов — вручную в admin console (API нет) → UI справка.
- Нет изоляции комнат на одном ПК (прикладной уровень) — не показываем как фичу.
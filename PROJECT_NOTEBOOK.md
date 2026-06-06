# PROJECT_NOTEBOOK.md — VoidLauncher

> **Единый "Живой Блокнот Проекта"** — мгновенный контекст для любого ИИ, который подключится к проекту. Только то, что **есть в коде прямо сейчас**. Никаких планов, гипотез и wishlist'ов.

---

## ⚠️ PRODUCTION STATUS — ВАЖНО ДЛЯ ЛЮБОГО ИИ

**VoidLauncher v0.1.0 — это СТАБИЛЬНЫЙ РЕЛИЗ, который прямо сейчас используют реальные люди** (личный лаунчер автора для друзей).

**Правила для любых изменений:**

1. **main = production.** Ветка `main` находится в активной эксплуатации. Не делай в ней breaking changes «по приколу».
2. **Любое изменение протокола подписи/апдейтера, capabilities, CSP, auth-flow или структуры данных на диске — потенциально ломает установленные у пользователей копии.** Если такое изменение необходимо, опиши риск пользователю и согласуй перед коммитом.
3. **Секреты репозитория (`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) — это production-криптография.** Потеря приватного ключа = невозможность подписать следующий релиз = пользователи не получат автообновления. Не трогай workflow подписи без подтверждения от пользователя.
4. **Версия в `package.json`, `src-tauri/tauri.conf.json` и `src-tauri/Cargo.toml` должна совпадать.** Рассинхрон → пользователи увидят `latest.json` с одной версией, а бинарь — с другой.
5. **Локализация.** Все user-facing строки — через `t()` / `useT()`. Хардкод EN-текста в JSX = регрессия.
6. **CSP / capabilities / allowlist хостов** — это защита от SSRF, path traversal, mixed-content. Не ослабляй, не добавляй wildcard'ы.
7. **Перед тегами `v*`:** подпись релиза работает только при наличии обоих секретов и корректного `latest.json` в `main`. CI стартует автоматически — `git push --delete` + `git push` нового тега = новый релиз. Не делай «случайных» тегов.

---

## 1. Общая информация

| Поле | Значение |
|---|---|
| **Название** | VoidLauncher |
| **Тип** | Кастомный лаунчер Minecraft для Windows |
| **Версия** | `0.1.0` (см. `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`) |
| **Репозиторий** | https://github.com/veb898-cyber/VoidLauncher (public, ветка `main`) |
| **Платформа** | Windows 10/11 (x86_64), NSIS-установщик, per-user install |
| **UI языки** | English (по умолчанию) + Русский (переключаемый в Settings) |

**Глобальная цель:** удобный, быстрый лаунчер Minecraft с модами (Fabric/Quilt/Forge/NeoForge), мод-браузером Modrinth, аккаунтами Microsoft / Ely.by / Offline, автообновлениями без UAC и тихим CI/CD.

---

## 2. Технологический стек

### Backend (Tauri / Rust)

| Компонент | Версия | Назначение |
|---|---|---|
| `tauri` | `2` | Главный фреймворк |
| `tauri-build` | `2` | Build-time кодогенерация |
| `tauri-plugin-opener` | `2` | Открытие URL в системном браузере |
| `tauri-plugin-shell` | `2` | Запуск внешних команд |
| `tauri-plugin-dialog` | `2` | Нативные диалоги выбора файлов |
| `tauri-plugin-process` | `2` | `relaunch()` для автообновления |
| `tauri-plugin-fs` | `2` | Чтение/запись файловой системы (scoped) |
| `tauri-plugin-updater` | `2` | Автообновления + проверка подписи minisign |
| `serde` / `serde_json` | `1` | Сериализация |
| `reqwest` | `0.12` (`json`, `stream`) | HTTP-клиент (Mojang, Modrinth, CurseForge, Ely.by) |
| `tokio` | `1` (`full`) | Async runtime |
| `zip` | `2` | Распаковка jar/zip |
| `chrono` | `0.4` (`serde`) | Дата/время |
| `sysinfo` | `0.33` | RAM detection |
| `notify` + `notify-debouncer-mini` | `6` / `0.4` | Watcher для `mods/`, `resourcepacks/`, `shaderpacks/` |
| `thiserror` | `2` | Кастомные ошибки |
| `sha1` | `0.10` | Хеширование |
| `uuid` | `1` (`v4`) | Offline-UUID |
| `dirs` | `6` | Кросс-платформенные пути |
| `hex` | `0.4` | hex-кодирование |
| `urlencoding` | `2` | URL-кодирование |
| `futures` | `0.3` | Async helpers |

Rust edition `2021`. Release-профиль: `strip = true`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `opt-level = "s"`.

### Frontend (React / TypeScript)

| Компонент | Версия |
|---|---|
| `react` / `react-dom` | `^19.1.0` |
| `typescript` | `~5.8.3` |
| `vite` | `^7.0.4` |
| `@vitejs/plugin-react` | `^4.6.0` |
| `zustand` | `^5.0.13` (state management) |
| `react-router-dom` | `^7.15.1` |
| `lucide-react` | `^1.16.0` (иконки) |
| `marked` | `^18.0.4` (Markdown в описаниях) |
| `dompurify` | `^3.4.7` (санитайзинг HTML) |
| `@tauri-apps/api` | `^2` |
| `@tauri-apps/plugin-dialog` | `^2.7.1` |
| `@tauri-apps/plugin-fs` | `^2.5.1` |
| `@tauri-apps/plugin-opener` | `^2` |
| `@tauri-apps/plugin-process` | `^2.3.1` |
| `@tauri-apps/plugin-shell` | `^2.3.5` |
| `@tauri-apps/plugin-updater` | `^2.10.1` |
| `@tauri-apps/cli` | `^2` (devDep) |

---

## 3. Архитектура бэкенда (`src-tauri/src/`)

```
src-tauri/src/
├── main.rs              # Точка входа (вызывает lib::run)
├── lib.rs               # AppState, все #[tauri::command], Builder, is_allowed_download_host
├── config.rs            # AppConfig (JSON-конфиг), пути к data/instances/libraries/versions/assets
├── auth.rs              # Microsoft OAuth2 Device Code, Ely.by login, refresh-токен, offline creds
├── accounts.rs          # CRUD аккаунтов (Microsoft / Offline / Ely.by) в accounts.json
├── instances.rs         # CRUD инстансов, моды, скриншоты, миры, паки
├── launch.rs            # Сборка JVM args, classpath, запуск Java-процесса
├── jvm.rs               # GC presets (standard / G1GC / ZGC), strip_gc_selection_flags
├── java.rs              # Детекция Java-установок
├── download.rs          # Параллельная загрузка с прогрессом
├── versions.rs          # Version manifest Mojang, classpath, asset index
├── playtime.rs          # ActiveSession, минутные тики, save в playtime.json
├── events.rs            # Tauri events (logs, install progress, game_started/game_exited)
├── error.rs             # LauncherError (thiserror)
├── curseforge.rs        # CurseForge API клиент (backend-модуль готов, UI не подключён)
├── modrinth.rs          # Modrinth API клиент (моды, паки)
└── modloaders/
    ├── mod.rs           # LoaderProfile, LoaderType
    ├── fabric.rs        # Fabric loader
    ├── quilt.rs         # Quilt loader
    ├── forge.rs         # Forge loader
    └── neoforge.rs      # NeoForge loader
```

### Ключевые структуры

#### `AppState` (`lib.rs`)
```rust
pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub auth_state: Mutex<auth::AuthState>,
    pub running_instance_id: Mutex<Option<String>>,
    pub pack_watcher: Mutex<Option<PackWatcherHandle>>,
    pub icon_cache: Mutex<HashMap<String, String>>,  // persist в icon_cache.json
    pub active_session: Mutex<Option<playtime::ActiveSession>>,
}
```

#### `AppConfig` (`config.rs`)
- `data_dir`, `client_id` (Microsoft OAuth), `default_memory_mb`, `max_memory_mb`
- `default_gc_preset: String` (стандартный/G1GC/ZGC), `default_jvm_args: Vec<String>`
- `java_path: Option<PathBuf>`, `close_on_launch: bool`, `show_snapshots`, `show_old_versions`
- `curseforge_api_key: String` — поле `#[serde(default)]`, отсутствие ключа в конфиге не ломает сохранение

#### `Instance` (`instances.rs`)
Поля: `name`, `mc_version`, `loader` (Vanilla/Fabric/Quilt/Forge/NeoForge), `loader_version`, `loader_profile`, `memory_mb`, `jvm_args`, `gc_preset`, `java_path`, `resolution`, `icon`, `created_at`, `last_played`, `play_time_seconds` (мержится из `playtime.json`), `notes`.

### NSIS-установщик (`tauri.conf.json` → `bundle.windows.nsis`)
```json
{ "installMode": "currentUser" }
```
Установка в `%LOCALAPPDATA%\VoidLauncher\`, без UAC, автообновления работают тихо.

### Capabilities (`src-tauri/capabilities/default.json`)
- `core:default` + window controls (min/max/unmax/close/is-maximized/set-size/set-position/center)
- `opener:default`, `shell:default` + `shell:allow-open`, `dialog:default`
- `process:default` (для `relaunch()`)
- `fs:default` + узкие `fs:allow-*` (read/write/exists/mkdir/remove/rename/read-dir/read-file/write-file)
- `updater:default`
- `fs:scope`: `$APPDATA/**`, `$HOME/**`, `$RESOURCE/**`

### Updater (`tauri.conf.json` → `plugins.updater`)
```json
{
  "pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDZDNzg3REUwOTlCN0UyRUEKUldUcTRyZVo0SDE0Ykt3NXVlTllIb0R6WTkzQWxxbWRYWGlwN1FlUmZGRUlQSFZ6aDcxWkFsTXMK",
  "endpoints": [
    "https://raw.githubusercontent.com/veb898-cyber/VoidLauncher/main/latest.json"
  ]
}
```
Приватный ключ: `~/.voidlauncher-update.key` (пароль `voidlauncher`), **не в репо** (`*.key` в `.gitignore`).

### Защита
- **CSP** в `tauri.conf.json`: точный allowlist `default-src`, `img-src`, `connect-src`, `style-src`, `font-src`. Не ослаблять.
- **SSRF guard** (`lib.rs` → `is_allowed_download_host`): белый список хостов (Modrinth, CurseForge CDN, Mojang, Fabric/Quilt/Forge/NeoForge Maven). Любой `http://` / `file://` / незнакомый хост → отказ.
- **Path traversal** (`validate_instance_name`): 3-64 символа, запрет `..`, `/`, `\`, `\0`, Windows reserved (`CON`/`PRN`/`COM1`...), `< > : " | ? *`, control-chars; разрешены Unicode (Cyrillic, CJK, emoji). 17 unit-тестов в `lib.rs`.
- **Offline-username validator** (`validate_offline_username`): 3-16 ASCII (буквы/цифры/`_`), кириллица запрещена. Unit-тесты.
- **Token-free IPC bridge**: токены авторизации (`access_token`, `elby_token`) **никогда** не покидают Rust — `cmd_list_accounts` возвращает `PublicAccountEntry` со стрипнутыми секретами.
- **HTTPS-only**: все download-URL.

---

## 4. Архитектура фронтенда (`src/`)

```
src/
├── main.tsx                          # React entry
├── App.tsx                           # Router + useUpdater() + <UpdaterModal />
├── index.css                         # Glass design tokens
├── vite-env.d.ts                     # Vite types + declare __APP_VERSION__
│
├── pages/
│   ├── Home.tsx                      # Quick play + список инстансов + play time
│   ├── Login.tsx                     # Microsoft / Ely.by / Offline login
│   ├── Settings.tsx                  # Java, память, GC, язык, latest version, очистка кэша
│   ├── Logs.tsx                      # Live-логи (info / warn / error)
│   ├── Accounts.tsx                  # Управление аккаунтами, set default
│   ├── Instances.tsx                 # Список + создание
│   └── CreateInstanceWizard.tsx      # Мастер создания инстанса
│
├── components/
│   ├── Titlebar.tsx                  # Кастомный titlebar (min/max/close)
│   ├── Sidebar.tsx                   # Home / Instances / Accounts / Logs / Settings
│   ├── ErrorBoundary.tsx             # React error boundary
│   ├── UpdaterModal.tsx              # Модалка обновления (progress bar, error, relaunch)
│   ├── layout/
│   │   └── HomeLayout.tsx            # Layout для страницы инстансов
│   ├── instances/
│   │   ├── InstanceList.tsx          # Список инстансов в сайдбаре
│   │   ├── InstanceDetail.tsx        # Вкладки Settings / Content / Worlds / Screenshots
│   │   ├── InstanceEditor.tsx        # Редактирование инстанса
│   │   ├── ContentManager.tsx        # Управление модами / паками / шейдерами
│   │   ├── ContentBrowser.tsx        # Браузер установленного контента
│   │   ├── PackBrowser.tsx           # Браузер паков (Modrinth)
│   │   ├── PacksManager.tsx          # Управление resourcepacks / shaderpacks
│   │   ├── WorldsManager.tsx         # Управление saves/
│   │   ├── ScreenshotsGallery.tsx    # Галерея скриншотов
│   │   └── ModBrowser.tsx            # Поиск и установка модов из Modrinth
│   ├── install/
│   │   └── InstallOverlay.tsx        # Прогресс установки поверх UI
│   ├── launch/
│   │   └── GameRunningBadge.tsx      # Бейдж запущенной игры
│   ├── mods/
│   │   └── ModBrowser.tsx            # То же что instances/ModBrowser.tsx (alias)
│   └── ui/
│       ├── Button.tsx                # primary / ghost / play
│       ├── Modal.tsx                 # Модалки
│       ├── Toast.tsx                 # Глобальные уведомления
│       ├── Input.tsx
│       ├── ProgressBar.tsx
│       ├── CustomSelect.tsx
│       ├── LoadingSpinner.tsx
│       ├── Skeleton.tsx
│       └── index.ts                  # Barrel export
│
├── stores/                           # Zustand, ВСЕ через индивидуальные селекторы
│   ├── authStore.ts
│   ├── accountsStore.ts
│   ├── settingsStore.ts
│   ├── instanceStore.ts
│   ├── logStore.ts
│   ├── languageStore.ts              # Persist в localStorage
│   └── focusStore.ts                 # Freeze UI когда игра запущена
│
├── hooks/
│   ├── useGameEvents.ts              # Подписка на Tauri events
│   ├── useKeyboardShortcuts.ts
│   ├── useUpdater.ts                 # check() + downloadAndInstall() + progress throttling
│   └── useLatestVersion.ts           # Fetch latest.json, semver compare, refresh
│
└── lib/
    ├── i18n/
    │   ├── en.ts                     # 341 ключ (default)
    │   ├── ru.ts                     # 341 ключ
    │   └── index.ts                  # t() / useT() / formatPlayTime() / getLanguage()
    └── memory.ts                     # Форматирование MB/GB
```

### Паттерны

**Zustand — только индивидуальные селекторы** (стабильные ссылки, минимум ре-рендеров):
```ts
const instances = useInstanceStore((s) => s.instances);
const launchGame = useInstanceStore((s) => s.launchGame);
```

**i18n** (`src/lib/i18n/index.ts`):
- `t(key, vars?)` — bare-функция, читает язык через `useLanguageStore.getState().language`
- `useT()` — React-хук, пере-рендерит при смене языка
- `MessageKey = keyof typeof en` — TypeScript проверяет все ключи в `en.ts` / `ru.ts`
- `formatPlayTime(seconds)` — локализованные «1 час 5 минут» / «1 hour 5 minutes»
- **Не переводятся** (технические термины): Minecraft, Java, RAM, Xmx, Xms, G1GC, ZGC, JVM, Modrinth, CurseForge, Fabric, Quilt, Forge, NeoForge, Vanilla, LWJGL, Microsoft, Windows, NVIDIA, OpenGL, VoidLauncher
- **Переменные** (`{name}`, `{count}`, `{error}`) сохраняются в переводах verbatim

**Updater flow** (`useUpdater.ts`):
1. Через 3 сек после маунта → `check()` из `@tauri-apps/plugin-updater`
2. Если update найден → state `{ updateAvailable: true, updateInfo: { version, body } }`
3. `downloadAndInstall(callback)` — `Started` → `contentLength`, `Progress` → `chunkLength` с троттлингом (setState только при смене rounded %)
4. `relaunch()` из `@tauri-apps/plugin-process` — перезапуск лаунчера
5. Ошибки: toast через `addToast(t('updater.error', { error }))`

**Latest version check** (`useLatestVersion.ts`):
- `APP_VERSION` инжектится Vite-define'ом из `package.json` (`__APP_VERSION__`)
- `useLatestVersion()` fetches `latest.json` с `raw.githubusercontent.com/.../main/latest.json` (cache: no-store)
- `compareVersions(a, b)` — MAJOR.MINOR.PATCH, пре-релиз-теги стрипаются (`"0.1.7-rc1" == "0.1.7"`)
- `getVersionComparison(current, latest)` → `{ status: -1|0|1|null, updateAvailable }`
- Используется в `Settings.tsx` (блок «Latest version» + кнопка «Check for updates»)

**Java subprocess safety**:
- В `cmd_launch_game` Java-процесс запускается с `Command::new(...).creation_flags(0x08000000)` (`CREATE_NO_WINDOW`) в `launch.rs` / `java.rs` / `jvm.rs` — иначе при запуске игры на секунду вылезает чёрное консольное окно
- `stdout` / `stderr` Java-процесса читаются в фоновых потоках с `String::from_utf8_lossy` (важно для не-UTF-8 кодировок типа CP1251 на русской Windows)
- Логи `stderr` помечаются уровнем `error` для подсветки в UI

**Playtime** (`playtime.rs` + `cmd_launch_game`):
- При старте игры создаётся `ActiveSession { started_at, last_flush, child }` (Arc<Mutex<Option<Child>>>)
- Каждые 60 сек тик — `add_minutes_and_save` в `playtime.json` (поминутно, не на каждом тике — вычитаем `unpaid_minutes`)
- При exit (try_wait → Some) — финальный флаш через `take_session`
- На чтение (`cmd_list_instances`, `cmd_get_instance`) — мерж из `playtime.json` в `instance.play_time_seconds`

**File watcher** (`cmd_watch_instance`):
- `notify-debouncer-mini` (300ms debounce) на `mods/`, `resourcepacks/`, `shaderpacks/`
- Эмитит `instance_dir_changed` event с `subfolder` — фронт перечитывает список

**Cache clear** (`cmd_clear_cache` в `lib.rs`):
- Удаляет `assets/` и `libraries/` (полностью перекачаются при следующем запуске)
- Вызывается из `Settings.tsx`

**Latest.json signing** (CI):
- `tauri-action@v0` **не** подписывает (нет `includeUpdaterJson: true`)
- Отдельный bash-шаг: `node_modules/.bin/tauri signer sign installer.exe` через env-переменные `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
- Сигнатура пишется в `installer.exe.sig` (base64 SignatureBox, **не** `.minisig`!)
- Из `.sig` собирается `latest.json`, который коммитится в `main` тем же workflow

---

## 5. CI/CD

**Файл:** `.github/workflows/publish.yml`, имя `Release`, триггер `push tags: ['v*']`.

**Шаги:**
1. `actions/checkout@v4`
2. `actions/setup-node@v4` (Node 20)
3. `dtolnay/rust-toolchain@stable`
4. `swatinem/rust-cache@v2` (workspaces: `src-tauri -> target`)
5. `npm ci`
6. `tauri-apps/tauri-action@v0` — сборка Windows-установщика + создание GitHub Release с ассетами. **Без** `includeUpdaterJson: true` (подпись делаем сами, чтобы ошибка была громкой)
7. **Sign installer and generate latest.json** (bash, `set -euxo pipefail`):
   - Нормализует secret (поддерживает `base64-of-minisign-text` и сырой minisign-текст) → `KEY_FOR_CLI`
   - `gh release download $TAG --pattern VoidLauncher_X.Y.Z_x64-setup.exe --output installer.exe`
   - `TAURI_SIGNING_PRIVATE_KEY=$KEY_FOR_CLI TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... node_modules/.bin/tauri signer sign installer.exe`
   - Читает `installer.exe.sig` (base64), собирает `latest.json` с `platforms.windows-x86_64.{signature,url}`
8. **Commit latest.json to main** — `github-actions[bot]` коммитит `latest.json` в main

**Секреты репозитория:**

| Secret | Значение | Назначение |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | Содержимое `~/.voidlauncher-update.key` | Приватный ключ minisign |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | `voidlauncher` | Пароль к ключу |
| `GITHUB_TOKEN` | (auto) | Создание релизов, `gh release download` |

**Процесс нового релиза:**
1. Поднять версию в `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` (все три!)
2. Закоммитить в `main`
3. `git tag vX.Y.Z && git push origin vX.Y.Z`
4. Дождаться зелёного CI (≈10–12 мин: 9 мин сборка + 30 сек подпись + коммит)
5. Проверить, что коммит `chore: update latest.json for vX.Y.Z` появился в `main`

**Updater client:**
- Endpoint: `https://raw.githubusercontent.com/veb898-cyber/VoidLauncher/main/latest.json` (raw GitHub, не release assets — обходит 404 на draft-релизах)
- `check()` через 3 сек после старта лаунчера
- Сигнатура проверяется встроенным `pubkey` (см. `tauri.conf.json`)

---

## 6. Quick Reference

```bash
# Dev (hot reload)
npm run tauri dev

# Production build (frontend + Tauri bundle)
npm run tauri build

# Frontend build only
npm run build

# TypeScript check
npx tsc --noEmit

# Rust check
cd src-tauri && cargo check

# Rust tests (35+ unit tests: instance name / offline username validation, и др.)
cd src-tauri && cargo test

# Генерация нового signing key (только при утере!)
npx tauri signer generate --password "voidlauncher"
# Затем: записать приватный ключ в GitHub Secret, обновить pubkey в src-tauri/tauri.conf.json

# Создание нового релиза
git tag v0.X.Y && git push origin v0.X.Y
```

**Путь установки у пользователя:** `%LOCALAPPDATA%\VoidLauncher\` (NSIS `currentUser`).

---

*Последнее обновление: v0.1.0 (stable, production)*
*Автор: veb898-cyber*

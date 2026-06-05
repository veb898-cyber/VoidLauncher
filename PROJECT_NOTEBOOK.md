# PROJECT_NOTEBOOK.md — VoidLauncher

> **Единый "Живой Блокнот Проекта"** — создан для того, чтобы любой ИИ (включая тебя же в будущем) мгновенно понимал весь контекст проекта без дополнительного анализа кода. Обновляй при каждом значимом изменении.

---

## 1. Общая информация и глобальная цель

| Поле | Значение |
|---|---|
| **Название** | VoidLauncher |
| **Описание** | Кастомный лаунчер Minecraft для Windows |
| **Для кого** | Для друзей разработчика (персональное использование) |
| **Версия** | 0.1.0 (активная разработка) |
| **Репозиторий** | https://github.com/veb898-cyber/VoidLauncher |
| **Платформа** | Windows 10/11 (x86_64) |
| **Язык UI** | English (по умолчанию) + Русский (переключаемый) |

### Глобальная цель
Создать удобный, быстрый и красивый лаунчер Minecraft с:
- Поддержкой Microsoft, Ely.by и оффлайн аккаунтов
- Модами (Fabric, Quilt, Forge, NeoForge) и их установкой из Modrinth/CurseForge
- Автообновлениями (без прав админа)
- Полной локализацией EN/RU
- Автоматической сборкой и релизами через CI/CD

---

## 2. Технологический стек

### Backend (Tauri / Rust)
| Компонент | Версия |
|---|---|
| **Tauri** | 2.x (`tauri = "2"`) |
| **Rust edition** | 2021 |
| **tauri-build** | 2.x |
| **serde / serde_json** | 1.x |
| **reqwest** | 0.12 (json, stream) |
| **tokio** | 1.x (full) |
| **zip** | 2.x |
| **chrono** | 0.4 (serde) |
| **sysinfo** | 0.33 |
| **notify** | 6.x (file watcher) |
| **thiserror** | 2.x |
| **sha1** | 0.10 |
| **uuid** | 1.x (v4) |

### Frontend (React / TypeScript)
| Компонент | Версия |
|---|---|
| **React** | 19.x |
| **TypeScript** | 5.8.x |
| **Vite** | 7.x |
| **Zustand** | 5.x (state management) |
| **lucide-react** | 1.16+ (иконки) |
| **marked** | 18.x (Markdown рендеринг) |
| **DOMPurify** | 3.x (санитайзинг HTML) |
| **react-router-dom** | 7.x |

### Tauri плагины (Rust side)
| Плагин | Версия | Назначение |
|---|---|---|
| `tauri-plugin-opener` | 2.x | Открытие ссылок в системном браузере |
| `tauri-plugin-shell` | 2.x | Запуск внешних команд |
| `tauri-plugin-dialog` | 2.x | Нативные диалоги (выбор файлов) |
| `tauri-plugin-process` | 2.x | `relaunch()` для автообновления |
| `tauri-plugin-fs` | 2.x | Чтение/запись файловой системы |
| `tauri-plugin-updater` | 2.x | Автоматические обновления + подпись |

### Tauri плагины (Frontend side)
| Пакет | Версия |
|---|---|
| `@tauri-apps/api` | ^2 |
| `@tauri-apps/plugin-dialog` | ^2.7.1 |
| `@tauri-apps/plugin-fs` | ^2.5.1 |
| `@tauri-apps/plugin-opener` | ^2 |
| `@tauri-apps/plugin-process` | ^2.3.1 |
| `@tauri-apps/plugin-shell` | ^2.3.5 |
| `@tauri-apps/plugin-updater` | ^2.10.1 |

### Сборка и dev体验
| Инструмент | Назначение |
|---|---|
| **Vite** | Bundler для фронтенда |
| **npm** | Менеджер пакетов |
| **cargo** | Rust toolchain |
| **@tauri-apps/cli** | ^2 (CLI для `npm run tauri`) |

### Release profile (Cargo)
```toml
[profile.release]
strip = true      # Удаление символов
lto = true        # Link-Time Optimization
codegen-units = 1 # Максимальная оптимизация
panic = "abort"   # Нет unwinding
opt-level = "s"   # Оптимизация по размеру
```

---

## 3. Архитектура бэкенда (Tauri / Rust)

### Структура файлов `src-tauri/src/`

```
src-tauri/src/
├── main.rs              # Точка входа (вызывает lib::run)
├── lib.rs               # Основной модуль: AppState, все Tauri commands, Builder
├── config.rs            # AppConfig (JSON конфиг), recommended_memory_mb, detect_total_ram_mb
├── auth.rs              # Microsoft OAuth2 Device Code Flow, Ely.by, токены
├── accounts.rs          # Управление аккаунтами (Microsoft, Offline, Ely.by) в accounts.json
├── instances.rs         # CRUD инстансов (instance.json), моды, скриншоты, миры
├── launch.rs            # Сборка JVM аргументов, classpath, запуск Java процесса
├── jvm.rs               # GC presets (Standard/G1GC/ZGC), strip_gc_selection_flags
├── java.rs              # Детекция Java установок, выбор оптимальной версии
├── download.rs          # Параллельная загрузка файлов с прогрессом
├── versions.rs          # Version manifest (Mojang), сборка classpath, game args
├── playtime.rs          # Трекинг наигранного времени (playtime.json)
├── events.rs            # Tauri event system (логи, прогресс установки, launch events)
├── error.rs             # LauncherError enum (thiserror)
├── curseforge.rs        # CurseForge API клиент
├── modrinth.rs          # Modrinth API клиент
└── modloaders/
    ├── mod.rs           # LoaderProfile, LoaderType
    ├── fabric.rs        # Fabric loader installation
    ├── quilt.rs         # Quilt loader installation
    ├── forge.rs         # Forge loader installation
    └── neoforge.rs      # NeoForge loader installation
```

### Ключевые структуры данных

#### `AppState` (lib.rs)
```rust
pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub auth_state: Mutex<auth::AuthState>,
    pub running_instance_id: Mutex<Option<String>>,
    pub pack_watcher: Mutex<Option<PackWatcherHandle>>,
    pub icon_cache: Mutex<HashMap<String, String>>,
    pub active_session: Mutex<Option<playtime::ActiveSession>>,
}
```

#### `AppConfig` (config.rs)
```rust
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub client_id: String,                    // Microsoft OAuth
    pub default_memory_mb: u32,
    pub max_memory_mb: u32,
    pub default_gc_preset: String,            // "standard" | "g1gc" | "zgc"
    pub default_jvm_args: Vec<String>,
    pub java_path: Option<PathBuf>,
    pub close_on_launch: bool,
    pub show_snapshots: bool,
    pub show_old_versions: bool,
    pub curseforge_api_key: String,           // #[serde(default)]
}
```

#### `Instance` (instances.rs)
```rust
pub struct Instance {
    pub name: String,
    pub mc_version: String,
    pub loader: LoaderType,                   // Vanilla/Fabric/Quilt/Forge/NeoForge
    pub loader_version: Option<String>,
    pub loader_profile: Option<LoaderProfile>,
    pub memory_mb: Option<u32>,
    pub jvm_args: Option<Vec<String>>,
    pub gc_preset: Option<String>,
    pub java_path: Option<PathBuf>,
    pub resolution: Option<Resolution>,
    pub icon: Option<String>,
    pub created_at: String,
    pub last_played: Option<String>,
    pub play_time_seconds: u64,               // Мержится из playtime.json при list/get
    pub notes: String,
}
```

### NSIS-установщик (CurrentUser)
```json
// tauri.conf.json → bundle.windows.nsis
{
  "installMode": "currentUser"
}
```
- Устанавливает в `%LOCALAPPDATA%\VoidLauncher\`
- **Без UAC**, без прав админа
- Автообновления работают тихо

### Tauri Capabilities (permissions)
Файл: `src-tauri/capabilities/default.json`
```json
{
  "permissions": [
    "core:default",
    "core:window:allow-minimize",
    "core:window:allow-maximize",
    "core:window:allow-unmaximize",
    "core:window:allow-close",
    "core:window:allow-is-maximized",
    "core:window:allow-set-size",
    "core:window:allow-set-position",
    "core:window:allow-center",
    "opener:default",
    "shell:default",
    "shell:allow-open",
    "dialog:default",
    "process:default",
    "fs:default",
    "fs:allow-read",
    "fs:allow-write",
    "fs:allow-exists",
    "fs:allow-mkdir",
    "fs:allow-remove",
    "fs:allow-rename",
    "fs:allow-read-dir",
    "fs:allow-read-file",
    "fs:allow-write-file",
    "updater:default",
    { "identifier": "fs:scope", "allow": ["$APPDATA/**", "$HOME/**", "$RESOURCE/**"] }
  ]
}
```

### Updater конфигурация
```json
// tauri.conf.json → plugins.updater
{
  "pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDZDNzg3REUwOTlCN0UyRUEK...",
  "endpoints": [
    "https://github.com/veb898-cyber/VoidLauncher/releases/latest/download/latest.json"
  ]
}
```
- Приватный ключ: `~/.voidlauncher-update.key` (пароль: `voidlauncher`)
- НЕ в репозитории (в .gitignore через `*.key`)

### Безопасность
- **CSP**: Ограничены источники (img-src, connect-src, style-src, font-src)
- **SSRF protection**: `is_allowed_download_host()` проверяет хосты перед загрузкой
- **Path traversal**: `validate_instance_name()` блокирует `..`, `/`, `\`, спецсимволы
- **Token-free bridge**: Приватный ключ не пересекает IPC мост (остаётся в Rust)
- **HTTPS enforcement**: Все URL должны начинаться с `https://`
- **Username validation**: Запрет кириллицы, контроль длины (3-16), ASCII-only

---

## 4. Архитектура фронтенда (React / TypeScript)

### Структура файлов `src/`

```
src/
├── main.tsx                          # Точка входа React
├── App.tsx                           # Корневой компонент (роутинг, updater)
├── index.css                         # Глобальные стили (glass-design)
├── vite-env.d.ts                     # Vite type declarations
│
├── pages/
│   ├── Home.tsx                      # Главная (quick play, список инстансов)
│   ├── Login.tsx                     # Microsoft/Ely.by/Offline авторизация
│   ├── Settings.tsx                  # Настройки (Java, память, GC, язык)
│   ├── Logs.tsx                      # Просмотр логов
│   ├── Accounts.tsx                  # Управление аккаунтами
│   ├── Instances.tsx                 # Список инстансов + создание
│   └── CreateInstanceWizard.tsx      # Мастер создания инстанса
│
├── components/
│   ├── Titlebar.tsx                  # Кастомный заголовок окна
│   ├── Sidebar.tsx                   # Навигация (Home/Instances/Accounts/Logs/Settings)
│   ├── ErrorBoundary.tsx             # Обработка ошибок React
│   ├── UpdaterModal.tsx              # Модалка обновления лаунчера
│   ├── layout/
│   │   └── HomeLayout.tsx            # Layout для страницы инстансов
│   ├── instances/
│   │   ├── InstanceList.tsx          # Список инстансов в сайдбаре
│   │   ├── InstanceDetail.tsx        # Детали инстанса (вкладки: Settings/Content/Worlds/Screenshots)
│   │   ├── InstanceEditor.tsx        # Редактирование инстанса
│   │   ├── ContentManager.tsx        # Управление модами/ресурспаками/шейдерпаками
│   │   ├── ContentBrowser.tsx        # Браузер контента (установленные файлы)
│   │   ├── PackBrowser.tsx           # Браузер паков (Minecraft/Modrinth)
│   │   ├── ModBrowser.tsx            # Браузер модов (Modrinth)
│   │   ├── PacksManager.tsx          # Управление паками (Minecraft/Modrinth)
│   │   ├── WorldsManager.tsx         # Управление мирами (saves)
│   │   └── ScreenshotsGallery.tsx    # Галерея скриншотов
│   ├── install/
│   │   └── InstallOverlay.tsx        # Оверлей прогресса установки
│   ├── launch/
│   │   └── GameRunningBadge.tsx      # Бейдж запущенной игры
│   ├── mods/
│   │   └── ModBrowser.tsx            # Поиск и установка модов
│   └── ui/
│       ├── Button.tsx                # Кнопки (primary/ghost/play)
│       ├── Modal.tsx                 # Модальные окна
│       ├── Toast.tsx                 # Уведомления (глобальные)
│       ├── Input.tsx                 # Поля ввода
│       ├── ProgressBar.tsx           # Прогресс-бар
│       ├── CustomSelect.tsx          # Кастомный select
│       ├── LoadingSpinner.tsx        # Спиннер загрузки
│       ├── Skeleton.tsx              # Скелетон загрузки
│       └── index.ts                  # Barrel export
│
├── stores/
│   ├── authStore.ts                  # Авторизация (Microsoft/Ely.by/Offline)
│   ├── accountsStore.ts              # Управление списком аккаунтов
│   ├── settingsStore.ts              # Конфигурация (AppConfig)
│   ├── instanceStore.ts              # Инстансы (список, запуск, установка)
│   ├── logStore.ts                   # Логи (массив сообщений)
│   ├── languageStore.ts              # Язык (persist в localStorage)
│   └── focusStore.ts                 # Фокус окна (freeze при запущенной игре)
│
├── hooks/
│   ├── useGameEvents.ts              # Tauri events (launch, install, log)
│   ├── useKeyboardShortcuts.ts       # Горячие клавиши
│   └── useUpdater.ts                 # Проверка/скачивание обновлений
│
├── lib/
│   ├── i18n/
│   │   ├── en.ts                     # Английские переводы (~350 ключей)
│   │   ├── ru.ts                     # Русские переводы (~350 ключей)
│   │   └── index.ts                  # t(), useT(), formatPlayTime(), getLanguage()
│   └── memory.ts                     # Форматирование памяти (MB/GB)
```

### Zustand Stores — паттерны

Все store используют **индивидуальные селекторы** для оптимизации рендеров:
```tsx
// Правильно (стабильные ссылки):
const instances = useInstanceStore((s) => s.instances);
const launchGame = useInstanceStore((s) => s.launchGame);

// Неправильно (вызывает ре-рендер при любом изменении store):
const { instances, launchGame } = useInstanceStore();
```

### useUpdater.ts — логика обновлений

```typescript
// 1. Через 3 сек после монтирования → check() из @tauri-apps/plugin-updater
// 2. Если update найден → setState({ updateAvailable: true, updateInfo: { version, body } })
// 3. Пользователь нажимает "Update Now" → downloadAndInstall()
// 4. downloadAndInstall(callback) — скачивает с прогрессом
// 5. relaunch() из @tauri-apps/plugin-process — перезапуск
```

### UpdaterModal.tsx — UI обновлений

- Модальное окно с описанием версии
- ProgressBar при скачивании
- Текст "Installing..." при установке
- Красный текст ошибки при сбое
- Кнопки: "Обновить" / "Позже"
- Нельзя закрыть во время скачивания/установки

### Локализация (i18n)

- **Два файла**: `en.ts` (по умолчанию) и `ru.ts`
- **Типизированные ключи**: `MessageKey = keyof typeof en` — TypeScript проверяет все ключи
- **`t()` — bare function**: Читает язык из `useLanguageStore.getState().language`
- **`useT()` — React hook**: Подписывается на store, перерендеривает при смене языка
- **`formatPlayTime()`**: Локализованное время (RU: "1 час 5 минут", EN: "1 hour 5 minutes")
- **Не переводятся**: Minecraft, Java, RAM, Xmx, Xms, G1GC, ZGC, JVM, Modrinth, CurseFabric, Fabric, Quilt, Forge, NeoForge, Vanilla, LWJGL, Microsoft, Windows, NVIDIA, OpenGL
- **Переменные**: `{name}`, `{count}`, `{error}` и т.д. — сохраняются в переводе как есть

---

## 5. Инфраструктура и Релизы (CI/CD)

### GitHub Actions Workflow

Файл: `.github/workflows/publish.yml`

```yaml
name: "Release"
on:
  push:
    tags: ["v*"]       # Срабатывает при пуше тегов v0.1.0, v0.2.0 и т.д.

permissions:
  contents: write       # Разрешение на создание релизов

jobs:
  build-windows:
    runs-on: windows-latest
    steps:
      - Checkout
      - Node.js 20
      - Rust stable
      - Rust cache (swatinem/rust-cache@v2)
      - npm ci
      - tauri-apps/tauri-action@v0
```

### Что делает tauri-action

1. Собирает frontend (`npm run build`)
2. Собирает Rust backend (`cargo build --release`)
3. Создаёт NSIS-установщик (`.exe`) + портативный `.exe`
4. Подписывает бинарники приватным ключом
5. Генерирует `latest.json` (для автообновлений)
6. Создаёт GitHub Release и прикрепляет все артефакты

### Секреты репозитория

| Secret | Значение | Описание |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | Содержимое `~/.voidlauncher-update.key` | Приватный ключ подписи обновлений |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | `voidlauncher` | Пароль к приватному ключу |
| `GITHUB_TOKEN` | (автоматический) | Токен для создания релизов |

### Процесс релиза

```bash
# 1. Обнови версию в tauri.conf.json и package.json
# 2. Закоммить
git add -A && git commit -m "v0.1.1"

# 3. Создай и пуши тег
git tag v0.1.1
git push origin v0.1.1

# 4. GitHub Actions соберёт и создаст релиз
# 5. Пользователи увидят обновление при следующем запуске
```

### Как лаунчер понимает, что вышло обновление

1. При запуске через 3 сек → `check()` → GET `latest.json` с GitHub Releases
2. `latest.json` содержит: `version`, `notes`, `pub_date`, `platforms.windows.x64.url` + `.signature`
3. Если `version` > текущей → показывается UpdaterModal
4. Скачивание → проверка подписи (pubkey) → установка → перезапуск

---

## 6. Текущий статус и План расширения

### Что уже реализовано (v0.1.0)

| Функционал | Статус |
|---|---|
| Авторизация (Microsoft OAuth2 Device Code) | ✅ |
| Оффлайн аккаунты | ✅ |
| Ely.by аккаунты | ✅ |
| Создание/удаление инстансов | ✅ |
| Запуск Minecraft (Vanilla + мод loader'ы) | ✅ |
| Автоматическая установка Java | ✅ |
| Установка библиотек и ассетов с прогрессом | ✅ |
| Fabric / Quilt / Forge / NeoForge | ✅ |
| GC presets (Standard/G1GC/ZGC) с auto-fallback | ✅ |
| Управление памятью JVM (Xms/Xmx) | ✅ |
| Кастомные JVM аргументы | ✅ |
| Установка модов из Modrinth | ✅ |
| Установка паков из Modrinth (resourcepacks/shaderpacks) | ✅ |
| Браузер контента (моды/паки/шейдеры) | ✅ |
| Управление мирами (saves) | ✅ |
| Галерея скриншотов | ✅ |
| Трекинг наигранного времени | ✅ |
| Тёмная тема (glass design) | ✅ |
| Локализация EN/RU (~350 ключей) | ✅ |
| Кастомный Titlebar + Sidebar | ✅ |
| Логи установки/запуска | ✅ |
| Горячие клавиши | ✅ |
| Автообновления (Tauri updater) | ✅ |
| CI/CD (GitHub Actions) | ✅ |
| NSIS-установщик (Per-User) | ✅ |
| Подпись релизов (minisign) | ✅ |
| Оптимизация Zustand селекторов | ✅ |
| XSS protection (DOMPurify) | ✅ |
| SSRF protection | ✅ |
| Path traversal protection | ✅ |
| Токен рефреш перед запуском | ✅ |

### Известные баги / TODO

| Проблема | Описание |
|---|---|
| `cmd_get_instance_dir` → `instance.dir()` | Должен возвращать `.minecraft_dir()` (уже исправлено) |
| Двойное открытие ссылок в описаниях | Исправлено: `onClick` + `openUrl()` вместо `target="_blank"` |
| Иконки обрезались при длинном тексте | Исправлено: `overflow: hidden` только на `.btn--play` |
| CurseForge API key молча ломал сохранение | Исправлено: `#[serde(default)]` на `curseforge_api_key` |

### Планы на будущее

| Функционал | Приоритет | Описание |
|---|---|---|
| CurseForge интеграция | 🔴 Высокий | Установка модов/паков с CurseForge API |
| Обновление модов | 🔴 Высокий | Проверка обновлений для установленных модов |
| Профили сборок | 🟡 Средний | Сохранение наборов модов как профилей |
| Автозапуск Minecraft | 🟡 Средний | Запуск игры двойным кликом по .minecraft |
| Проверка целостности файлов | 🟡 Средний | Проверка повреждённых/отсутствующих файлов |
| Кастомные иконки инстансов | 🟡 Средний | Загрузка自己的 иконок |
| Краши и отчёты | 🟢 Низкий | Автоматический сбор крашей |
| Multi-platform (Linux/macOS) | 🟢 Низкий | Расширение CI/CD на другие ОС |
| Обновление модов | 🟡 Средний | Auto-update для модов из Modrinth/CurseForge |
| Интеграция с模组 launcher'ами | 🟢 Низкий | CurseForge App, Modrinth App |
| Профили Java | 🟡 Средний | Переключение между JDK 8/17/21 |
| Статистика | 🟢 Низкий | Графики наигранного времени |

---

## Quick Reference — Часто используемые команды

```bash
# Dev mode (hot reload)
npm run tauri dev

# Production build (frontend only)
npm run build

# Production build (full Tauri)
npm run tauri build

# Rust type check
cd src-tauri && cargo check

# Rust tests
cd src-tauri && cargo test

# TypeScript type check
npx tsc --noEmit

# Run all 35 Rust tests
cd src-tauri && cargo test

# Generate signing key
npx tauri signer generate --password "voidlauncher"

# Push release
git tag v0.1.1 && git push origin v0.1.1
```

---

*Последнее обновление: v0.1.0 — 05.06.2026*
*Автор: veb898-cyber*

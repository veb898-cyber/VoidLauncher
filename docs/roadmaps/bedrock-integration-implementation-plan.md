# Интеграция официального Minecraft for Windows (Bedrock) — план

> Статус: исследование и план. Код в рамках этого этапа не изменяется.
> Связанные документы: [`skin-editor-implementation-plan.md`](skin-editor-implementation-plan.md), [`theme-editor-implementation-plan.md`](theme-editor-implementation-plan.md), [`README.md`](README.md).

## Легенда

- **Подтверждено** — проверено в официальной документации или на текущей машине.
- **Предлагаемое решение** — рекомендация для будущей реализации.
- **Требует исследования** — нельзя обещать без spike/проверки.

## 1. Цель и границы

### Цель

Добавить в VoidLauncher отдельную страницу для **официальной** версии Minecraft for Windows (Bedrock Edition): показать, установлена ли игра, дать официальную ссылку на покупку/пробную версию, запустить установленный экземпляр и дать безопасный доступ к папке данных. В следующих этапах — показать миры и делать резервные копии. Лаунчер не заменяет Store, не обходит лицензию и не изменяет установленный пакет игры.

### Что входит в MVP

- Отдельная страница `Bedrock` в боковой панели, доступная всегда.
- Определение установленного пакета `Microsoft.MinecraftUWP` для текущего пользователя.
- Статус: не установлено / установлено / ошибка; версия, package family, путь установки, AUMID.
- Официальные кнопки:
  - «Открыть Microsoft Store»;
  - «Пробная версия» — официальная страница trial;
  - «Купить» — официальная страница Store.
- Запуск установленной игры через официальный Windows API активации приложений.
- Понятная юридическая плашка: VoidLauncher не связан с Mojang/Microsoft; покупка и лицензия — через Microsoft Store.
- Диагностическая карточка для поддержки: версия, PFN, AUMID, пути, код ошибки. Без токенов и личных данных.
- Открытие папки данных Bedrock (`LocalState\games\com.mojang`) через существующий безопасный механизм.
- Локализация en/ru, состояния загрузки/ошибки, доступность с клавиатуры.

### Что входит в этап 2 (после MVP)

- Список миров: имя, размер, дата изменения, иконка (если безопасно читается).
- Открыть папку мира.
- Резервная копия мира/всех миров в zip; восстановление — только с подтверждением и предварительной копией.
- Открыть папку `custom_skins` и `skin_packs` только для просмотра.
- Информация о пакетах/скинах без изменения.

### Что не входит

- Пиратство, обход Microsoft Store/лицензии, подмена лицензионных файлов, распространение игры или её ассетов.
- Скачивание Bedrock с сторонних сайтов, «офлайн-активация», обход Xbox Live.
- Автоматический вход в Microsoft/Xbox-аккаунт.
- Изменение `AppxManifest.xml`, переупаковка `Microsoft.MinecraftUWP`, установка модифицированного пакета.
- Прямая инъекция скинов в `custom_skins` (внутренний формат не документирован).
- Запуск конкретного мира через неподтверждённый протокол.
- Кроссплатформенность: Bedrock для Windows — Windows 10/11; на других ОС страница должна честно сообщать о недоступности.
- Поддержка preview-версий, Education Edition, Java Edition, сторонних клиентов.

### Критерии успеха MVP

1. На машине без Bedrock страница не выглядит ошибкой: объясняет, что игры нет, и ведёт в официальный Store/trial.
2. На машине с Bedrock страница показывает корректную версию и запускает игру одной кнопкой.
3. Если запуск невозможен, пользователь видит понятную причину и код для диагностики, а не «ничего не произошло».
4. VoidLauncher не скачивает и не распространяет игру, не обходит лицензию.
5. Страница не требует Microsoft-аккаунта в VoidLauncher: Bedrock использует свой Xbox-аккаунт.
6. Все действия с файлами ограничены package data и подтверждены пользователем.
7. `tsc`, `vitest`, `cargo fmt`, `cargo clippy`, `cargo test` зелёные.

## 2. Текущее состояние

### 2.1. Что уже есть в VoidLauncher

**Подтверждено кодом:**

- `src/components/Sidebar.tsx` — массив `navItems` с `home`, `instances`, `rooms`, `accounts`, `terminal`; страницы переключаются через `onNavigate`.
- `src/App.tsx` — централизованный рендер страниц; отдельной Bedrock-страницы нет.
- `src-tauri/src/commands/launcher.rs` — запуск Java-инстансов, работа с процессами, Java runtime.
- `src-tauri/src/commands/misc.rs` — открытие папок, канонизация путей, защита от выхода за пределы разрешённых директорий.
- `src-tauri/src/commands/mod.rs` — регистрация Tauri-команд.
- `src-tauri/Cargo.toml` — есть `zip`, `sysinfo`, `chrono`, `dirs`, `serde_json`, `tauri-plugin-opener`, `tauri-plugin-dialog`, `tauri-plugin-fs`; **нет** крейта `windows`.
- `src-tauri/capabilities/default.json`:
  - `opener:default`, `shell:default`, `dialog:default`, `process:default`, `fs:default`;
  - fs scope: `$APPDATA/**`, `$RESOURCE/**`;
  - доступа к `$LOCALAPPDATA/Packages/**` через fs plugin нет.
- `src/pages/Settings.tsx` и другие страницы используют `t()` и русские строки из `src/lib/i18n/ru.ts`; `en.ts` — source of truth.
- Tauri updater обновляет только VoidLauncher; Bedrock обновляется через Microsoft Store независимо.

**Следствие:** для Bedrock нужен отдельный backend-модуль и команды. Нельзя переиспользовать Java-запуск: у Bedrock другая модель установки, лицензирования, процессов и данных. Нельзя открывать `LocalState` через текущий fs scope без отдельного решения.

### 2.2. Что подтверждено на текущей машине

**Подтверждено:**

- Установлен пакет `Microsoft.MinecraftUWP`, версия `1.26.5203.0`.
- `PackageFullName`: `Microsoft.MinecraftUWP_1.26.5203.0_x64__8wekyb3d8bbwe`.
- `PackageFamilyName`: `Microsoft.MinecraftUWP_8wekyb3d8bbwe`.
- Package data: `%LOCALAPPDATA%\Packages\Microsoft.MinecraftUWP_8wekyb3d8bbwe`.
- В `LocalState\games\com.mojang` присутствуют `custom_skins`, `resource_packs`, `skin_packs`, `behavior_packs`.
- `custom_skins` и `skin_packs` сейчас пусты.
- `minecraftWorlds` на этой машине отсутствует — миров нет, формат мира не проверялся.
- `Get-AppxPackage` и `Get-AppxPackageManifest` работают.
- В манифесте Application Id = `Game`, Executable = `GameLaunchHelper.exe`, EntryPoint = `Windows.FullTrustApplication`.
- Протоколы: `ms-xbl-35760c07`, `ms-xbl-multiplayer`, `minecraft`.
- AUMID: `Microsoft.MinecraftUWP_8wekyb3d8bbwe!Game` (наблюдение; должен вычисляться, а не хардкодиться).
- `Microsoft.MinecraftJavaEdition 1.0.5.0` установлен отдельно; это не Bedrock.
- `Get-Process Minecraft.Windows` на машине ничего не вернул, но игра не была запущена. Имя процесса нельзя считать подтверждённым.

**Не подтверждено:**

- Универсальность AUMID/PFN/пути на всех установках и архитектурах (x64/ARM64).
- Прямой запуск конкретного мира.
- Стабильность и документированность внутреннего формата `custom_skins`.
- API прямой загрузки скина в Bedrock.
- Поведение `GameLaunchHelper.exe`: это долгоживущий процесс игры или промежуточный helper, который запускает другой процесс.
- Наличие блокировок файлов мира, по которым можно надёжно определить «игра запущена».

### 2.3. Официальные источники

- Пробная версия: <https://www.minecraft.net/en-us/free-trial> — около 100 минут, без мультиплеера, LAN разрешён; доступность зависит от региона/аккаунта/Store.
- Покупка: <https://www.minecraft.net/en-us/store/minecraft-deluxe-collection-pc>.
- EULA: <https://www.minecraft.net/en-us/eula> — инструменты/плагины допустимы, если не выглядят официальными; распространение и коммерческое использование материалов Mojang без разрешения запрещены.
- Запуск приложений: <https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iapplicationactivationmanager-activateapplication>.
- Поиск AUMID: <https://learn.microsoft.com/en-us/windows/configuration/store/find-aumid>.
- `Get-AppxPackage`: <https://learn.microsoft.com/en-us/powershell/module/appx/get-appxpackage>.
- Package data: <https://learn.microsoft.com/en-us/windows/apps/develop/data/store-and-retrieve-app-data>.
- Bedrock skin pack: <https://learn.microsoft.com/en-us/minecraft/creator/documents/packagingaskinpack>.
- Официальный product ID: `9NBLGGH2JHXJ`. `https://www.microsoft.com/store/apps/9NBLGGH2JHXJ` и `https://apps.microsoft.com/detail/9NBLGGH2JHXJ` перенаправляют на Xbox Store (проверено, HTTP 200). Deep link `ms-windows-store://pdp/?productid=9NBLGGH2JHXJ` не проверен из-за ограничений окружения, но является официальным форматом Store.

**Не использовать:** `learn.microsoft.com/en-us/minecraft/creator/documents/skins` и `/skins/overview` — 404.

## 3. Предлагаемая архитектура

### 3.1. Общая схема

```
Sidebar
  └─ Bedrock page
       ├─ status card (detect)
       ├─ official store / trial links
       ├─ launch button
       ├─ worlds (phase 2)
       └─ diagnostics

Rust commands
  ├─ cmd_bedrock_status() -> BedrockStatus
  ├─ cmd_bedrock_launch() -> BedrockLaunchResult
  ├─ cmd_bedrock_worlds() -> Vec<BedrockWorld>
  ├─ cmd_bedrock_backup_worlds() -> BackupResult
  └─ cmd_bedrock_open_data_dir(kind) -> ()

Windows
  ├─ Get-AppxPackage / manifest (MVP: PowerShell)
  ├─ IApplicationActivationManager (launch)
  └─ package data under %LOCALAPPDATA%\Packages\<PFN>
```

### 3.2. Почему отдельная страница, а не карточка в Home

**Предлагаемое решение:** отдельный пункт `Bedrock` в Sidebar.

- Bedrock — не Java-инстанс: у него нет `instance.json`, Java runtime, mod loader, папки `.minecraft` в привычном смысле.
- Пользователь должен видеть статус установки и официальные ссылки, даже если игра не установлена.
- Миры, скины и диагностика — отдельный контекст, который не помещается в карточку Home.
- Страница может развиваться независимо от Java-части; это соответствует принципу «независимые направления» из README.

**Альтернатива:** вкладка внутри Instances. Отклонена: смешает две разные модели владения игрой и усложнит пустые состояния.

### 3.3. Определение установки

#### MVP: PowerShell с фиксированным путём

**Предлагаемое решение:** вызывать `powershell.exe` по фиксированному пути, без пользовательского ввода, с таймаутом и разбором JSON.

```
%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe
  -NoProfile -NonInteractive -Command
  "$p = Get-AppxPackage -Name Microsoft.MinecraftUWP | Sort-Object Version -Descending | Select-Object -First 1; ..."
```

Собирать:

- `Name`, `PackageFullName`, `PackageFamilyName`, `Version`, `InstallLocation`, `Status`, `SignatureKind`;
- Application Id, Executable, EntryPoint из `Get-AppxPackageManifest`;
- AUMID = `PackageFamilyName + "!" + ApplicationId`.

**Правила безопасности:**

- Команда — константа; никакой конкатенации путей/имён пользователя.
- Запуск с `CREATE_NO_WINDOW`, stderr/stdout с лимитом, timeout 5–10 секунд; при timeout процесс убить.
- Не использовать `-ExecutionPolicy Bypass` без необходимости: inline-команда не требует файла политики; если политика блокирует, показать ошибку.
- Не использовать `-AllUsers` (требует прав и не отражает текущего пользователя).
- Не читать содержимое `WindowsApps`; только метаданные пакета.
- JSON парсить как `serde_json::Value`, принимать объект или массив (PowerShell может вернуть массив при нескольких пакетах).
- Если пакетов несколько — выбрать `Status = Ok` и наибольшую версию; архитектуру сверить с ОС, если доступна.

**Почему PowerShell в MVP:** меньше кода, использует официальный `Get-AppxPackage`, легко диагностируется, не требует новой зависимости `windows`. **Минус:** зависимость от PowerShell, политик и локали. Для production-надежности нужен native-путь.

#### Этап 2: native WinRT

**Предлагаемое решение:** заменить/дополнить PowerShell на `windows` crate:

- `windows::Management::Deployment::PackageManager`;
- `FindPackagesForUser` / `FindPackagesForUserWithPackageTypes`;
- `Package::Id`, `Package::InstalledLocation`, `Package::Status`;
- чтение `AppxManifest.xml` из `InstalledLocation` только для метаданных (или `Package::GetAppListEntriesAsync`).

Добавлять фичу `Management_Deployment` и COM/WinRT инициализацию. Это отдельный spike: неизвестно, насколько API доступен без elevation и как ведёт себя на ARM64.

**Рекомендация:** MVP на PowerShell, но с интерфейсом `BedrockDetector` (trait), чтобы native-реализация не меняла команды и UI.

### 3.4. Запуск

**Предлагаемое решение:** `IApplicationActivationManager::ActivateApplication`.

- Получить AUMID из detection, не хардкодить.
- COM: `CoInitializeEx(COINIT_APARTMENTTHREADED)`, `CoCreateInstance(CLSID_ApplicationActivationManager)`, `ActivateApplication(aumid, None, AO_NONE, &pid)`, `CoUninitialize`.
- Выполнять в blocking-потоке (`spawn_blocking`), не блокировать UI.
- Возвращать `pid` и HRESULT; при ошибке — человекочитаемое сообщение + код.
- Не передавать аргументы в MVP.
- Не использовать `cmd /c start`, `explorer shell:AppsFolder\...` и `minecraft://` как основной путь: меньше контроля, возможны проблемы с экранированием и PID.

**Требует исследования:**

- `ActivateApplication` возвращает PID `GameLaunchHelper.exe` или игры?
- Можно ли считать запуск успешным по PID, если helper сразу завершается?
- Как определить, что игра уже запущена, чтобы не открывать второй экземпляр?
- Есть ли у Bedrock single-instance behavior (скорее всего, да, но проверить).

**MVP-компромисс:** после успешного `ActivateApplication` показывать «Запуск…», блокировать кнопку на 15–20 секунд, затем «Запустить снова». Статус «Игра запущена» не обещать, пока не подтверждён процесс.

### 3.5. Официальные ссылки

**Предлагаемое решение:**

- «Купить» → `https://www.minecraft.net/en-us/store/minecraft-deluxe-collection-pc` (официальная страница) или Xbox Store product page.
- «Пробная версия» → `https://www.minecraft.net/en-us/free-trial`.
- «Открыть Store»:
  - primary: `ms-windows-store://pdp/?productid=9NBLGGH2JHXJ`;
  - fallback: `https://www.xbox.com/games/store/minecraft-for-windows/9NBLGGH2JHXJ` или `https://apps.microsoft.com/detail/9NBLGGH2JHXJ`.
- Использовать `@tauri-apps/plugin-opener`, не `shell.open`.
- `opener:default` может не разрешать custom scheme. Нужно либо добавить явный scope для `ms-windows-store://*`, либо в MVP открывать только HTTPS-страницу Store, а deep link добавить после проверки. **Требует проверки** на Windows 10/11.
- Все ссылки — константы; не строить URL из пользовательских данных.

### 3.6. Данные и пути

**Подтверждено:** `%LOCALAPPDATA%\Packages\<PFN>\LocalState\games\com.mojang`.

**Предлагаемое решение:**

```rust
fn bedrock_data_dir(pfn: &str) -> Result<PathBuf> {
    // PFN: [A-Za-z0-9._-]+, обязательный суффикс _8wekyb3d8bbwe для stable
    // local_app_data/Packages/<pfn>/LocalState/games/com.mojang
}
```

- Проверять PFN по whitelist символов; не принимать путь из frontend.
- Проверять, что итоговый путь существует и является директорией.
- Не канонизировать через `..` без проверки; использовать `canonicalize` и проверять префикс `local_app_data/Packages`.
- Возвращать frontend только безопасные пути для отображения/открытия.
- `worlds_dir = data_dir/minecraftWorlds`; `custom_skins_dir = data_dir/custom_skins`; `skin_packs_dir = data_dir/skin_packs`.
- Если `minecraftWorlds` нет — это не ошибка: показать «Миров пока нет».

**Открытие папки:** использовать существующий `misc.rs`-подход; не давать frontend произвольный путь. Команда принимает enum `BedrockFolder { Data, Worlds, CustomSkins, SkinPacks }`, а путь вычисляет backend.

### 3.7. Миры (этап 2)

**Предлагаемое решение:**

- Итерировать только непосредственные поддиректории `minecraftWorlds`.
- Для каждой:
  - `id` = имя директории (валидировать, не использовать как путь без проверки);
  - `name` = содержимое `levelname.txt` (UTF-8, trim, максимум 100 символов) или имя директории;
  - `modified` = mtime `level.dat` (chrono);
  - `size` = рекурсивный размер с лимитом/таймаутом; при превышении — «—»;
  - `icon` = `world_icon.jpeg`, читать только если размер < 512 КБ, возвращать data URL или отдельную команду; кэшировать в памяти.
- Не парсить `level.dat` (NBT) в MVP: это лишняя зависимость и риск.
- Не показывать «последняя игра» без надёжного источника.
- Сортировка: по дате изменения, новые сверху.
- Пустое состояние: «В этом профиле пока нет миров Bedrock. Создайте мир в игре.»

**Безопасность:** имя мира — недоверенный текст; React экранирует; не использовать в путях, кроме проверенного id. Иконка — только JPEG, лимит размера, без SVG.

### 3.8. Резервные копии (этап 2)

**Предлагаемое решение:**

- Кнопка «Создать резервную копию» для выбранного мира или всех.
- Требовать подтверждение: «Закройте Minecraft for Windows перед копированием».
- Проверять, что игра не запущена, если появится надёжный способ; иначе — чекбокс пользователя.
- Копировать в `data_dir/voidlauncher-backups/<timestamp>/<world-id>/` или в выбранную пользователем папку.
- Использовать существующий `zip` crate; писать во временный `.part`, затем rename.
- Не удалять и не перезаписывать оригинальные миры.
- Восстановление — только этап 3: сначала автоматическая safety-копия текущего мира, затем распаковка, затем проверка; никогда не «восстановить поверх» без подтверждения.
- Не обещать «атомарное восстановление»: Bedrock может держать файлы открытыми.

**Риск:** копирование работающей игры может дать повреждённый backup. Лучше явно блокировать/предупреждать, чем делать вид, что backup гарантирован.

### 3.9. Скины и паки (этап 2/3)

**Подтверждено:** `custom_skins` и `skin_packs` существуют, но пусты; официальная документация описывает **skin pack**, а не внутренний формат `custom_skins`.

**Предлагаемое решение:**

- В MVP — только открыть папку `custom_skins` и объяснить: «Bedrock хранит скины в собственном формате; VoidLauncher не изменяет их».
- Не реализовывать импорт скина в Bedrock без документированного API/spike.
- `skin_packs` — список файлов/папок read-only, без импорта/удаления.
- Не копировать `.png` в `custom_skins` «наугад»: это может сломать профиль или ничего не сделать.
- Не путать с Java-скинами из `skin-editor-implementation-plan.md`: это разные форматы и пайплайны.

### 3.10. UI

```
src/pages/Bedrock.tsx
src/components/bedrock/
  BedrockStatusCard.tsx
  BedrockInstallCard.tsx
  BedrockWorldsCard.tsx
  BedrockSkinsCard.tsx
  BedrockDiagnosticsCard.tsx
  BedrockLegalNote.tsx
src/stores/bedrockStore.ts
src/lib/bedrockTypes.ts
```

**Структура страницы:**

1. Заголовок «Minecraft for Windows (Bedrock)» + статус-чип.
2. Если не установлено:
   - объяснение, что это отдельная игра, не Java;
   - кнопки «Открыть Store», «Пробная версия»;
   - плашка о лицензии;
   - кнопка «Проверить снова».
3. Если установлено:
   - карточка «Установка»: версия, PFN, путь, кнопка «Запустить»;
   - карточка «Миры» (этап 2);
   - карточка «Скины» (этап 2, read-only);
   - «Диагностика» collapsible.
4. Ошибки:
   - `not_installed` — не ошибка, а пустое состояние;
   - `powershell_unavailable`, `package_broken`, `activation_failed`, `data_dir_missing` — с действиями.

**Состояния кнопки запуска:** idle → launching → launched/timeout; ошибка с кодом. Не показывать бесконечный спиннер.

**Доступность:** статус не только цветом; кнопки с понятными label; фокус; `aria-live` для результата запуска.

### 3.11. Backend-команды и типы

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BedrockStatus {
    pub installed: bool,
    pub state: String, // not_installed | installed | error
    pub package_full_name: Option<String>,
    pub package_family_name: Option<String>,
    pub version: Option<String>,
    pub application_id: Option<String>,
    pub aumid: Option<String>,
    pub install_location: Option<String>,
    pub data_dir: Option<String>,
    pub worlds_dir: Option<String>,
    pub custom_skins_dir: Option<String>,
    pub skin_packs_dir: Option<String>,
    pub signature_kind: Option<String>,
    pub error: Option<BedrockError>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BedrockError {
    pub code: String,
    pub message: String,
    pub details: Option<String>, // без секретов
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BedrockLaunchResult {
    pub ok: bool,
    pub pid: Option<u32>,
    pub error: Option<BedrockError>,
}
```

Команды:

- `cmd_bedrock_status(force: bool)` — detection + data paths; кэш 5–10 секунд.
- `cmd_bedrock_launch()` — повторно проверяет installed/AUMID, активирует.
- `cmd_bedrock_worlds()` — этап 2.
- `cmd_bedrock_backup_worlds(world_id: Option<String>)` — этап 2.
- `cmd_bedrock_open_data_dir(folder: BedrockFolder)` — enum, не произвольный путь.
- `cmd_bedrock_store_url()` — не обязательно; ссылки можно держать во frontend, но централизованно в backend/types лучше.

**Регистрация:** `src-tauri/src/commands/bedrock.rs`, добавить в `commands/mod.rs` и `invoke_handler` в `lib.rs`.

### 3.12. Зависимости

**MVP:**

- detection — PowerShell + `serde_json`, без новых крейтов;
- launch — добавить `windows` (фичи `Win32_UI_Shell`, `Win32_System_Com`) или `windows-sys` с теми же функциями; это единственная новая зависимость MVP;
- остальное — существующие `tauri-plugin-opener`, `tauri-plugin-dialog`, `zip`, `chrono`.

**Почему не PowerShell для запуска:** `IApplicationActivationManager` можно вызвать через `Add-Type`/COM из PowerShell, но это хрупко, хуже типизируется и усложняет диагностику. PowerShell остаётся для detection, native COM — для запуска.

**Native-этап:** добавить в `windows` фичу `Management_Deployment` для `PackageManager` (точный набор уточнить в spike). Не добавлять тяжёлые оболочки/эмуляторы.

## 4. Последовательность этапов

### Этап 0. Spike (1–2 дня)

- [ ] Зафиксировать точную PowerShell-команду detection и JSON-схему.
- [ ] Проверить `Get-AppxPackageManifest` на чистой машине/другом пользователе.
- [ ] Проверить AUMID = PFN + `!` + Application Id.
- [ ] Запустить игру через `IApplicationActivationManager` (Rust или PowerShell-прототип).
- [ ] Выяснить, что возвращает PID и как определить, что игра уже запущена.
- [ ] Проверить `ms-windows-store://` через opener и fallback.
- [ ] Проверить поведение на машине без Bedrock.
- [ ] Решить: PowerShell MVP или сразу native detection.

**Выход:** подтверждённый протокол detection/launch; список неизвестных.

### Этап 1. MVP (3–5 дней)

- [ ] Добавить `windows`/`windows-sys` для COM-активации.
- [ ] `src-tauri/src/commands/bedrock.rs`: detection, paths, launch, ошибки.
- [ ] Типы + регистрация команд.
- [ ] `Bedrock.tsx` + карточки статуса/установки/легальная плашка.
- [ ] Официальные ссылки Store/trial.
- [ ] Кнопка запуска с состояниями.
- [ ] Диагностика (copy info).
- [ ] Sidebar + App routing + иконка.
- [ ] i18n en/ru.
- [ ] Unit-тесты парсинга/валидации; ручной тест на машине с игрой.
- [ ] Проверка на машине без игры (симуляция: подменить detector в тесте).

### Этап 2. Миры и backup (4–6 дней)

- [ ] `cmd_bedrock_worlds`: список, имена, mtime, размер с лимитом.
- [ ] Иконки с лимитом и кэшем.
- [ ] Открыть папку мира.
- [ ] Backup selected/all в zip, `.part` + rename.
- [ ] Проверка «игра закрыта» (если появится надёжный способ) или чекбокс.
- [ ] UI списка, пустые состояния, ошибки.
- [ ] Тесты на временной директории (без реальной игры).

### Этап 3. Скины/паки и восстановление (3–5 дней)

- [ ] Read-only список `skin_packs`/`custom_skins`.
- [ ] Открыть папку, объяснение формата.
- [ ] Восстановление backup с safety-копией и подтверждением.
- [ ] Исследование: есть ли поддерживаемый способ импорта скина/пака.
- [ ] Не добавлять прямую инъекцию без официального подтверждения.

### Этап 4. Native и advanced

- [ ] Заменить PowerShell на `PackageManager` (или оставить fallback).
- [ ] Определение running state по процессу/активации.
- [ ] Запуск мира, если появится документированный протокол.
- [ ] ARM64/Windows 10 проверка.
- [ ] Тесты на нескольких редакциях (Store, Game Pass, trial).

## 5. Ошибки и безопасность

### Пользовательские ошибки

| Код | Когда | Что показать |
|---|---|---|
| `not_installed` | Пакет не найден | «Minecraft for Windows не установлен», Store/trial |
| `powershell_unavailable` | Нет PowerShell/политика | Диагностика, ссылка на Store, «Проверить снова» |
| `package_broken` | Пакет есть, manifest не читается | «Восстановите игру через Store» |
| `activation_failed` | COM/активация вернула ошибку | HRESULT, «Открыть Store», диагностика |
| `data_dir_missing` | Игра установлена, данных нет | «Запустите игру один раз», открыть папку |
| `worlds_unavailable` | Нет `minecraftWorlds` | Пустое состояние, не ошибка |
| `backup_failed` | Ошибка копирования/zip | Причина, путь, оригинал не тронут |
| `unsupported_os` | Не Windows | Честное сообщение |

### Безопасность

- Не скачивать и не распространять Bedrock; только официальные ссылки.
- Не обходить Store/license/Xbox Live; не автоматизировать вход.
- Не изменять установленный пакет и `AppxManifest.xml`.
- Не выполнять пользовательские строки в PowerShell; только константная команда.
- Не передавать пути из frontend в backend; использовать enum.
- Валидировать PFN, world id, имена файлов; не допускать `..`, абсолютных путей, symlink-выходов.
- Не читать токены Microsoft/Xbox; не логировать их.
- Не отправлять миры/скины в сеть.
- Backup — только локально, с предупреждением о персональных данных.
- Диагностика не должна включать содержимое миров, пути с именем пользователя (по возможности маскировать `C:\Users\<name>`), токены, email.

### Юридическая плашка

> VoidLauncher — независимый лаунчер и не связан с Mojang Studios или Microsoft. Minecraft for Windows приобретается и лицензируется через Microsoft Store. VoidLauncher не содержит игру, не обходит проверки лицензии и не изменяет установленный пакет. Minecraft является товарным знаком Mojang Studios.

## 6. Тестирование

### Unit-тесты

- Парсер PowerShell JSON: объект, массив, пустой вывод, BOM, ошибка.
- Построение AUMID: корректный PFN + Application Id; отсутствующий Id; неверный PFN.
- Валидация PFN: допустимые символы, попытка `..`, пробелы, unicode.
- Пути: data dir, worlds dir; отсутствие директории; выход за пределы.
- World id/name: пустое имя, очень длинное, управляющие символы, path traversal.
- Backup: выбор пути, `.part`, отказ при ошибке, оригинал не изменён.
- Ошибки: маппинг HRESULT/кодов в сообщения без утечек.

### Интеграционные/ручные

- Машина с Bedrock: статус, версия, запуск, повторный запуск.
- Машина без Bedrock: пустое состояние, Store/trial.
- Пользователь без прав администратора.
- PowerShell отключён политикой (симуляция/ручная проверка).
- Store deep link и HTTPS fallback.
- Машина с несколькими пакетами/версиями (если возможно).
- Миры: нет папки, пустая, 1–2 мира, длинные имена, отсутствующая иконка.
- Backup: игра закрыта, игра открыта (ожидаемое предупреждение/ошибка).
- Русская/английская локаль, 900×600.
- Keyboard-only, screen reader labels.

### CI

```powershell
npx tsc --noEmit
npm test
cd src-tauri
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

COM-активацию нельзя надёжно проверить в CI; детектор и запуск скрыть за trait и тестировать логику на mock.

## 7. Критерии готовности

- На чистой машине страница объясняет отсутствие игры и ведёт только на официальные Store/trial.
- На машине с игрой корректно определяются версия, PFN, AUMID, путь.
- Запуск работает без elevation и без PowerShell-инъекций.
- Ошибки содержат код и действие; нет «молчаливого» отказа.
- Нет скачивания/распространения игры, обхода лицензии, изменения пакета.
- FS-доступ ограничен package data; frontend не может открыть произвольный путь.
- Backup не изменяет оригинал; восстановление требует подтверждения.
- Все строки локализованы; юридическая плашка видима.
- `tsc`, `vitest`, `cargo fmt`, `cargo clippy`, `cargo test` зелёные.

## 8. Риски и открытые вопросы

| Риск | Влияние | Смягчение |
|---|---|---|
| AUMID/PFN отличаются | Запуск не работает | Вычислять, не хардкодить; диагностика |
| PowerShell отключён | Нет detection | Native fallback, понятная ошибка |
| `GameLaunchHelper.exe` — не игра | Неверный running state | Не обещать running state в MVP; spike |
| Нет документированного API миров | Хрупкий парсинг | Read-only, лимиты, не менять файлы |
| `custom_skins` формат неизвестен | Порча профиля | Не писать; только открыть папку |
| Backup во время игры | Повреждённая копия | Предупреждение/блокировка, safety-копия |
| Store/trial недоступны в регионе | Пользователь не может установить | Честное сообщение, официальная ссылка |
| Юридические претензии | Репутация/блокировка | Только официальный запуск, плашка, EULA |
| Пользовательские пути | Path traversal | Enum, whitelist, canonicalize |
| Дублирование Java-логики | Ошибки | Отдельный Bedrock-модуль |

### Открытые вопросы для владельца

1. Нужна ли отдельная страница Bedrock в первом релизе или достаточно карточки «Установить Bedrock» в Home?
2. Должны ли миры и backup входить в MVP или это следующий этап?
3. Нужна ли поддержка пробной версии отдельной кнопкой?
4. Нужно ли показывать «игра запущена» или достаточно кнопки «Запустить»?
5. Нужна ли поддержка ARM64/Windows 10 явно?
6. Нужен ли read-only браузер `custom_skins`/`skin_packs` или достаточно открыть папку?

## 9. Сложность и оценка

- **Spike:** S/M, 1–2 дня.
- **MVP:** M, 3–5 дней.
- **Миры + backup:** M/L, 4–6 дней.
- **Скины/паки + restore:** M, 3–5 дней.
- **Native detection/running state:** M, 2–4 дня.
- **Основная сложность:** не UI, а надёжное определение пакета, COM-активация, различие helper/игры, безопасные пути и юридические ограничения.

## 10. Источники и подтверждения

- **Подтверждено официально:** trial, Store, EULA, `IApplicationActivationManager`, `Get-AppxPackage`, package data, skin pack docs, product ID `9NBLGGH2JHXJ`.
- **Подтверждено на машине:** пакет `Microsoft.MinecraftUWP 1.26.5203.0`, PFN, App ID `Game`, `GameLaunchHelper.exe`, AUMID, протоколы, package data, пустые `custom_skins`/`skin_packs`, отсутствие `minecraftWorlds`.
- **Подтверждено кодом:** Sidebar/App routing, `launcher.rs`, `misc.rs`, `commands/mod.rs`, Tauri capabilities, зависимости Cargo.
- **Требует исследования:** поведение PID/helper, running state, прямой запуск мира, внутренний формат `custom_skins`, native `PackageManager`, Store deep link и opener scope.

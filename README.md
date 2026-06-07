# VoidLauncher

A custom, lightweight Minecraft launcher for Windows built with Tauri 2 (Rust + React 19). Designed as a fast, no-telemetry, per-user-installed alternative for players who want a clean experience, native mod loader support, and painless mod browsing.

> **Status:** v0.1.0 — stable, in active personal use.
> **Platform:** Windows 10/11 (x86_64).
> **Audience:** personal project; not a public product.

---

## Why VoidLauncher

Most launchers are either bloated Electron apps, web wrappers, or vendor-locked. VoidLauncher is a small native binary with a React UI, a focused feature set, and zero background services. It installs to `%LOCALAPPDATA%` (no UAC, no admin rights) and updates itself silently with minisign-verified releases.

## Features

- **Multiple account types** — Microsoft (OAuth device code with automatic token refresh), Ely.by, and Offline accounts, all managed in one place.
- **Mod loaders out of the box** — Vanilla, Fabric, Quilt, Forge, and NeoForge profiles with version selection.
- **Modrinth integration** — search, install, and update mods and modpacks directly from the launcher.
- **CurseForge backend** — API client is wired in; UI surfacing is a work in progress.
- **Java management** — automatic detection of installed Java runtimes, custom path override, RAM sliders (Xmx/Xms), and GC presets (standard / G1GC / ZGC).
- **Per-instance settings** — independent memory, JVM args, Java path, resolution, icon, and notes per instance.
- **Playtime tracking** — per-instance time, persisted to disk, survives suspend/resume.
- **Live logs** — Minecraft stdout/stderr streamed in real time, with non-UTF-8 (CP1251) capture on Russian Windows.
- **Worlds, screenshots, resource packs, shader packs** — full content manager for every instance.
- **Auto-updates** — signed releases delivered via a signed `latest.json` on the `main` branch; user sees a non-dismissible prompt and a single-click relaunch.
- **EN / RU localization** — 341 keys, switchable at runtime from Settings, with a custom `t()` / `useT()` API.
- **No telemetry, no analytics, no remote services** beyond the upstream APIs (Mojang, Microsoft, Ely.by, Modrinth, CurseForge, GitHub raw).

## Tech Stack

| Layer | Technology |
|---|---|
| Shell | Tauri 2 (Rust + WebView2) |
| Frontend | React 19, TypeScript 5.8, Vite 7 |
| State | Zustand 5 (individual selectors) |
| Routing | React Router 7 |
| Icons | Lucide |
| Markdown | `marked` + `dompurify` |
| Async runtime | Tokio (Rust) |
| HTTP | reqwest 0.12 (streaming downloads) |
| File watching | `notify` + `notify-debouncer-mini` (300ms debounce) |

## Installation

Download the latest installer from the [Releases](../../releases) page:

```
VoidLauncher_<version>_x64-setup.exe
```

The installer uses NSIS in `currentUser` mode, so it writes to `%LOCALAPPDATA%\VoidLauncher\` and does not request administrator rights. Auto-update is enabled by default.

## Building from Source

Prerequisites: Node.js 20+, Rust stable, the Tauri 2 prerequisites for Windows, and the WebView2 runtime.

```bash
# Install JS dependencies
npm install

# Run in dev mode (hot reload)
npm run tauri dev

# Produce a release build (frontend + Tauri bundle)
npm run tauri build
```

TypeScript and Rust can be checked independently:

```bash
npx tsc --noEmit
cd src-tauri && cargo check
```

The Rust crate has a growing suite of unit tests (currently 35+), focused on input validation, path-traversal guards, and offline-username rules:

```bash
cd src-tauri && cargo test
```

## Project Layout

```
.
├── src/                       React + TypeScript frontend
│   ├── pages/                 Home, Login, Settings, Logs, Accounts, Instances
│   ├── components/            Sidebar, Titlebar, modals, mod browser, instance UI
│   ├── stores/                Zustand stores (auth, accounts, instances, settings, logs, focus, language)
│   ├── hooks/                 useUpdater, useGameEvents, useLatestVersion, useKeyboardShortcuts
│   └── lib/i18n/              en.ts, ru.ts, t() / useT() helpers
├── src-tauri/                 Rust backend
│   ├── src/
│   │   ├── lib.rs             #[tauri::command] surface, AppState, is_allowed_download_host
│   │   ├── auth.rs            Microsoft OAuth (device code), Ely.by, refresh tokens
│   │   ├── accounts.rs        Account storage and listing (PublicAccountEntry strips secrets)
│   │   ├── instances.rs       Instance CRUD, mod/save/world/screenshot management
│   │   ├── launch.rs          JVM args, classpath, subprocess launch
│   │   ├── jvm.rs             GC presets and flag stripping
│   │   ├── java.rs            Java detection
│   │   ├── download.rs        Parallel chunked downloads with progress events
│   │   ├── versions.rs        Mojang manifest, asset index, library resolution
│   │   ├── playtime.rs        ActiveSession, per-minute flush, suspend-safe accounting
│   │   ├── modloaders/        Fabric / Quilt / Forge / NeoForge
│   │   ├── modrinth.rs        Modrinth API
│   │   ├── curseforge.rs      CurseForge API (backend, no UI yet)
│   │   ├── events.rs          Tauri event names
│   │   └── config.rs          AppConfig (JSON, on disk)
│   ├── capabilities/default.json
│   ├── tauri.conf.json        Window, CSP, bundle, updater
│   └── Cargo.toml
├── .github/workflows/publish.yml   Signed release pipeline
└── latest.json                    Updater manifest (regenerated by CI on every tag)
```

## Security Model

VoidLauncher runs untrusted code (Minecraft + mods) but the launcher itself is hardened in four layers:

1. **Content Security Policy** in `tauri.conf.json` — a hand-maintained allowlist for `default-src`, `img-src`, `connect-src`, `style-src`, `font-src`. Mixed-content downgrade attacks against Mojang asset downloads are blocked at the WebView level.
2. **Download host allowlist** (`is_allowed_download_host` in `lib.rs`) — every Rust-side download routes through a whitelist of trusted hosts. Unknown schemes, `file://`, and unknown domains are rejected.
3. **Input validators** — `validate_instance_name` and `validate_offline_username` block path traversal (`..`, `\`), Windows reserved names, control characters, and unsafe Unicode. Covered by unit tests.
4. **Token-free IPC bridge** — `cmd_list_accounts` returns `PublicAccountEntry`, which has `access_token` and `elby_token` stripped before crossing the IPC boundary. Auth secrets never leave the Rust process.

If you add a new download source, register its host in the allowlist. If you add a new account-bearing command, strip secrets in the response type. These are not suggestions; they are the threat model.

## Auto-Updates

Updates are published as signed `latest.json` files on the `main` branch. The launcher checks for updates three seconds after launch, verifies the minisign signature against the public key embedded in `tauri.conf.json`, and prompts the user before downloading.

To cut a new release:

1. Bump the version in **all three** files: `package.json`, `src-tauri/tauri.conf.json`, and `src-tauri/Cargo.toml`. They must match.
2. Commit to `main`.
3. Tag and push:
   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```
4. The `Release` workflow builds the Windows installer, signs it with the project's minisign key, generates `latest.json`, commits it back to `main`, and attaches the installer to the GitHub release.

The signing key is stored in a GitHub Actions secret and is never present in the repository.

## Localization

All user-facing strings live in `src/lib/i18n/en.ts` and `src/lib/i18n/ru.ts`. The TypeScript `MessageKey` type is derived from `en.ts`, so any missing key in either file is a compile error.

Two helpers are exposed:

- `t(key, vars?)` — bare function for callbacks and effects; reads the language from the store on each call.
- `useT()` — React hook for JSX; subscribes to language changes and re-renders.

Technical terms (Minecraft, Java, RAM, Xmx, Xms, G1GC, ZGC, JVM, Modrinth, CurseForge, Fabric, Quilt, Forge, NeoForge, Vanilla, LWJGL, Microsoft, Windows, NVIDIA, OpenGL, VoidLauncher) are not translated — they appear verbatim in both languages.

## Known Limitations

- **Windows only.** Tauri 2 is cross-platform in principle, but the project is built and tested exclusively on Windows.
- **CurseForge UI not wired up.** The API client and key storage are present; the frontend does not surface it yet.
- **No macOS / Linux builds.** Not a target.

## License

No license file is currently included. All rights reserved by the author. If you intend to use, fork, or redistribute the code, please contact the author first.

## Author

[veb898-cyber](https://github.com/veb898-cyber)

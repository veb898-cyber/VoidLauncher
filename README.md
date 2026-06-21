# VoidLauncher

Lightweight Minecraft launcher for Windows, built with Tauri 2 (Rust + React 19).

> **Platform:** Windows 10/11 (x86_64)
> **Install:** runs in `%LOCALAPPDATA%`, no admin rights required.

---

## Features

- **Accounts** — Microsoft (OAuth device code), Ely.by, Offline
- **Mod loaders** — Vanilla, Fabric, Quilt, Forge, NeoForge
- **Mod browsing** — search, install, update mods & modpacks via Modrinth
- **CurseForge** — backend ready, UI coming soon
- **Java management** — auto-detect, custom path, RAM sliders, GC presets (G1GC / ZGC)
- **Per-instance settings** — independent memory, JVM args, resolution, icon, notes
- **Playtime tracking** — per-instance, survives suspend/resume
- **Live logs** — real-time Minecraft stdout/stderr with CP1251 support
- **Content manager** — worlds, screenshots, resource packs, shader packs
- **Instance import/export** — Prism, MultiMC, Modrinth, CurseForge, ATLauncher packs
- **Mod updates** — hash-based detection like Prism Launcher
- **Banner & icon customization** — gradient presets or custom images per instance
- **Auto-updates** — signed releases via minisign
- **EN / RU localization** — switchable at runtime
- **No telemetry** — zero background services

## Install

Download the latest installer from [Releases](../../releases):

```
VoidLauncher_<version>_x64-setup.exe
```

## Build from source

Prerequisites: Node.js 20+, Rust stable, Tauri 2 prerequisites, WebView2.

```bash
pnpm install
pnpm run tauri build
```

## License

All rights reserved. Contact the author before use or redistribution.

## Author

[veb898-cyber](https://github.com/veb898-cyber)

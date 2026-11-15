# VoidLauncher

A modern, open-source Minecraft launcher for Windows.

[Download the latest release](../../releases/latest) · [Report a bug](../../issues/new?template=bug_report.yml) · [Request a feature](../../issues/new?template=feature_request.yml) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

VoidLauncher is a desktop launcher built around a simple idea: keep Minecraft versions, modded installations, configuration, and multiplayer in one place without turning the launcher itself into a complicated system.

Each Minecraft installation is managed independently, with its own game version, mod loader, Java configuration, content, and settings. The launcher handles the setup and management around Minecraft while keeping the actual game installations separate and easy to work with.

VoidLauncher is built with a native Rust backend and a React-based interface using Tauri 2. The project is designed primarily for Windows and focuses on a lightweight desktop experience rather than replicating the interface or architecture of an existing launcher.

## Getting Started

Download the latest release from the [Releases](../../releases/latest) page and run the Windows installer.

VoidLauncher uses a per-user installation, so administrator privileges are not required for a normal installation.

## Reporting bugs and requesting features

### Bugs

The fastest way to report a problem is to open the launcher and go to **Settings → About → Report a bug**. The in-app reporter:

- collects system, instance and optional diagnostic information;
- redacts tokens, passwords, session IDs, private paths and other sensitive values;
- copies the complete report to your clipboard;
- opens the [bug report form](../../issues/new?template=bug_report.yml) with its fields prefilled.

Review the report and remove anything you do not want to publish before submitting. If the report is very large, the GitHub form may receive shortened sections; the complete text stays on your clipboard. If the launcher cannot start, use the bug report form directly.

### Feature requests

Open a [feature request](../../issues/new?template=feature_request.yml) and describe the problem you are trying to solve, not only the proposed implementation. For large changes, open the request before writing code so the approach can be discussed.

### Questions

Use the [question form](../../issues/new?template=question.yml), or check the [documentation](docs/) first.

## Contributing

VoidLauncher is open to external contributions. Read [CONTRIBUTING.md](CONTRIBUTING.md) for the development setup, architecture overview, test commands, coding conventions and pull-request process.

Good starting points:

- [`good first issue`](../../labels/good%20first%20issue) — small, well-scoped tasks;
- [`help wanted`](../../labels/help%20wanted) — areas where maintainers would appreciate help;
- [open pull requests](../../pulls) — review or test an existing change.

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development

### Requirements

* Windows 10/11 (x86_64)
* Node.js 24+
* Rust (stable)
* [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/)
* WebView2

### Build from source

```bash
npm install
npm run tauri build
```

### Run in development

```bash
npm install
npm run tauri dev
```

### Tests and checks

Run these before opening a pull request:

```bash
npx tsc --noEmit
npm test
npm run build

cd src-tauri
cargo fmt --all -- --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
```

`npm test` runs Vitest once. `cargo test` includes unit, integration and Rooms loopback tests; the real Minecraft launch smoke test is opt-in and downloads game files. See [CONTRIBUTING.md](CONTRIBUTING.md#tests-and-checks) for details.

### Project structure

* `src/` — React UI, Zustand stores and frontend tests.
* `src-tauri/src/` — Rust backend: instances, downloads, launch, mod loaders, imports, accounts, rooms and diagnostics.
* `src-tauri/src/commands/` — thin Tauri command layer.
* `src-tauri/tests/` — Rust integration tests and golden launch fixtures.
* `docs/` — design notes, research and implementation roadmaps.

The desktop-side game management and system integration live in Rust; the user interface is implemented with React.

## Technology

VoidLauncher is built with:

* [Tauri 2](https://tauri.app/)
* Rust
* React 19
* TypeScript
* Vite
* Tokio

## Project

VoidLauncher is an independent open-source project developed around Minecraft and its surrounding ecosystem.

The project is still evolving, and its architecture and user experience may change as development continues.

## Security

Do not report security vulnerabilities in a public issue. Follow [SECURITY.md](SECURITY.md) and use GitHub's private vulnerability reporting.

## License

VoidLauncher is licensed under the [MIT License](LICENSE).

You are free to use, modify, and redistribute the project in accordance with the license terms. Third-party components and their licenses are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Author

[veb898-cyber](https://github.com/veb898-cyber)

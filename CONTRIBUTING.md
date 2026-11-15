# Contributing to VoidLauncher

Thank you for helping improve VoidLauncher. The project is an independent,
open-source Minecraft launcher for Windows, built with a Rust/Tauri 2 backend
and a React 19 + TypeScript frontend.

This guide explains how to set up the project, run the checks and prepare a
pull request. By participating, you agree to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to contribute

You do not have to write Rust or React to help:

- **Report a bug.** Use the in-app **Settings → About → Report a bug** flow so
  diagnostics and redaction are included, or open the
  [bug report form](https://github.com/veb898-cyber/VoidLauncher/issues/new?template=bug_report.yml).
- **Request a feature.** Open a
  [feature request](https://github.com/veb898-cyber/VoidLauncher/issues/new?template=feature_request.yml).
  Describe the problem before the proposed implementation.
- **Ask a question.** Use the
  [question form](https://github.com/veb898-cyber/VoidLauncher/issues/new?template=question.yml).
- **Test a change.** Windows testing on real instances, mod loaders and
  hardware is especially valuable.
- **Improve documentation or translations.** English is the source of truth;
  Russian must stay complete.
- **Fix code or review pull requests.** Start with issues labelled
  [`good first issue`](https://github.com/veb898-cyber/VoidLauncher/labels/good%20first%20issue)
  or [`help wanted`](https://github.com/veb898-cyber/VoidLauncher/labels/help%20wanted).

For a large feature or an architectural change, open an issue first. It is
easier to agree on the approach before a large pull request exists.

**Security vulnerabilities must not be reported in a public issue.** Follow
[SECURITY.md](SECURITY.md) instead.

## Requirements

VoidLauncher targets **Windows 10/11 x64**. Linux and macOS are not supported
targets for the desktop application.

Install:

- [Node.js](https://nodejs.org/) 24 or newer;
- [Rust](https://rustup.rs/) stable (the MSVC toolchain on Windows);
- [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/),
  including the Microsoft C++ Build Tools and WebView2;
- Git.

The CI uses `windows-latest`, Node.js 24 and the current stable Rust
toolchain. Other versions may work, but matching CI is the safest option.

## Get the code

```powershell
git clone https://github.com/veb898-cyber/VoidLauncher.git
cd VoidLauncher
npm install
```

Rust dependencies are downloaded by Cargo on the first build; there is no
separate `cargo install` step.

## Run in development

```powershell
npm run tauri dev
```

This starts Vite on `http://localhost:1420` and opens the Tauri window. The
frontend can also be served alone with `npm run dev`, but launcher features
need the Tauri backend.

Useful production-style commands:

```powershell
# Frontend only: type-check and bundle into dist/
npm run build

# Full Windows application + installers (MSI and NSIS)
npm run tauri build
```

`npm run tauri build` writes installers under
`src-tauri/target/release/bundle/`. A plain
`cargo build --release --features custom-protocol` embeds `dist/` into the
executable; without `custom-protocol` the binary tries to load the development
server at `http://localhost:1420`.

## Project layout

```text
src/                         React UI
  pages/                     top-level screens (Home, Modpacks, Rooms, Settings, ...)
  components/                reusable UI, dialogs and feature panels
  stores/                    Zustand stores and async state
  lib/                       API helpers, formatting, redaction, i18n
  assets/                    bundled fonts and images
src-tauri/
  src/lib.rs                 app bootstrap, AppState, Tauri command registration
  src/commands/              thin Tauri command layer
  src/instances.rs           instance metadata and filesystem layout
  src/versions.rs            Mojang version metadata and launch composition
  src/download.rs            downloads, mirrors, retries and integrity checks
  src/launch.rs              Java process assembly and launch
  src/modloaders/            Fabric, Forge, NeoForge and Prism metadata
  src/packlock.rs            modpack lock files and update planning
  src/import.rs              Prism/MultiMC, Modrinth, CurseForge and ATLauncher import
  src/verify.rs              file verification and repair
  src/accounts.rs, auth.rs   account storage and Microsoft/Ely.by authentication
  src/rooms/                 VoidLink, Tailscale and room networking
  src/diagnostics.rs         system checks, instance checks and crash analysis
  src/bug_report.rs          report collection and secret/path redaction
  src/playtime.rs            playtime tracking
  tests/                     Rust integration tests and golden launch fixtures
  capabilities/              Tauri permissions granted to the main window
  tauri.conf.json            window, CSP, updater and bundle configuration
docs/                        design notes, research and implementation roadmaps
```

The frontend never receives raw logs for bug reports. The Rust side collects,
redacts and truncates them before returning a section to the renderer. Keep that
boundary intact when changing diagnostics or logging.

## Architecture notes

- **Commands are thin.** Files in `src-tauri/src/commands/` validate inputs
  (instance names, subfolders, URLs, filenames) and delegate to a domain module.
  Do not put large filesystem or network workflows directly in a command.
- **`AppState` owns shared state.** Configuration, authentication, launch
  phases, playtime sessions and room state are synchronized there. Avoid
  process-global mutable state.
- **Blocking work does not run on the async runtime.** Filesystem scans, archive
  extraction and CPU-heavy work use `spawn_blocking` or a blocking helper.
- **Instances are independent.** Each instance has its own game version,
  loader, JVM settings and content. Changes to one instance must not leak into
  another.
- **Paths and archives are untrusted.** Validate archive entries, destination
  paths and download URLs before writing. The verification and pack-lock code
  contains deliberate guards against traversal, duplicate destinations and
  manifest overrides; do not remove them to make a test pass.
- **Secrets stay out of logs.** Tokens, refresh tokens, session IDs, API keys,
  passwords and private keys must never be logged or included in a report.
  `src-tauri/src/bug_report.rs` is the redaction boundary; add tests for any
  new sensitive field.
- **UI text is localized.** `src/lib/i18n/en.ts` is the source of truth;
  `src/lib/i18n/ru.ts` must contain every key. Use CSS variables and the
  existing animation timings instead of one-off styles.

## Tests and checks

Run these before opening a pull request:

```powershell
# Frontend
npx tsc --noEmit
npm test
npm run build

# Rust (run from src-tauri)
cd src-tauri
cargo fmt --all -- --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
```

`npm test` runs Vitest once (`vitest run`) and includes the static checks for
the GitHub issue forms and release workflow.

`cargo test` includes unit tests, integration tests and the Rooms end-to-end
tests. The Rooms tests use real TCP loopback sockets but do not need Tailscale
or the internet. The full suite is expected to pass with the default test
parallelism.

Focused commands:

```powershell
cd src-tauri
cargo test --test launch_command
cargo test --test launch_known_issues
cargo test rooms::e2e -- --nocapture
```

The real Minecraft launch smoke test is intentionally ignored because it
downloads several hundred megabytes and needs a graphics-capable Windows
runner:

```powershell
cd src-tauri
cargo test --release smoke_launch::smoke_all -- --ignored --nocapture
```

It uses `SMOKE_DATA_DIR` for an isolated data directory, `SMOKE_FILTER` to run a
subset, `SMOKE_SKIP_INSTALL=1` to reuse prepared files, and
`SMOKE_STDERR_FILE` to keep full stderr. The manual **Minecraft smoke** GitHub
Actions workflow runs the same test.

### Test expectations

- Add a regression test for every bug fix. A test that only checks a helper
  while the real failure path stays untested is not enough.
- Use temporary directories for filesystem tests and clean them up.
- Do not make unit tests depend on Mojang, Modrinth, CurseForge, Tailscale or
  other live services.
- Do not relax an existing assertion to make a change pass. If an expected
  value really changed, explain why in the PR.
- For frontend behavior, prefer a test in `src/**/*.test.ts` or a focused store
  test over relying on manual clicking alone.

## Coding guidelines

### Rust

- Follow `cargo fmt`.
- `cargo clippy --all-targets -- -D warnings` must stay clean.
- Return `Result<T, String>` from Tauri commands and use the existing error
  helpers. Avoid `unwrap()`/`expect()` in production paths; tests may use them.
- Prefer small, domain-focused modules over adding more code to `lib.rs`.
- Preserve atomic-write, path-validation and manifest-protection behavior when
  touching pack installation, imports, verification or world operations.
- Add `#[cfg(test)]` tests next to the code they cover.

### TypeScript and React

- TypeScript is strict; do not introduce `any` to silence a real type problem.
- Use function components and the existing Zustand stores. Keep async operations
  cancellable or sequence-guarded so a stale response cannot overwrite newer
  state.
- Use `useT()` / `t()` for user-facing strings and add both English and Russian
  entries. `src/lib/i18n/i18n.test.ts` checks key completeness.
- Use the existing CSS variables, spacing scale and animation conventions.
  Animations should stay soft (roughly 150–250 ms, ease-out) and must not flash
  or jump.
- Keep accessibility intact: labels, `aria-*`, keyboard operation and focus
  management matter.

### Security and privacy

- Never commit tokens, passwords, `.env` files, signing keys or personal data.
- Do not print secrets in logs, issues, screenshots or test fixtures.
- When adding a new credential-like field, update the redaction lists and tests
  in `src-tauri/src/bug_report.rs` and `src/lib/bugReport.ts` if it can appear
  in a report.
- Do not weaken URL allowlists, archive path checks, CSP or Tauri capabilities
  without a clear security justification.
- Report vulnerabilities privately using [SECURITY.md](SECURITY.md).

## Commit and pull-request workflow

1. Create a short-lived branch from `main`:
   ```powershell
   git switch -c fix/short-description
   ```
2. Keep commits focused. Use a clear prefix such as `fix:`, `feat:`, `docs:`,
   `perf:`, `test:` or `chore:`.
3. Run the checks above and review your own diff before opening the PR.
4. Open the pull request and fill in the template. Link the issue it closes and
   describe the manual testing you performed.
5. Keep the PR up to date with `main`; do not force-push a branch that someone
   is reviewing unless the maintainer asks for it.
6. A maintainer will review the change, may request tests or documentation, and
   will merge it once the required `Windows checks` status is green.

Do not bump the project version in a normal contribution. Version bumps, tags,
installer signing and releases are maintainer tasks.

Pull requests are validated by `.github/workflows/ci.yml`. Its job is named
`Windows checks` (job id `check`) and runs on Windows. `Windows checks` is a
required status check for merging into `main`: a pull request cannot be merged
while it is failing. All changes to `main` are published through pull requests;
direct pushes are blocked by the repository ruleset.

## Release process (maintainers)

Releases are built by the `publish.yml` workflow from a `vX.Y.Z` tag. The tag
must match `package.json`, `src-tauri/tauri.conf.json` and
`src-tauri/Cargo.toml`; the workflow refuses to build otherwise. The workflow
also signs the installer and uploads it to the GitHub release.

The same workflow generates and validates `latest.json`, then opens (or updates)
a pull request from `release/latest-json-vX.Y.Z` to `main` using a GitHub App
installation token. The PR changes only `latest.json`; the required
`Windows checks` must pass and a maintainer merges it normally. Reruns update
the same branch and PR without force-pushing. The App needs `Contents: write`
and `Pull requests: write`, stored as `RELEASE_APP_ID` and
`RELEASE_APP_PRIVATE_KEY`; never add a ruleset bypass or use a PAT for this.

Signing keys and passwords are stored outside the repository (local key files
or GitHub Actions secrets). Never commit them, print them in a workflow log or
paste them into an issue.

## License

By contributing, you agree that your contribution is licensed under the
project's [MIT License](LICENSE).

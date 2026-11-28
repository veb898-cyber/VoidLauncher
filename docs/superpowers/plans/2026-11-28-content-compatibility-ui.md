# ContentManager Compatibility Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a complete, freshness-aware local Compatibility Engine report in the Mods pane of ContentManager while preserving the separate Modrinth/CurseForge catalog check.

**Architecture:** Add a dedicated Tauri command that resolves one instance and uses the existing installed-content reader, cached JAR metadata, and pure analyzer on a blocking worker. Keep the catalog check independent. Add an additive structured dependency detail to findings, then present the local report in a compact summary and expandable, filterable panel with request-generation guards.

**Tech Stack:** Tauri 2 commands, Rust/Serde, React 19/TypeScript, existing `invoke`, Vitest, Cargo tests.

**Spec:** `docs/superpowers/specs/2026-11-28-content-compatibility-ui-design.md` (derived from the user goal objective file at `C:\Users\User\.codex\attachments\b262ba6f-fae1-43d2-a551-aa6191835185\goal-objective.md`).

## Global Constraints

- Work only on `feat/content-compatibility-ui`, based on merge `5e11a5abd8ce3a1186c9d1929b263f75fcea75e4`.
- Do not modify `backup-pre-compatibility-work` or user build files.
- Keep local diagnostics independent of the Modrinth/CurseForge catalog check.
- Reuse the existing metadata cache and file watcher; add no network calls to the pure analyzer.
- Incomplete/error results must not appear as “no conflicts” or “fully compatible”.
- Do not automatically remove/disable content, gate Minecraft launch, or change versions or install transactions.
- All new UI strings must be localized in English and Russian.

## Review Focus

1. Missing or unreadable mods directories must not produce a successful empty report; cover command-level errors and a legitimate missing/empty mods directory.
2. Malformed or unreadable JAR metadata must stay `unknown`/incomplete; test the existing parser-to-report path and ensure UI renders the incomplete state.
3. Multiple providers, nested entries, and disabled mods must preserve filenames and versions in findings; cover structured dependency details and detail rendering.
4. Instance switching or content changes during a request must not let stale responses replace the current report; cover deferred frontend promises and invalidation.
5. Catalog-check state and failures must remain independent from local diagnostics; cover both commands with local-only and network-only failure cases.

---

### Task 1: Structured dependency detail in analyzer reports

**Files:**
- Modify: `src-tauri/src/compatibility.rs`
- Test: `src-tauri/src/compatibility.rs` (`compatibility::tests`)

**Interfaces:**
- Produces `CompatibilityFinding.dependency: Option<DependencyFindingDetail>`.
- `DependencyFindingDetail` contains dependency `mod_id`, `kind`, optional `requirement`, `alternatives`, and `providers: Vec<InstalledModVersion>`.
- `InstalledModVersion` contains `filename` and optional `version`.
- Existing finding fields and certainty logic remain unchanged.

- [ ] **Step 1: Write failing tests** asserting a version mismatch serializes the declared range and all observed provider versions/filenames; assert missing dependencies include the requirement with an empty provider list; assert non-dependency findings omit the detail.
- [ ] **Step 2: Run the focused analyzer tests** with `cargo test --lib compatibility::tests::dependency_finding_details -- --test-threads=1`; confirm the new assertions fail because the structured field is absent.
- [ ] **Step 3: Add the additive detail types and a dependency-aware finding helper**; populate details only for dependency/incompatibility findings where the source metadata is available. Do not alter status selection.
- [ ] **Step 4: Re-run the focused tests** and then `cargo test --lib compatibility::tests -- --test-threads=1`.

### Task 2: Instance-scoped backend command

**Files:**
- Modify: `src-tauri/src/commands/content.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/src/commands/content.rs` (command/helper tests)

**Interfaces:**
- Produces `cmd_analyze_instance_mod_compatibility(instance_name: String) -> Result<CompatibilityReport, String>`.
- Uses `instances::get_instance`, `content::list_installed`, `commands::mods::get_mod_metadata`, and `compatibility::analyze_installed_mods`.
- Clones configuration before `spawn_blocking`; it does not call Modrinth/CurseForge or mutate installed files.

- [ ] **Step 1: Write failing tests** for a real empty mods directory, a populated fixture-backed instance producing engine findings, and an unreadable/non-directory mods path returning an error rather than a clean report.
- [ ] **Step 2: Run the focused command/helper tests** and confirm failures represent the missing orchestration/error behavior.
- [ ] **Step 3: Implement the command and register it**; preflight directory access so scan errors propagate while an absent/empty directory follows the documented empty-set path; run cached metadata parsing and analysis inside `spawn_blocking`.
- [ ] **Step 4: Re-run command tests** and run `cargo check --tests`.

### Task 3: Frontend report lifecycle and race protection

**Files:**
- Modify: `src/components/instances/ContentManager.tsx`
- Create: `src/components/instances/ContentManager.compatibility.test.tsx` (or extend an existing manager-level test if test setup supports it)

**Interfaces:**
- Local report state is separate from the existing `compatibility` catalog state.
- A refresh accepts the current `instanceName`, invokes `cmd_analyze_instance_mod_compatibility`, and stores receipt time only on success.
- Request generation and in-flight state prevent parallel requests and stale instance/snapshot results.

- [ ] **Step 1: Write failing manager tests** for manual refresh, duplicate-refresh coalescing, report clearing while refreshing, instance changes, and an older deferred response arriving after a newer result.
- [ ] **Step 2: Run the focused Vitest tests** and confirm they fail on the absent command/state lifecycle.
- [ ] **Step 3: Implement typed report state and a single refresh path**; invalidate on `loadItems`/watcher changes, rerun after reload, clear old reports while pending, and retain localized error state on command failure.
- [ ] **Step 4: Re-run lifecycle tests** and existing `ContentManager.row.test.ts`.

### Task 4: Compact diagnostics UI and localization

**Files:**
- Modify: `src/components/instances/ContentManager.tsx`
- Modify: `src/lib/i18n/en.ts`
- Modify: `src/lib/i18n/ru.ts`
- Test: `src/components/instances/ContentManager.compatibility.test.tsx`

**Interfaces:**
- Summary displays total, confirmed, potential, unknown, completeness, and last-check time.
- Findings are filtered by all/confirmed/potential/unknown and each finding expands to show every involved mod name/ID, dependency requirement and known versions, source, affected files, and explanation.
- Only mod content renders diagnostics; catalog badges and provider checks keep existing behavior.

- [ ] **Step 1: Write failing UI tests** for localized summary counts, complete-empty vs incomplete/error states, every filter, multiple findings for one mod, version/provider details, and no-regression independence from catalog compatibility.
- [ ] **Step 2: Run focused UI tests** and confirm they fail due to missing report UI.
- [ ] **Step 3: Implement the compact summary and expandable finding list** using existing ContentManager components, tokens, and i18n patterns; render status text/icons as well as color.
- [ ] **Step 4: Re-run UI tests** and existing ContentManager tests in both locale coverage paths.

### Task 5: Full verification and delivery

**Files:**
- Review all changed files; update local `CHANGELOG.md` according to the repository's private instructions.

- [ ] **Step 1: Run required Rust checks**: `cargo test`, `cargo check --tests`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check` from `src-tauri`/repository root as appropriate.
- [ ] **Step 2: Run required frontend checks**: `npm test`, `npx tsc --noEmit`, and `npm run build`.
- [ ] **Step 3: Review the full diff and staged contents**, verify the working tree contains only this feature, scan staged content for secrets, and ensure version/config files were not changed.
- [ ] **Step 4: Commit focused feature changes, push to `feat/content-compatibility-ui`, and open a separate Draft PR into `main`; do not merge it.**
- [ ] **Step 5: Report actual command results, changed files, limitations, commit hash, and Draft PR link.**

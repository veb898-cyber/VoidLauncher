# ContentManager Compatibility Diagnostics Design

## Intent

Integrate the existing Rust Compatibility Engine into the Mods view of
`ContentManager`. Users should see actionable diagnostics for the installed
mod set without conflating them with the existing Modrinth/CurseForge catalog
version check.

## Goals

- Analyze the selected instance's actual installed and disabled mod files.
- Reuse `InstalledContent`, `get_mod_metadata` and its persistent metadata
  cache, and `analyze_installed_mods`.
- Return the complete `CompatibilityReport` through a Tauri command without
  blocking the UI runtime or adding network work to the analyzer.
- Show a compact summary with a collapsible, filterable list of all findings,
  their certainty, participants, versions, source, files, and explanation.
- Support manual rechecks and refresh after watcher or manager mutations.
- Make stale, incomplete, failed, and empty results visibly distinct.

## Non-goals

- Automatic conflict remediation, deletion, disabling, or install/update
  candidate checks.
- Changes to installation transactions, launch gating, or installed versions.
- Replacing or merging the existing Modrinth/CurseForge catalog check.
- A new filesystem watcher; use the existing `instance_dir_changed` watcher.

## Approaches

1. **Dedicated local-diagnostics command (chosen).** Add a command that resolves
   the supplied instance name, gathers the installed snapshot and cached JAR
   metadata on a blocking worker, then calls the pure analyzer. This keeps the
   catalog endpoint and local report separate while reusing the current cache.
2. Extend `cmd_get_mod_metadata` to include a compatibility report. This saves
   a command round trip, but couples listing metadata to analysis and broadens
   an existing endpoint's contract.
3. Analyze on the frontend. Rejected because it would duplicate the Rust
   analyzer and weaken the source of truth.

## Architecture and contracts

- A dedicated `cmd_analyze_instance_mod_compatibility(instance_name)` returns
  `CompatibilityReport`. It clones configuration before spawning blocking
  work, resolves the actual `Instance`, reads the `mods` snapshot, calls the
  existing cached metadata reader, then invokes `analyze_installed_mods`.
- Unreadable directories or command failures return an error; they never
  become an empty successful report. Corrupt or incomplete JAR metadata remains
  represented as `unknown` by the analyzer. A truly empty mods directory is a
  distinct normal result.
- Dependency findings gain an optional structured detail containing the
  dependency kind/ID, declared requirement (including alternatives), and
  detected provider filenames and versions. Existing status, basis, source,
  explanation, affected files, and IDs remain authoritative and unchanged.
- `ContentManager` maintains a second state for local diagnostics. Existing
  catalog compatibility results and their command remain independent.
- The summary reports total findings, confirmed, potential, unknown, report
  completeness, and the local time the report was received. A complete empty
  report may say no issues; an incomplete report must never say fully
  compatible.
- The findings panel supports all/confirmed/potential/unknown filters and
  expands each item to show the complete explanation and available metadata.
- Rechecks run only for the Mods view, have visible loading/error/empty states,
  and are serialized. The report is invalidated when its source list changes;
  request generations prevent responses for prior instances or snapshots from
  replacing current state.
- The existing watcher and `loadItems` mutation refresh paths trigger a new
  local report. Existing `get_mod_metadata` mtime/size cache remains the only
  JAR metadata cache.

## Constraints

- Keep the public version unchanged.
- Keep all new user-facing strings in English and Russian i18n catalogs.
- No direct filesystem access from React and no new network request for local
  analysis.
- Do not change compatibility certainty rules or use findings to block launch.

## Verification

Backend tests cover empty sets, complete reports, dependency details, missing
or unreadable inputs, incomplete metadata, and preservation of engine findings.
Frontend tests cover summary counts, completeness/error/empty states, filters,
multiple findings for one mod, independent catalog state, refresh coalescing,
instance switching, stale responses, and content-change invalidation. The full
task also runs Rust tests, Clippy, formatting, TypeScript, frontend tests, and
the production frontend build.

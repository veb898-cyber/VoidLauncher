# Known launch-composition issues

Status after M4/A4. KI-1 through KI-4 are fixed and their regression tests run
with the normal suite in `launch_known_issues.rs`. A failing test means the
defect came back; the expectation must not be relaxed to make CI pass.

Run the integration target explicitly with:

```powershell
cd src-tauri
cargo test --test launch_known_issues -- --nocapture
```

## Fixed defects

| ID | Defect | Resolution |
|---|---|---|
| KI-1 | Stale native files from a previous build were never replaced. | Extraction stages the complete set and syncs it into `versions/<id>/natives` with an ownership manifest. A failed preparation leaves the previous working set untouched. |
| KI-2 | A corrupt native archive was silently skipped. | Reading or extracting a required native archive now fails preparation with the library name, archive path and stage; no partial set is installed. |
| KI-3 | `versions::native_classifier` replaced `${arch}` with the literal `64`. | Native selection now receives an explicit `TargetArch`; `${arch}` maps to `32`/`64`/`arm64` for x86/x86_64/ARM64 and is not resolved at all for an unknown target. Legacy plain classifiers are used as-is. |
| KI-4 | Mojang `arm64` argument rules never matched (`aarch64` was compared with `arm64`). | OS/arch rules and native selection share the same `TargetArch` mapping, so `x86`, `x86_64` and `arm64` match exactly. |
| KI-6 | Concurrent native preparation of the same version collided on Windows: a `rename` between two preparers hit a delete-pending destination (`os error 5`), a leftover install temp could be swept as a stale native and pull its directory out from under another preparer, and a delete-pending destination failed the preflight write-open. | `install_native_file` and the manifest write share a short retrying atomic replacement (no locks); launcher `.voidlauncher-*.tmp` files are exempt from the stale sweep and from the "our files" listing; the preflight write-open retries transient permission errors briefly instead of reporting a lock. Locked and read-only destinations still fail with their path (KI-1 semantics unchanged). |

KI-5 (`${classpath}` inside loader JVM arguments) was fixed in M2: the
placeholder receives the final composed classpath, and any unresolved `${...}`
fails the launch instead of reaching Java. Its regression test lives in
`tests/launch_command.rs`
(`loader_jvm_classpath_placeholder_is_expanded_to_final_classpath`).

Real Mojang metadata does **not** use `${arch}`: 1.12.2, 1.5.2 and 1.6.4 use a
plain `natives-windows` classifier. Modern manifests (1.19+) list
`natives-windows`, `natives-windows-x86` and `natives-windows-arm64` as separate
libraries with only an OS rule, so the launcher filters those variants by the
Maven classifier suffix and never puts another architecture's natives on the
classpath.

## Not verified at runtime

- **Windows ARM64**: selection and rules are covered by table tests, but there
  is no ARM64 build or ARM64 smoke run. Do not declare ARM64 runtime support
  until a real launch is verified.
- **x86**: mapped and tested, but VoidLauncher does not ship an x86 build and
  managed Java is always x64 (`java_download.rs` requests `architecture=x64`).
- **Source of the target architecture**: `TargetArch::current()` currently
  derives from the launcher binary's compile-time target. M3 must replace it
  with the selected JVM's `os.arch`; until then a native ARM64 launcher with an
  x64 managed JVM would select the wrong natives.
- **macOS** is out of scope (Windows-only project); `os.name` normalisation for
  Mojang's `osx` token is not implemented.

## Recorded behaviour that is intentional (not a bug)

These are asserted by the normal suite because they are deliberate policy:

- `--clientId ""` and `--xuid 0`: VoidLauncher does not have Xbox Live
  device/user identifiers; the placeholders are resolved to empty/zero instead
  of being left as `${clientid}` / `${auth_xuid}`.
- `--userType msa`: modern manifests still ask for it. 26.x dropped
  `--userType` from the manifest, so the launcher does not invent it there.
- For legacy Forge, the profile's `-Djava.library.path` / launcher brand/version
  properties are repeated by the manifest fallback. The values are identical;
  duplicate `-D` properties are harmless.
- Vanilla launches go through `VoidLauncherEntry.jar` and
  `-Dvoidlauncher.mainClass=...`; loader launches use the loader's main class
  directly. This is required by the JPMS behaviour of modern JDKs.

## A4 audit: library deduplication (M4)

Audited the final library list produced by merging vanilla metadata with loader
profiles, replicating the A3 include rules (Windows / x86_64) across local
metadata for all installed versions (1.0 … 26.3) and all three loader fixtures
(Fabric 0.16.9/1.20.1, Forge 14.23.5.2860/1.12.2, NeoForge 21.1.172/1.21.4).

Findings:

- **No version conflicts exist in the final list.** No two *different*
  versions of the same `group:artifact` are ever included together.
- All "conflicts" reported by the audit are **intentional classifier
  variants**: LWJGL 3.3.1/3.3.4 main JARs plus `natives-windows` JARs of the
  same version, and single-version pairs like `text2speech`. These are
  separate artifacts by design (natives are extracted, main JARs are
  classpath); deduplicating them by `group:artifact` would break native
  extraction.
- Older metadata (1.0–1.17) contains literal duplicate entries with the
  **same download path** (LWJGL 3.1.6/3.2.1/3.2.2 across ranges, `text2speech`
  in 1.12.2/1.13–1.17, `jinput-platform` in 1.0/rd-132211). These collapse
  through the existing exact-path dedup in `launch::build_launch_profile` and
  `dedupe_download_targets`; `assert_classpath_sane` (no duplicate classpath
  entries) passes for every golden fixture, and
  `neoforge_overrides_vanilla_asm_and_keeps_loader_first` already pins the
  loader-wins override semantics. No code change was needed.
- **Hypothetical, not observed**: a loader profile shipping a library with the
  same `group:artifact` as a vanilla *native* cluster would drop the vanilla
  native JAR (override logic works per `group:artifact`). No real loader does
  that, so it is recorded here rather than "fixed"; the per-path dedup and the
  override test cover the paths that exist today.

## A4 audit: concurrent native preparation (M4)

`extract_natives` was audited for two simultaneous calls targeting the same
`versions/<id>/natives`, plus the manifest and staging interactions. Found and
fixed (see KI-6), deliberately without adding locks:

1. **Replace race**: thread A between its `remove_file` and `rename` leaves the
   destination delete-pending; thread B's `rename` then fails with `os error 5`.
   Fixed by a short bounded retry around the atomic replacement, shared by the
   native install and the manifest write.
2. **Temp sweep race**: during a first preparation (no manifest yet) the
   fallback "what is ours" listing includes other methods' in-flight
   `.voidlauncher-*.tmp` files; a stale sweep could delete them (and, after
   removing files from a subdirectory like `nested/`, even drop that
   directory) right under another preparer. Fixed by exempting
   `.voidlauncher-*.tmp` names from both the sweep and the listing.
3. **Preflight race**: the preflight write-open can hit a same-moment
   delete-pending destination and misreport a genuine lock. Fixed by a short
   retry on transient permission errors; truly locked or read-only files still
   fail with their path.

Deterministic regression test:
`concurrent_native_preparation_of_the_same_version_converges` (4 threads
released through one shared barrier, two consecutive builds): all callers
succeed, no staging leftovers, final listing is exactly the desired set with
the current build's bytes, and the manifest matches the extracted set.

Residual, accepted risk (no fix, self-healing): with *different* native sets in
flight (the version changed between two simultaneous launches), one call may
observe a partially replaced directory; the next launch's complete staging and
manifest sync brings the directory to the current set. A game already running
keeps KI-1's "locked file must not be silently replaced" behaviour.

## Open questions for later stages

- The natives directory is per Minecraft version
  (`versions/<id>/natives`), so two concurrently running instances of the same
  version share it. `extract_natives` stages a complete set and skips files
  whose bytes already match, so a second launch does not rewrite natives that
  the first one has loaded. If the set changed while an old game is running, a
  locked/read-only file fails preparation with its path instead of being
  silently replaced or skipped.
- `build_classpath_entries` does not deduplicate or apply loader overrides;
  ordering/deduplication now happens in the pure `launch::build_launch_profile`
  and is covered by the M2 semantic tests.

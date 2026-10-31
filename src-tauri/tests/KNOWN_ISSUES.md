# Known launch-composition issues

Status after M4/A3. KI-1 through KI-4 are fixed and their regression tests run
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

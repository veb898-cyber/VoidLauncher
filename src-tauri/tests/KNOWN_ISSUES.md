# Known launch-composition issues

Status as of the M7.1–M7.3 test-infrastructure stage. These defects are
**recorded, not accepted**: the normal suite never asserts them as correct.
Each one has an `#[ignore]`d test in `launch_known_issues.rs` that encodes the
desired behaviour, so fixing the bug makes the test fail-to-pass and reminds us
to remove the entry here.

Run them explicitly with:

```powershell
cd src-tauri
cargo test --test launch_known_issues -- --ignored --nocapture
```

| ID | Issue | Impact | Planned stage |
|---|---|---|---|
| KI-1 | `versions::extract_natives` skips a destination file that already exists. | A DLL left over from an older download (or a half-written extraction) is never replaced. Stale/corrupt natives can crash the game after an update. | M4 (natives handling) |
| KI-2 | `versions::extract_natives` silently ignores a native classifier JAR that exists but cannot be read/unzipped. | A corrupt download degrades to a runtime crash without a launcher error. Download verification should catch it earlier, but launch must not pretend extraction succeeded. | M4 |
| KI-3 | `versions::native_classifier` replaces `${arch}` with the literal `64`. | Correct on x64, wrong on Windows ARM64 where Mojang publishes `natives-windows-arm64`. Confirmed against the 1.12.2 manifest (the only committed fixture that uses `${arch}`). | M4 (after verifying the real classifier set) |
| KI-4 | `versions::argument_rule_allows` compares Mojang's `arm64` against `std::env::consts::ARCH` (`aarch64`), so `arm64` rules never match. The same function compares `os.name` against `std::env::consts::OS`, which is `macos` while Mojang metadata says `osx`. | Wrong arguments/natives on Windows ARM64 and macOS. No effect on Windows x64. | M3 (`RuntimeContext`-style platform normalisation) |

KI-5 (`${classpath}` inside loader JVM arguments) was confirmed as a real
mechanism-level defect and fixed in M2: the placeholder now receives the final
composed classpath, and any unresolved `${...}` fails the launch instead of
reaching Java. The regression test lives in `tests/launch_command.rs`
(`loader_jvm_classpath_placeholder_is_expanded_to_final_classpath`).

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
  version share it. Any future "clean the directory before extraction" change
  (M4) must be designed around concurrent launches.
- `build_classpath_entries` does not deduplicate or apply loader overrides;
  ordering/deduplication now happens in the pure `launch::build_launch_profile`
  and is covered by the M2 semantic tests.
- Modern manifests list `natives-windows`, `natives-windows-arm64` and
  `natives-windows-x86` as separate libraries whose only rule is
  `{"os":{"name":"windows"}}` — there is no `arch` condition to filter them.
  On x64 the normal suite therefore puts all three JARs on the classpath and
  extracts all three native sets (7 LWJGL modules each in the 1.20.1 golden).
  M4 must check against the reference launchers whether they filter these
  classifier suffixes by architecture themselves, or whether shipping the
  extra JARs is harmless.
- Related inconsistency to unify in M3: `should_include_library` treats an
  `x86` rule as matching `x86_64`, while `argument_rule_allows` treats `x86`
  as never matching. Both should consult one platform context.

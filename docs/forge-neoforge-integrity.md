# Forge and NeoForge integrity model

This note documents checksum verification for Forge and NeoForge loader
artifacts. Fabric, vanilla downloads, Minecraft assets and content-manager
downloads keep their own existing integrity paths.

## Sources

| Loader | Artifact | Metadata source | URL origin | Checksum |
| --- | --- | --- | --- | --- |
| Forge | installer JAR | official Maven artifact | `maven.minecraftforge.net` | `.sha512` / `.sha256` / `.sha1` sidecar |
| Forge | `version.json` libraries | installer metadata (`sha1`, optional `sha256`/`sha512` + size) | `maven.minecraftforge.net`, `libraries.minecraft.net` | metadata first, repository sidecar only when metadata has none |
| Forge | processor libraries | `install_profile.json` artifact metadata | `maven.minecraftforge.net`, `libraries.minecraft.net` | metadata first, repository sidecar fallback |
| NeoForge | installer JAR | official Maven artifact | `maven.neoforged.net/releases`; allowlisted `maven.creeperhost.net` fallback; BMCLAPI body mirror (see below) | `.sha512` / `.sha256` / `.sha1` sidecar |
| NeoForge | `version.json` libraries | installer metadata (`sha1`, optional `sha256`/`sha512` + size) | `maven.neoforged.net`, `libraries.minecraft.net` | metadata first, repository sidecar fallback |
| NeoForge | processor libraries | `install_profile.json` artifact metadata | `maven.neoforged.net`, `libraries.minecraft.net` | metadata first, repository sidecar fallback |
| Forge/NeoForge | generated processor outputs | `install_profile.json` `data` / `outputs` | local Java processors from verified inputs | SHA-1 from installer metadata when declared; structural check otherwise |

The BMCLAPI host is only a body-transport mirror for NeoForge Maven paths. The
initial metadata/artifact source still has to match an allowed origin, and the
resulting bytes are accepted only after the publisher/repository checksum
passes. A body mirror is not a publisher and does not provide independent
authentication.

`url` + `path` in `instance.json` may be either a complete artifact URL (the
current format) or a root Maven base URL (defensive compatibility). The
resolver accepts a complete URL when its path already ends with the library
path, when its final filename exactly matches the filename in `path` while the
Maven directory differs (official Forge 1.13.2/1.14.4/1.15.2 shadowed/service
classifiers), or when it is a root base that can be joined once. The metadata
`path` always determines the destination; the URL only determines the source.
Exact-host HTTPS, path safety and checksum verification remain mandatory.
Ambiguous values are rejected instead of producing a doubled URL.

The three URL states are deliberately distinct:

- `url: Some("")` — explicitly embedded in the verified installer;
- `url: Some(URL)` — an ordinary remote Maven artifact;
- `url: None` — the historical Maven fallback, **not** embedded. The coordinate
  in `path`/`name` is resolved once through the trusted repository policy; if
  the source cannot be established unambiguously, the operation fails. A
  whitespace-only URL is malformed, not embedded.

An empty artifact URL means the JAR is embedded in the installer. It is not
downloaded from Maven and it is not silently skipped. Embedded libraries are
read from the `maven/<library-path>` entry of the verified installer, checked
against metadata SHA-1/SHA-256/SHA-512 when present, and otherwise bound to the
exact bytes of that verified installer entry. Extraction goes through a
temporary file and an atomic replacement. A non-empty malformed embedded
checksum is an integrity issue; it is not treated as "no checksum". Install,
Repair and Launch all use this same path, including Forge 1.12.2 where the
installer has no processors.

Historical Forge installer metadata (1.7.10, 1.8.9, 1.10.2 and 1.11.2) may
omit both `downloads` and `url` for game-side libraries such as
`net.minecraft:launchwrapper:1.12`. Such a coordinate is not assigned to an
arbitrary public Maven. A closed coordinate policy maps known Mojang/library
groups to Mojang Libraries and loader groups to the selected loader Maven;
unknown groups fail closed. The same policy is used by profile construction,
processor-library fallback, Install, Repair and Launch. Verify uses the
persisted typed checksum metadata and, being read-only, does not resolve a new
origin.

For a persisted profile that still contains the old Forge-Maven fallback URL
for a known Mojang coordinate, checksum lookup first tries the explicit URL and
then the known Mojang Libraries origin only when the explicit origin has no
sidecar. A malformed sidecar or a non-404 error does not trigger a weaker
fallback. The selected checksum is still verified before publication; the
sidecar remains a same-origin consistency check, not independent publisher
authentication.

## Asset index bytes

The asset index is a publisher artifact, not a launcher-normalized document.
The launcher downloads the raw JSON bytes, verifies the SHA-1 and size from the
official version metadata **before publication**, and writes those exact bytes
to `assets/indexes/<id>.json`. Parsing into `AssetIndexData` happens afterwards
and only adds in-memory defaults (`virtual: false`, `map_to_resources: false`);
it never rewrites the stored publisher file.

Install, launch-time restore and Repair all use this path. Verify compares the
stored file's raw SHA-1 with the publisher checksum. A legacy launcher-
normalized index therefore shows as corrupt until Repair replaces it with the
exact publisher bytes; it is never declared publisher-verified merely because
it parses. Structural validation is shared by every consumer: empty `objects`,
missing/wrong-typed required fields, unsafe asset names, malformed hashes and
zero object sizes are rejected. Verify and Repair load an existing trusted
index through the validated helper, so a checksum-matching but unusable index
cannot be reported Healthy or repaired as a no-op. If the high-level structure
is invalid, it is rejected before the atomic replacement, so the previous
working index remains on disk.

The download is streamed with a hard 32 MiB cap on the retained response body;
`Content-Length` is only an early rejection hint. A chunked response cannot
bypass the cap: the excess chunk is rejected before it is appended. This is a
cap on retained payload, not an absolute process peak: while a chunk is being
read, and because `Vec` may reserve growth headroom, transient memory can exceed
32 MiB by roughly the current transport chunk plus allocator slack. No partial
index is published. Publication is serialized
per target path within the single launcher process with a unique temporary
file. A second caller that presents a different non-empty publisher checksum
for the same index id is rejected instead of silently winning
last-writer-wins. Missing and malformed publisher
SHA-1 values are separate: empty means explicitly unverified, while a
non-empty malformed value fails closed.

## Trust priority

1. SHA-512 from trusted installer/profile metadata.
2. SHA-256 from trusted metadata.
3. SHA-1 from trusted metadata.
4. SHA-512/SHA-256/SHA-1 sidecar from the artifact's origin, recorded as
   `checksum_source = "repository"`.

A missing or exactly empty metadata field means “not provided”. A non-empty
malformed value fails closed and never falls back to a weaker hash. A
repository sidecar is a same-origin consistency check; it is not independent
publisher proof.

## Origins and redirects

- Forge artifacts are limited to `https://maven.minecraftforge.net/` and
  Mojang's `https://libraries.minecraft.net/` for libraries that metadata
  explicitly points there.
- NeoForge artifacts are limited to `https://maven.neoforged.net/releases/`,
  `https://maven.creeperhost.net/` and `https://libraries.minecraft.net/`.
  The shared download pipeline may additionally use
  `https://bmclapi2.bangbang93.com/maven/` as a body mirror; any mirrored body
  still has to match the publisher/repository checksum.
- Historical metadata without a library URL uses the closed origin policy
  above: known Mojang coordinates go to `https://libraries.minecraft.net/`,
  loader coordinates to the selected loader Maven, and unknown coordinates
  fail closed.
- `maven.creeperhost.net` is an allowlisted fallback used by this launcher when
  the primary NeoForge Maven is unavailable. This describes the current code,
  not an assertion that NeoForged endorses the host or that it appears in an
  official mirror registry; the repository does not independently verify such
  external claims. It is used only with the same coordinates/path as the
  primary metadata, never constructed independently. It is not a publisher:
  its artifacts are accepted only with the same checksum verification, and its
  checksum is not independent proof of authenticity.
- Sidecar checksums are fetched from the exact origin of the selected
  artifact with an exact-host redirect policy and a 4 KiB response cap.
- The initial artifact URL also requires an exact allowed host. The artifact
  body download still goes through the shared download pipeline's allowlist
  and redirect checks, so it may follow an allowlisted redirect or use a
  same-path mirror; the publisher checksum remains mandatory before
  publication. Exact-host applies to the sidecar/initial source, not to every
  body-transport hop.

## Publication and rollback

All Maven artifacts use the existing verified download pipeline:

1. `.part` staging file;
2. checksum/size verification;
3. atomic rename into `libraries/`;
4. temporary-file cleanup.

On failure the previous working file is kept, a mismatched artifact is not
published, no invalid `.part` remains, and the profile is not updated with a
failed checksum. Installer caches are accepted only after their checksum has
been verified.

## Verify, Repair and launch

- Verify uses `LoaderLibrary::checksum()` for profile libraries, so SHA-512,
  SHA-256, SHA-1 and repository checksums are all honored.
- Verify also performs a read-only processor-generation inspection. It checks
  the cache/installer identity, transaction journal, generation manifest,
  expected output paths and hashes, and embedded libraries. A missing or
  corrupt generated output, a wrong NeoForge client JAR, a missing/stale
  manifest, a pending transaction, a damaged committed transaction or a corrupt
  journal is reported as an issue. `Recovery Required` and `Busy` are never
  reported as healthy.
- Repair resolves the original artifact URL without double concatenation,
  obtains a checksum from metadata or the permitted repository sidecar, and
  replaces a missing/corrupt profile library through the same atomic pipeline.
  It also inspects processor generation when the loader profile already exists
  and runs the ensure path for fixable states, so a present profile is not
  treated as proof of healthy generated outputs or embedded libraries.
- Launch-time self-healing runs the generation ensure path and then verifies
  the profile libraries before the classpath is built. It checks/re-extracts
  embedded libraries from the verified installer, validates the generation
  manifest/journal, and aborts if safe recovery is impossible.
- The launch `Starting` reservation is shared with the blocking launch worker
  and owned by the child-process guard once the game exists. The guard is
  created immediately after `launch_minecraft` returns the spawned child;
  output-reader attachment runs only after that ownership transfer. If the
  command future is dropped before the process is registered, the guard stops
  and reaps the just-spawned game (including its helper tree) and only then
  releases the reservation. A failed stop or failed wait/reap keeps the child
  and lease under a pending reaper, which retries with diagnostics. If the
  reaper thread cannot be created (reported error or panic), cleanup runs
  synchronously in the owning path; the reservation remains busy until a
  confirmed reap. Cleanup is never reported as successful while the child may
  still be alive. If repeated wait or reap attempts cannot confirm process
  exit, the instance remains in the
  fail-closed `Starting` state with diagnostic logs rather than being reported
  as free; recovery may require restarting the launcher.
- Install, Repair and launch prepare the processor JVM through the existing
  Java selector. When no compatible Java is installed, the existing managed
  Java download is reused; install/failed-download errors are reported instead
  of being promised as a download that never runs. A user-configured Java path
  is never silently replaced. The existing managed-Java downloader is outside
  PR #20: if Adoptium omits or returns a malformed archive checksum, that
  inherited downloader may fall back to TLS-only transport; this is not
  claimed as publisher-authenticated Java. That limitation is tracked
  separately from Forge/NeoForge artifact verification.
- When Repair returns a summary, it is kept separately from the subsequent
  Verify report: repaired/failed counters and error details remain visible
  after the automatic re-verify, and a partial Repair is never presented as a
  successful one. An early command error is shown as a Repair error; it is not
  converted into a green Verify report. The
  modal keys its Verify cache by instance/scope/mode, drops it when the dialog
  closes or the instance changes, ignores late responses from an older
  instance, and clears the old Verify report when a new Repair starts or fails.
  A forced Re-verify drops the previous cache entry before the request and on
  failure, so switching away and back cannot resurrect a pre-failure Healthy
  report.
- Fabric continues to use its own resolver and is not mixed into these paths;
  embedded-installer handling is Forge-specific.
- Verify remains read-only and never rolls back a journal. The GUI command
  reserves the instance with an owned edit guard and moves that guard into the
  blocking scan worker. Repair/Install/Update/launch preparation therefore
  cannot start midway and produce a mixed report. A normal return or panic
  releases the guard; if the async command future is dropped/cancelled while
  `spawn_blocking` is still reading, the worker keeps the reservation until it
  actually finishes. The result of a cancelled command is not published. If
  the instance is already starting, running, editing or preparing, Verify
  returns Busy. The loader cache inspection additionally uses a non-blocking
  operation-lock attempt and reports `verification_incomplete`/Busy instead of
  returning a partial healthy report.
- The instance identity used by Verify, Repair and the logs UI is the existing
  `Instance::name`, which is also the instance directory name for launcher-
  managed instances. Create, save/rename, import and duplicate enforce
  case-insensitive uniqueness of that name (including the name reservation used
  while a directory operation is in flight), so two launcher instances cannot
  share it. No separate UUID or canonical-path identity is stored; this
  document does not claim that two externally copied folders with hand-edited
  duplicate names are distinguishable.

## Legacy profiles and offline behavior

Old Forge/NeoForge profiles with only `sha1`, `size`, `url` and `path` remain
readable. Missing checksums are resolved from the permitted metadata or
repository sidecar, the existing file is verified, and the provenance is
persisted on install/update or a successful launch-time migration. Repeated
verification uses the saved checksum without a network lookup.

Missing, exactly empty and malformed checksums are separate states. For loader
libraries, install/repair/launch fail closed when no checksum can be obtained.
Verify reports a missing/empty checksum honestly as `unverified`, never as
verified; a non-empty malformed checksum is an explicit integrity error. For an
Asset Index, an empty publisher SHA-1 is likewise explicitly unverified, while
a malformed non-empty SHA-1 fails closed. The existing file is not deleted and
no synthetic checksum is written. The first online preparation of an un-migrated
profile is therefore required.

An instance created by an older launcher build does not have the persistent
`loader-cache` provenance. Its first install, Verify/Repair generation check or
launch after the upgrade may therefore require one online installer download,
even when generated outputs already exist. The installer is publisher-verified
before it is cached; if the network is unavailable, the operation fails closed
rather than trusting the old outputs structurally.

## Generated processor outputs

Forge and NeoForge do not download the patched client, slim, extra or SRG JARs
as ready-made files. They are created locally by the installer's Java
processors from the verified installer, the verified processor libraries, the
verified vanilla client JAR and the installer-embedded patch data. The
processor binaries are themselves resolved as typed `LoaderLibrary` entries
(metadata checksum first, same-origin sidecar fallback).

The installer profile declares SHA-1 values for the client-side outputs it can
identify (`MC_SLIM`, `MC_EXTRA`, `PATCHED`, and their `*_SHA` companions). Those
values are publisher metadata, not a hash computed after generation, and are
verified before the generated output is accepted. A successful processor exit
is not enough. `MC_SRG` has no declared hash in the profiles audited here; it is
validated structurally, while the final patched client is checked against its
metadata hash.

Existing outputs are reused only when the persistent generation manifest
matches the current installer, `install_profile.json`, client JAR, processor
libraries, processor definitions, expected target paths and output hashes. A
loader update changes the version-scoped cache and output paths, so an old
output cannot silently satisfy the new profile. Processor-generated files are
never treated as publisher-signed downloads, and no synthetic checksum is
written for them.

## Persistent installer cache and provenance

The verified installer and its metadata live under the application data
directory, not `%TEMP%`:

```
<data_dir>/loader-cache/<loader>/<mc_version>/<loader_version>/
  installer.jar
  installer.json       # installer publisher checksum + local file identity
  generation.json      # inputs and generated-output provenance
  tx/                  # staging, backups, journal.json + journal.prev.json
```

`installer.json` records the exact installer file hash/size, the publisher
checksum algorithm and value, its source (`repository` sidecar) and URL, plus
the loader/Minecraft/loader versions. It is never accepted without matching the
actual installer file and checksum.

`generation.json` binds a successful generation to:

- the installer identity and `install_profile.json` SHA-256;
- the client JAR hash and size;
- every downloaded processor library, its path and SHA-256;
- each processor's main JAR, classpath and arguments;
- every generated target path, publisher SHA-1 (when declared), local SHA-256
  and size, and whether the output is structural-only;
- a manifest schema version.

The manifest is a local history record, not a signature or publisher
authentication. It detects accidental corruption, stale outputs and the common
"wrong loader/version" mistakes. It does **not** protect against malware or a
malicious process running as the same Windows user: such a process can modify
the installer, generated JARs and manifest together. Windows per-user ACLs on
the application-data directory protect against other standard users, not
against code running inside the same user context. No DPAPI token, HMAC or
secret is claimed as protection here; a secret stored in the same user profile
would not stop that attacker.

If the persistent installer/metadata is missing, damaged or has a mismatched
identity, the installer is downloaded and publisher-verified again. This
includes syntactically valid `installer.json` files whose checksum algorithm or
hex value is malformed: such a record is treated as untrusted metadata, not as
a fatal error, so the normal re-download path runs. A read/permission failure is
still surfaced instead of being disguised as malformed metadata. If recovery is
impossible (offline/cache loss), the operation fails closed. Existing generated
outputs are not accepted on structural validation alone. Existing files are
left in place until a replacement set has been verified and committed.

The NeoForge standalone profile JSON is only an optional optimization. It is
fetched with an exact-host HTTPS client, bounded response size and a required
same-origin repository checksum sidecar (`checksum_source = "repository"`);
this is a consistency check against the repository's own bytes, not an
independent publisher signature. Redirects to another host, HTTP redirects and
missing/malformed checksums cause it to be ignored. The verified installer JAR
is the authoritative source when the standalone JSON is unavailable. A network
failure never causes a transition from the installer to a less trusted source.

## Processor publication transaction

Generated outputs are written into `tx/staging/`; target files under
`libraries/` are not touched during processor execution. After all processors
finish, the staged outputs are verified and a transaction journal is written
with the target, staging and backup paths, old/new hashes and output
provenance. Previously existing targets are moved to `tx/backup/` before the
staged files are moved into place.

Embedded installer libraries use a separate, smaller publication path: the
exact installer entry is read into memory, checked against trusted metadata (or
bound to the verified installer entry when no checksum exists), written through
a flushed temporary file, and atomically renamed. A failed extraction or
verification leaves the previous working file in place. These writes are inside
the same per-cache lock as processor publication, so two instances sharing one
Forge version cannot publish a partial embedded JAR concurrently.

The journal is written twice (`journal.json` and `journal.prev.json`) with a
monotonic generation and a content hash. The **secondary** copy is the
authoritative copy. Before advancing its generation, the stale primary mirror
is deleted while the authoritative copy remains untouched; if that
invalidation fails, the operation aborts and the old generation is not
advanced. Only after successful invalidation is the new authoritative
secondary written (its atomic write/`sync_all` is the commit point), followed
by a new primary mirror. A secondary failure leaves the previous authoritative
generation intact; a primary-only failure leaves the new authoritative copy
alone, so recovery fails closed rather than selecting an older mirror if that
copy is later damaged. Both atomic replacements use a flushed temporary file,
so a failed `sync` is reported instead of being silently accepted. One
randomly damaged copy is recovered from the other; this is resilience against
accidental corruption, not authentication: a process running as the same user
can rewrite both copies consistently. If both copies are damaged, recovery
fails closed and preserves backups/staging for a later repair instead of
guessing which files are old.

`libraries/` may be a junction to another volume. Publication and rollback do
not assume that a rename across the cache volume and the target volume can
succeed: on Windows `ERROR_NOT_SAME_DEVICE` triggers a verified copy into a
temporary file on the destination volume followed by a same-volume atomic
replacement. The source is removed only after the destination hash matches.
This covers `target -> backup`, `staging -> target` and `backup -> target`
(rollback already copies through a target-local temporary file).

The journal has `prepared` and `committed` states:

- every journal path is checked as a relative path below `libraries/`,
  `tx/staging/` or `tx/backup/`; parent components, root/drive/UNC prefixes,
  alternate data stream names, trailing-dot/space aliases and reparse points
  are rejected before any rename;
- duplicate or overlapping target/staging/backup paths, malformed hashes and
  inconsistent provenance flags are rejected before the first old target is
  moved;
- an oversized (over 4 MiB), unreadable, non-regular or malformed journal is
  an error, never "no transaction"; a pending journal also prevents the
  generation manifest from being trusted;
- **prepared** means the new set is not committed. Any crash or failure rolls
  back the complete output set from the backups; newly created targets are
  removed only when their hash matches the staged output. Before the first
  target is changed, rollback verifies that every required backup exists and
  matches its old hash, so a missing/corrupt backup cannot leave a partially
  restored old set;
- rollback copies a backup to a temporary file, verifies it, replaces the
  target and verifies the restored hash before deleting the backup. A failed
  restore therefore leaves the last recoverable old file in place;
- **committed** means the new set is final. If every target is a structurally
  valid JAR with the recorded hash (and publisher checksum, when one was
  declared), recovery only removes backups/staging. If any target is damaged,
  recovery does **not** roll back: backups may already have been partially
  deleted, and restoring only part of the old set would create a mixed
  generation. The transaction becomes Recovery Required, leaves all remaining
  backups/staging and the journal in place, and blocks the generation
  manifest;
- recovery is idempotent and can itself be interrupted and resumed.

Processor publication, recovery and embedded-library extraction for different
instances that share one loader version are serialized by a process-wide
per-cache lock. The lock is transferred into the blocking processor worker
itself (not held only by the async command frame), so dropping/cancelling the
outer command cannot free the cache while Java is still writing staging. A
`spawn_blocking` task is not aborted after it starts; if cancellation occurs,
the worker safely continues to completion under the owned lock, and no new
generation may begin until it exits. The UI/API must not describe that as an
already-stopped operation. Installer archive and `installer.json` identity
publication also take the same per-cache lock, so two instances cannot
interleave a verified installer with metadata from a different generation.
Installer/library downloads additionally use the downloader's per-path lock and
atomic `.part` publication. The per-cache lock covers the complete prepare ->
recover -> installer publication -> embedded extraction -> stage -> publish
sequence; it does not make profile-metadata writes or unrelated shared
downloads a single transaction. A second launcher process is refused by the existing single-instance
mutex; the locks are not claimed to protect against a same-user attacker or a
separately started process that bypasses that mutex.

Asset Index publication uses its own per-target serialization because the index
is shared by every instance using the same Minecraft version. The temporary and
backup names are unique, the final bytes are re-read and verified after the
atomic replace, and a conflicting non-empty expected publisher checksum for the
same index id is rejected before writing. Within that process, a verified
target is not replaced by bytes that have no publisher checksum. Validation
and write failures before the atomic replace leave the old working index in
place; the bytes are fully validated before the replace. A successful replace
followed by a failed re-read or verification is reported as a failure, but the
previous file is not restored on that path. Such a post-replace failure is
fail-closed rather than a weaker fallback.
Install, launch, import and Repair all prepare the shared index path through
the same reparse-point boundary check.

Process-kill crash recovery is tested with a separate test process at four
publication stages (before the first rename, after the first, after all
renames before commit, and after commit), including corrupted and oversized
journal copies after partial publication. A Windows test also uses a real
`libraries/` junction to another local volume when one is available and
exercises both publication and rollback across volumes. Power-loss durability
is **not** claimed: the journal is flushed, but NTFS directory-entry durability
and rename ordering across a sudden power loss were not proven on this
platform.

## Known limits

- Installer data files extracted from an already verified installer inherit
  that installer's integrity; they do not have separate sidecars.
- Generated outputs without a declared publisher checksum (`MC_SRG`, some modern
  NeoForge outputs) rely on the local provenance manifest only. This detects
  local corruption/staleness but is not publisher authenticity.
- The first launch of a legacy profile with no stored checksum requires online
  metadata or repository-sidecar access.
- The managed Java downloader is inherited outside PR #20. If Adoptium omits
  the archive checksum, or returns a malformed value, it may continue with TLS
  transport rather than publisher authentication. Its large archive also has no
  resume/retry for a mid-stream body failure. This is a separate future
  hardening item, not a Forge/NeoForge publisher verification guarantee.
- Process-kill recovery is tested; power-loss durability and same-user
  malicious tampering are not guaranteed.
- A committed generation with a damaged target becomes Recovery Required. The
  application does not automatically quarantine the damaged transaction or
  regenerate that output set; doing so safely requires the caller's verified
  installer and processor inputs. Remaining backups/staging are preserved for
  manual recovery. Verify reports the Recovery Required state. Repair inspects
  the same generation state even when a Forge profile already exists; it either
  uses the generation ensure path for fixable states or reports Recovery
  Required instead of silently claiming success. It does not delete backups. If
  a safe automatic regeneration is not implemented, the transaction remains
  Recovery Required.
- The concrete manual recovery path is to stop the launcher, preserve/archive
  the **entire** `loader-cache/<loader>/<mc_version>/<loader_version>/tx/`
  directory (journal, backups and staging), and only then reinstall the loader
  from the verified installer so the processor pipeline regenerates the full
  output set. Do not delete just the journal. If the verified installer or
  required processor inputs are unavailable offline, automatic regeneration is
  not possible and the profile remains Recovery Required.
- Cross-volume fallback is tested with a real local NTFS junction when a second
  local volume exists. Network shares and non-NTFS destination volumes may have
  different rename/replace semantics and are not covered by that test.
- Before advancing a journal generation, failure to remove the stale primary
  mirror aborts the step. The old generation remains valid, but no newer
  generation was written. After a successful advance, a failed primary mirror
  leaves only the authoritative secondary; if that copy is later damaged,
  recovery fails closed instead of selecting an older primary.
- A process killed during a cross-volume copy can leave a hidden
  `.rollback-*.tmp` file in the target directory. It is never treated as an
  output; journal recovery and later publication do not consume it.
- If both journal copies are corrupted at once, automatic recovery cannot map
  backups to targets; the transaction fails closed and keeps backups/staging
  for manual repair.
- Old loader files may remain in `libraries/` after an update; the new profile
  and manifest do not reference them.

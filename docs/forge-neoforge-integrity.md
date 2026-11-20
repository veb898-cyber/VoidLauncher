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
| NeoForge | installer JAR | official Maven artifact | `maven.neoforged.net/releases`, sanctioned `maven.creeperhost.net` mirror | `.sha512` / `.sha256` / `.sha1` sidecar |
| NeoForge | `version.json` libraries | installer metadata (`sha1`, optional `sha256`/`sha512` + size) | `maven.neoforged.net`, `libraries.minecraft.net` | metadata first, repository sidecar fallback |
| NeoForge | processor libraries | `install_profile.json` artifact metadata | `maven.neoforged.net`, `libraries.minecraft.net` | metadata first, repository sidecar fallback |
| Forge/NeoForge | generated processor outputs | `install_profile.json` `data` / `outputs` | local Java processors from verified inputs | SHA-1 from installer metadata when declared; structural check otherwise |

`url` + `path` in `instance.json` may be either a complete artifact URL (the
current format) or a root Maven base URL (defensive compatibility). The
resolver accepts a complete URL only when its path already ends with the
library path, or a root base that can be joined once. Ambiguous values are
rejected instead of producing a doubled URL.

An empty artifact URL means the JAR is embedded in the installer. It is not
downloaded from Maven; it is extracted by the processor pipeline.

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
- `maven.creeperhost.net` is the emergency fallback named by the official
  NeoForged website when the primary Maven is offline. The official website
  source also uses it for version-list fallback and records that CreeperHost
  granted permission. The official mirror registry
  (`mirrors.neoforged.net`) lists only `maven.neoforged.net/releases` as the
  primary, so CreeperHost is an emergency fallback rather than a listed
  mirror. It is used only with the same coordinates/path as the primary
  metadata, never constructed independently. It is not a publisher: its
  artifacts are accepted only with the same checksum verification, and its
  checksum is not independent proof of authenticity.
- Sidecar checksums are fetched from the exact origin of the selected
  artifact with an exact-host redirect policy and a 4 KiB response cap.
- Artifact downloads still go through the shared download pipeline's
  allowlist and redirect checks; the checksum is verified before publication.

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

- Verify uses `LoaderLibrary::checksum()` for Forge/NeoForge, so SHA-512,
  SHA-256, SHA-1 and repository checksums are all honored.
- Repair resolves the original artifact URL without double concatenation,
  obtains a checksum from metadata or the permitted repository sidecar, and
  replaces a missing/corrupt library through the same atomic pipeline.
- Launch-time self-healing uses the same metadata: it verifies every loader
  library, enriches legacy profiles with a repository checksum when needed,
  saves the upgraded profile, and aborts the launch if verification fails.
- Fabric continues to use its own resolver and is not mixed into these paths.

## Legacy profiles and offline behavior

Old Forge/NeoForge profiles with only `sha1`, `size`, `url` and `path` remain
readable. Missing checksums are resolved from the permitted metadata or
repository sidecar, the existing file is verified, and the provenance is
persisted on install/update or a successful launch-time migration. Repeated
verification uses the saved checksum without a network lookup.

If no checksum can be obtained, verification/repair/launch fails closed. The
existing file is not deleted and no synthetic checksum is written. The first
online preparation of an un-migrated profile is therefore required.

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

Existing outputs are reused only when the cached installer metadata still
describes the same paths and the declared hash matches. If `%TEMP%` was
cleaned, the fallback is structural only; a loader update changes the
version-scoped output paths so an old output cannot silently satisfy the new
profile. Processor-generated files are never treated as publisher-signed
downloads, and no synthetic checksum is written for them.

## Known limits

- Installer data files extracted from an already verified installer inherit
  that installer's integrity; they do not have separate sidecars.
- The NeoForge standalone profile JSON (used by older versions) is metadata
  fetched over the official HTTPS origin; the installer JAR itself and all
  downloaded libraries are checksum-verified.
- The first launch of a legacy profile with no stored checksum requires online
  metadata or repository-sidecar access. Generated-output fast-path checks are
  metadata-based only while the installer is still cached; a cleaned temp
  directory falls back to structural validation for outputs without a
  reachable installer.

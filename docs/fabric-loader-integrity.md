# Fabric Loader integrity model

This note documents the checksums used for Fabric Loader installation and
launch-time self-healing. It covers loader libraries only; Fabric API and other
game mods are content files handled by the Modrinth/CurseForge integrity paths.

## Artifact sources

| Artifact | Metadata | Checksum source |
| --- | --- | --- |
| ASM (`org.ow2.asm:*`), Sponge Mixin | `meta.fabricmc.net` profile JSON | strongest of SHA-512/SHA-256/SHA-1 + size |
| Fabric Loader (`net.fabricmc:fabric-loader`) | profile JSON (no checksum) | `maven.fabricmc.net` sidecar |
| Intermediary (`net.fabricmc:intermediary`) | profile JSON (no checksum) | `maven.fabricmc.net` sidecar |

Every library URL is normalized through a strict Maven-coordinate parser. The
base URL must be exactly `https://maven.fabricmc.net/` (HTTPS, no port, no
userinfo, no query/fragment). Destinations are safe relative paths under
`libraries/`; traversal, NTFS streams, absolute paths and reparse points are
rejected before any network request.

## Trust priority

1. SHA-512 from profile metadata.
2. SHA-256 from profile metadata.
3. SHA-1 from profile metadata.
4. SHA-512/SHA-256/SHA-1 sidecar from the same Maven origin, recorded with
   `checksum_source = "repository"`.

A missing or exactly empty metadata field means "not provided" and the next
source is tried. A non-empty malformed value fails the operation; it is never
treated as absent and never falls back to a weaker algorithm. An empty or
malformed repository sidecar response is also an error, not a missing hash.

Metadata hashes and repository sidecars have different meanings:

- A metadata checksum is what the Fabric profile publisher declared for the
  artifact.
- A repository checksum only proves that the bytes match what the same Maven
  origin serves. It is a repository-consistency check, not independent
  publisher proof, and it inherits the repository's trust.

The repository sidecar client follows redirects only within the exact
`maven.fabricmc.net` host. Artifact downloads use the global HTTPS allowlist;
the bytes are still verified against the selected `HashSpec` before publication.

## Download and repair behavior

- Files are downloaded to `.part`, verified, then atomically renamed. A failed
  checksum, size check, redirect or write leaves the previous working file
  untouched and removes the temporary files.
- Existing files are hashed, not accepted because they exist. A corrupt file is
  replaced only after a verified download.
- A legacy instance profile without a persisted checksum resolves the repository
  sidecar once and records it in `instance.json`; later launches verify locally
  without a checksum request.
- If no checksum can be obtained, installation/repair/launch fails closed with a
  clear error. A file is not downloaded or trusted on the strength of its name
  or presence.
- Launch-time self-healing runs before any game process is spawned. A checksum
  failure aborts the launch, and a successfully migrated legacy profile is
  saved before preparation continues.

## Compatibility

`LoaderLibrary` keeps `sha1`/`size` and adds optional `sha256`, `sha512` and
`checksum_source` fields with `serde(default)`. Profiles written by older builds
remain readable. Because those profiles may lack checksums for Fabric Loader and
Intermediary, the first online launch resolves and stores them; an offline
launch of a never-migrated profile is refused rather than silently trusting the
existing files.

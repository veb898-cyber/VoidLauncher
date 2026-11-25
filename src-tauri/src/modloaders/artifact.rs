//! Typed verification for Forge/NeoForge Maven artifacts.
//!
//! This module does not replace the shared download pipeline. It only turns
//! loader metadata and repository sidecars into a [`HashSpec`] and then uses
//! the existing verified download APIs.

use crate::error::{LauncherError, Result};
use crate::integrity::HashSpec;
use crate::modloaders::{LoaderLibrary, LoaderProfile};
use std::path::Path;
use std::time::Duration;

const MAX_CHECKSUM_BODY_BYTES: usize = 4096;

pub(crate) const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/";
pub(crate) const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/";
pub(crate) const NEOFORGE_MIRROR_MAVEN: &str = "https://maven.creeperhost.net/";
pub(crate) const MOJANG_LIBRARIES_MAVEN: &str = "https://libraries.minecraft.net/";

/// Groups that are part of Mojang's/game-side dependency universe in the
/// historical installer metadata. These coordinates must be resolved from
/// Mojang Libraries, not from Forge/NeoForge Maven. The list is intentionally
/// closed: an unknown group fails closed instead of being guessed onto an
/// arbitrary public Maven host.
fn is_mojang_library_group(group: &str) -> bool {
    matches!(
        group,
        "net.minecraft"
            | "com.mojang"
            | "org.ow2.asm"
            | "lzma"
            | "net.sf.jopt-simple"
            | "com.google.guava"
            | "org.apache.commons"
            | "java3d"
            | "net.sf.trove4j"
            | "jline"
            | "it.unimi.dsi"
            | "org.slf4j"
            | "commons-io"
            | "commons-codec"
            | "org.apache.httpcomponents"
            | "com.google.code.gson"
            | "com.google.code.findbugs"
            | "org.checkerframework"
            | "com.github.oshi"
            | "net.java.dev.jna"
            | "org.jetbrains"
            | "org.lwjgl"
            | "org.lwjgl.lwjgl"
            | "net.java.jinput"
            | "net.java.jutils"
            | "com.paulscode"
            | "paulscode"
            | "ibxm"
            | "argo"
            | "org.bouncycastle"
    ) || group.starts_with("com.typesafe")
        || group.starts_with("org.scala-lang")
        || group.starts_with("org.apache.logging.log4j")
        || group.starts_with("io.netty")
}

/// Groups whose artifacts are published by the loader's own Maven.
fn is_loader_library_group(group: &str) -> bool {
    matches!(
        group,
        "net.minecraftforge"
            | "net.neoforged"
            | "cpw.mods"
            | "org.spongepowered"
            | "com.electronwill.nightconfig"
    )
}

/// Resolve a library coordinate that has neither a per-library URL nor a
/// `downloads.artifact.url` to one of the trusted Maven origins.
///
/// Historical Forge metadata (1.7.10/1.8.9/1.10.2/1.11.2) leaves `url` absent
/// for Mojang libraries such as `net.minecraft:launchwrapper:1.12`. Treating
/// those as Forge Maven artifacts makes their checksum sidecars unavailable.
/// This mapping keeps the origin and checksum policy explicit instead of
/// weakening verification.
pub(crate) fn default_maven_origin(coordinate: &str, loader_maven: &str) -> Result<String> {
    let group = coordinate
        .split(':')
        .next()
        .filter(|group| !group.is_empty())
        .ok_or_else(|| {
            LauncherError::ModLoader(format!(
                "Historical library has no Maven group: {coordinate:?}"
            ))
        })?;
    if is_loader_library_group(group) {
        return Ok(loader_maven.to_string());
    }
    if is_mojang_library_group(group) {
        return Ok(MOJANG_LIBRARIES_MAVEN.to_string());
    }
    Err(LauncherError::ModLoader(format!(
        "No trusted Maven origin for historical library coordinate {coordinate:?}; \
         refusing to guess an arbitrary repository"
    )))
}

/// Resolve the base URL for a metadata library entry. An explicit non-empty
/// base wins. An exactly empty base means the historical "not provided" case
/// and uses [`default_maven_origin`]; whitespace-only is malformed and fails
/// closed rather than becoming a broken URL.
pub(crate) fn declared_maven_base(
    declared: Option<&str>,
    coordinate: &str,
    loader_maven: &str,
) -> Result<String> {
    match declared {
        Some("") => default_maven_origin(coordinate, loader_maven),
        Some(base) if base.trim().is_empty() => Err(LauncherError::ModLoader(format!(
            "Maven base URL is whitespace-only for historical library {coordinate:?}"
        ))),
        Some(base) => Ok(base.to_string()),
        None => default_maven_origin(coordinate, loader_maven),
    }
}

/// Join a Maven base/complete URL and a safe library path without appending
/// the same path twice. A complete artifact URL in historical metadata is
/// therefore accepted as a source; the destination remains `path`.
pub(crate) fn join_maven_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    if base.ends_with(path) {
        base.to_string()
    } else {
        format!("{base}/{path}")
    }
}

pub(crate) const FORGE_HOSTS: &[&str] = &["maven.minecraftforge.net", "libraries.minecraft.net"];
pub(crate) const NEOFORGE_HOSTS: &[&str] = &[
    "maven.neoforged.net",
    "maven.creeperhost.net",
    "libraries.minecraft.net",
];

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct EnsureOutcome {
    pub downloaded: bool,
    pub metadata_updated: bool,
    /// The artifact is embedded in the installer and was verified locally.
    pub embedded: bool,
}

/// Provenance of a verified installer download. The checksum/source are the
/// publisher-sidecar verification that was performed; the local file hash is
/// computed separately by the persistent cache.
#[derive(Debug, Clone)]
pub(crate) struct InstallerVerification {
    pub checksum: HashSpec,
    pub source: String,
    pub url: String,
}

pub(crate) fn has_metadata_checksum(
    sha1: Option<&str>,
    sha256: Option<&str>,
    sha512: Option<&str>,
) -> bool {
    [sha1, sha256, sha512]
        .into_iter()
        .any(|value| value.is_some_and(|v| !v.is_empty()))
}

fn validate_artifact_url(raw: &str, allowed_hosts: &[&str], label: &str) -> Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(raw.trim())
        .map_err(|e| LauncherError::ModLoader(format!("Invalid artifact URL for {label}: {e}")))?;

    // Test-only loopback origin for deterministic mock HTTP tests.
    #[cfg(test)]
    if parsed.scheme() == "http"
        && parsed.host_str() == Some("127.0.0.1")
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
    {
        return Ok(parsed);
    }

    if parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(LauncherError::ModLoader(format!(
            "Artifact URL has an unsafe shape for {label}: {raw}"
        )));
    }
    // Exact host match for loader artifacts. The shared downloader accepts
    // allowlisted subdomains for historical compatibility; those are not a
    // valid Maven origin for this typed path.
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if !allowed_hosts
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed))
    {
        return Err(LauncherError::ModLoader(format!(
            "Artifact host '{}' is not allowed for {label}",
            host
        )));
    }
    let hosts: Vec<String> = allowed_hosts.iter().map(|h| (*h).to_string()).collect();
    crate::download::validate_download_url_for_hosts(parsed.as_str(), &hosts)?;
    Ok(parsed)
}

/// The final filename of a safe relative Maven path.
///
/// The path itself has already passed `check_safe_remote_path`; this helper
/// additionally rejects an empty/trailing component so a directory-like
/// `library.path` can never be used as a file destination.
fn artifact_file_name(path: &str) -> Option<&str> {
    if path.ends_with('/') {
        return None;
    }
    let name = path.rsplit('/').next()?;
    if name.is_empty() || crate::import::check_safe_file_name(name).is_err() {
        return None;
    }
    Some(name)
}

/// Resolve a persisted `LoaderLibrary` URL without ever concatenating a path
/// onto a URL that already ends with that path.
///
/// Supported persisted forms:
/// - full artifact URL ending in `library.path` (current Forge/NeoForge);
/// - a legitimate Maven URL whose *filename* is exactly `library.path`'s
///   filename, but whose Maven directory differs. Older Forge metadata
///   (1.13.2/1.14.4/1.15.2) publishes shadowed/service classifiers this way:
///   the metadata `path` names `0.16.0-shadowed/...shadowed.jar` while the
///   official URL lives under `0.16.0/...shadowed.jar`. The destination must
///   remain the metadata `path`; the URL is only the source;
/// - root Maven base URL, e.g. `https://maven.minecraftforge.net/` + `path`
///   (defensive compatibility with old/hand-edited profiles).
///
/// An ambiguous URL is rejected rather than guessed. A matching basename is
/// not sufficient by itself: the URL must also pass the exact-host HTTPS
/// policy below, and the caller still verifies the publisher checksum before
/// atomically publishing the file.
pub(crate) fn resolve_library_artifact_url(
    lib: &LoaderLibrary,
    allowed_hosts: &[&str],
    label: &str,
) -> Result<String> {
    if lib.url.is_empty() {
        return Ok(String::new());
    }
    if lib.url.trim().is_empty() {
        return Err(LauncherError::ModLoader(format!(
            "Artifact URL is whitespace-only for {label}: {:?}",
            lib.url
        )));
    }
    crate::import::check_safe_remote_path(&lib.path)
        .map_err(|e| LauncherError::ModLoader(format!("Unsafe library path for {label}: {e}")))?;

    let mut parsed = validate_artifact_url(&lib.url, allowed_hosts, label)?;
    let wanted = lib.path.trim_start_matches('/');
    let wanted_name = artifact_file_name(wanted).ok_or_else(|| {
        LauncherError::ModLoader(format!(
            "Unsafe library destination filename for {label}: {}",
            lib.path
        ))
    })?;
    let current = parsed.path().trim_start_matches('/');

    if !current.is_empty() && current.ends_with('/') {
        return Err(LauncherError::ModLoader(format!(
            "Ambiguous artifact URL for {label}: {} ends with a directory, not a file",
            lib.url
        )));
    }
    if !current.is_empty() && current.ends_with(wanted) {
        return Ok(parsed.to_string());
    }
    if !current.is_empty() {
        if let Some(current_name) = artifact_file_name(current) {
            if current_name == wanted_name {
                return Ok(parsed.to_string());
            }
        }
    }
    if current.is_empty() || current == "/" {
        parsed.set_path(&format!("/{wanted}"));
        return Ok(parsed.to_string());
    }
    Err(LauncherError::ModLoader(format!(
        "Ambiguous artifact URL for {label}: {} does not end with {} and is not a Maven base",
        lib.url, lib.path
    )))
}

pub(crate) async fn fetch_repository_checksum(
    artifact_url: &str,
    label: &str,
) -> Result<(HashSpec, &'static str)> {
    let parsed = reqwest::Url::parse(artifact_url)
        .map_err(|e| LauncherError::ModLoader(format!("Invalid artifact URL for {label}: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| LauncherError::ModLoader(format!("Artifact URL has no host: {label}")))?
        .to_string();
    if parsed.scheme() != "https" && !cfg!(test) {
        return Err(LauncherError::ModLoader(format!(
            "Repository checksum requires HTTPS: {label}"
        )));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() || !parsed.username().is_empty() {
        return Err(LauncherError::ModLoader(format!(
            "Repository checksum URL has an unsafe shape: {label}"
        )));
    }

    let client = crate::download::http_client_for_exact_host(&host)?;
    for (extension, algorithm) in [
        (".sha512", "sha512"),
        (".sha256", "sha256"),
        (".sha1", "sha1"),
    ] {
        let checksum_url = format!("{artifact_url}{extension}");
        let mut response = crate::download::send_with_fallback_same_host(
            client.get(&checksum_url).timeout(Duration::from_secs(20)),
            &host,
        )
        .await
        .map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to fetch repository checksum for {label}: {e}"
            ))
        })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            continue;
        }
        if !response.status().is_success() {
            return Err(LauncherError::ModLoader(format!(
                "Repository checksum for {label} returned {}",
                response.status()
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to read repository checksum for {label}: {e}"
            ))
        })? {
            if body.len().saturating_add(chunk.len()) > MAX_CHECKSUM_BODY_BYTES {
                return Err(LauncherError::ModLoader(format!(
                    "Repository checksum for {label} is too large"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let text = std::str::from_utf8(&body).map_err(|e| {
            LauncherError::ModLoader(format!("Repository checksum for {label} is not UTF-8: {e}"))
        })?;
        let hash = HashSpec::parse(algorithm, text.trim()).map_err(|e| {
            LauncherError::ModLoader(format!(
                "Invalid {} repository checksum for {label}: {e}",
                algorithm.to_ascii_uppercase()
            ))
        })?;
        return Ok((hash, "repository"));
    }
    Err(LauncherError::ModLoader(format!(
        "No repository checksum is available for {label}"
    )))
}

fn missing_repository_checksum(error: &LauncherError) -> bool {
    error
        .to_string()
        .contains("No repository checksum is available")
}

fn loader_maven_for_hosts(allowed_hosts: &[&str]) -> &'static str {
    if allowed_hosts.iter().any(|host| host.contains("neoforged")) {
        NEOFORGE_MAVEN
    } else {
        FORGE_MAVEN
    }
}

/// Ordered checksum-source candidates for a library whose publisher metadata
/// did not provide a checksum. The explicitly resolved URL is always first.
/// For a known Mojang coordinate, the Mojang Libraries origin is the only
/// additional candidate; this also migrates persisted profiles that still
/// contain the old Forge-Maven fallback URL. Unknown coordinates add nothing.
fn repository_checksum_candidates(
    lib: &LoaderLibrary,
    resolved_url: &str,
    allowed_hosts: &[&str],
) -> Vec<String> {
    let mut candidates = vec![resolved_url.to_string()];
    if let Ok(origin) = default_maven_origin(&lib.name, loader_maven_for_hosts(allowed_hosts)) {
        let alternate = format!(
            "{}{}",
            if origin.ends_with('/') {
                origin
            } else {
                format!("{}/", origin)
            },
            lib.path.trim_start_matches('/')
        );
        if alternate != resolved_url && !candidates.iter().any(|item| item == &alternate) {
            candidates.push(alternate);
        }
    }
    candidates
}

fn write_checksum(lib: &mut LoaderLibrary, hash: &HashSpec, source: &str) {
    let hex = hash.hex().to_string();
    match hash.algorithm() {
        "sha1" => lib.sha1 = Some(hex),
        "sha256" => lib.sha256 = Some(hex),
        "sha512" => lib.sha512 = Some(hex),
        _ => {}
    }
    lib.checksum_source = Some(source.to_string());
}

/// Verify/replace one persisted library using trusted metadata or a
/// same-origin repository sidecar.
pub(crate) async fn ensure_library(
    lib: &mut LoaderLibrary,
    libraries_dir: &Path,
    allowed_hosts: &[&str],
    label: &str,
) -> Result<EnsureOutcome> {
    crate::import::check_safe_remote_path(&lib.path)
        .map_err(|e| LauncherError::ModLoader(format!("Unsafe library path for {label}: {e}")))?;
    let dest = libraries_dir.join(&lib.path);
    if !dest.starts_with(libraries_dir) {
        return Err(LauncherError::ModLoader(format!(
            "Library path escapes the libraries directory for {label}: {}",
            lib.path
        )));
    }
    crate::import::ensure_no_reparse_points(libraries_dir, &dest)?;

    // An empty URL means the artifact is embedded in the installer. It is
    // extracted by the loader's verified-installer path, not downloaded from
    // Maven. Never treat the empty URL itself as proof that the file is valid:
    // validate the existing file against the trusted metadata here, and let
    // the caller surface a missing/corrupt embedded artifact as an error.
    if lib.url.is_empty() {
        let metadata = std::fs::metadata(&dest).map_err(|e| {
            LauncherError::ModLoader(format!(
                "Embedded {label} library {} is missing: {} ({e})",
                lib.name,
                dest.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(LauncherError::ModLoader(format!(
                "Embedded {label} library is not a regular file: {}",
                dest.display()
            )));
        }
        if metadata.len() == 0 {
            return Err(LauncherError::ModLoader(format!(
                "Embedded {label} library is empty: {}",
                dest.display()
            )));
        }
        if let Some(size) = lib.size.filter(|size| *size > 0) {
            if metadata.len() != size {
                return Err(LauncherError::ModLoader(format!(
                    "Embedded {label} library size mismatch: {} (expected {size}, got {})",
                    dest.display(),
                    metadata.len()
                )));
            }
        }
        if let Some(hash) = lib.checksum()? {
            if !hash.verify_file(&dest)? {
                return Err(LauncherError::ModLoader(format!(
                    "Embedded {label} library failed its metadata checksum: {}",
                    dest.display()
                )));
            }
        }
        return Ok(EnsureOutcome {
            downloaded: false,
            metadata_updated: false,
            embedded: true,
        });
    }

    let resolved_url = resolve_library_artifact_url(lib, allowed_hosts, label)?;
    let mut download_url = resolved_url.clone();
    let mut metadata_updated = false;
    let (hash, source) = match lib.checksum()? {
        Some(hash) => {
            let source = match lib.checksum_source.as_deref() {
                Some(source) => source.to_string(),
                None => {
                    lib.checksum_source = Some("metadata".to_string());
                    metadata_updated = true;
                    "metadata".to_string()
                }
            };
            (hash, source)
        }
        None => {
            let candidates = repository_checksum_candidates(lib, &resolved_url, allowed_hosts);
            let mut last_error = None;
            let mut found = None;
            for candidate in candidates {
                match fetch_repository_checksum(&candidate, label).await {
                    Ok((hash, source)) => {
                        found = Some((hash, source, candidate));
                        break;
                    }
                    Err(error) if missing_repository_checksum(&error) => {
                        last_error = Some(error);
                    }
                    Err(error) => return Err(error),
                }
            }
            let (hash, source, selected_url) = match found {
                Some(value) => value,
                None => {
                    return Err(last_error.unwrap_or_else(|| {
                        LauncherError::ModLoader(format!(
                            "No repository checksum is available for {label}"
                        ))
                    }))
                }
            };
            download_url = selected_url;
            write_checksum(lib, &hash, source);
            metadata_updated = true;
            (hash, source.to_string())
        }
    };

    let existed = dest.is_file();
    if existed && hash.verify_file(&dest).unwrap_or(false) {
        return Ok(EnsureOutcome {
            downloaded: false,
            metadata_updated,
            embedded: false,
        });
    }
    if dest.exists() && !dest.is_file() {
        return Err(LauncherError::ModLoader(format!(
            "Library destination is not a regular file for {label}: {}",
            dest.display()
        )));
    }
    if existed {
        tracing::warn!(
            target: "launcher",
            "{label} library {} is corrupt; replacing it (checksum source: {})",
            lib.name,
            source
        );
    }

    if let Some(size) = lib.size.filter(|size| *size > 0) {
        crate::download::download_file_sized_verified(&download_url, &dest, &hash, size).await?;
    } else {
        crate::download::download_file_verified(&download_url, &dest, &hash).await?;
    }
    tracing::info!(
        target: "launcher",
        "{label} library {} verified ({}, source: {})",
        lib.name,
        hash.algorithm(),
        source
    );
    Ok(EnsureOutcome {
        downloaded: true,
        metadata_updated,
        embedded: false,
    })
}

pub(crate) async fn ensure_libraries(
    profile: &mut LoaderProfile,
    libraries_dir: &Path,
    allowed_hosts: &[&str],
    label: &str,
) -> Result<bool> {
    let mut metadata_updated = false;
    for lib in &mut profile.libraries {
        let outcome = ensure_library(lib, libraries_dir, allowed_hosts, label).await?;
        if outcome.embedded {
            tracing::debug!(
                target: "launcher",
                "Verified embedded {label} library {}",
                lib.name
            );
        }
        metadata_updated |= outcome.metadata_updated;
    }
    Ok(metadata_updated)
}

/// Download an installer from the first candidate whose same-origin sidecar is
/// available. Mirrors do not bypass verification: a cached file is accepted
/// only when it matches the selected checksum.
pub(crate) async fn download_verified_installer(
    candidates: &[String],
    dest: &Path,
    allowed_hosts: &[&str],
    label: &str,
) -> Result<InstallerVerification> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut last_err = None;
    for candidate in candidates {
        let parsed = match validate_artifact_url(candidate, allowed_hosts, label) {
            Ok(url) => url,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        let (hash, source) = match fetch_repository_checksum(parsed.as_str(), label).await {
            Ok(value) => value,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        match crate::download::download_file_verified(parsed.as_str(), dest, &hash).await {
            Ok(()) => {
                return Ok(InstallerVerification {
                    checksum: hash,
                    source: source.to_string(),
                    url: parsed.to_string(),
                });
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        LauncherError::ModLoader(format!(
            "No verified download source is available for {label}"
        ))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::tests::{
        assert_clean, direct_proxy_lock, mock_dir, sha1_hex, sha256_hex, sha512_hex, spawn_mock,
        MockServer, MockSpec,
    };
    use std::sync::atomic::Ordering;

    fn base(server: &MockServer) -> String {
        server.url.trim_end_matches("artifact.bin").to_string()
    }

    fn part_paths(path: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let mut part = path.as_os_str().to_os_string();
        part.push(".part");
        let mut meta = path.as_os_str().to_os_string();
        meta.push(".part.meta");
        (
            std::path::PathBuf::from(part),
            std::path::PathBuf::from(meta),
        )
    }

    fn library(url: &str, path: &str) -> LoaderLibrary {
        LoaderLibrary {
            name: "net.minecraftforge:example:1.0".to_string(),
            url: url.to_string(),
            path: path.to_string(),
            sha1: None,
            sha256: None,
            sha512: None,
            checksum_source: None,
            size: None,
        }
    }

    #[test]
    fn full_artifact_url_is_not_concatenated_twice() {
        let lib = library(
            "https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar",
            "net/minecraftforge/example/1.0/example-1.0.jar",
        );
        assert_eq!(
            resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").unwrap(),
            "https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar"
        );
    }

    #[test]
    fn maven_base_join_does_not_duplicate_a_complete_url_or_path() {
        let path = "net/minecraftforge/example/1.0/example-1.0.jar";
        assert_eq!(
            join_maven_url(
                "https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar",
                path
            ),
            "https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar"
        );
        assert_eq!(
            join_maven_url("https://maven.minecraftforge.net/", path),
            "https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar"
        );
    }

    #[test]
    fn historical_coordinate_origin_policy_is_closed_and_deterministic() {
        assert_eq!(
            default_maven_origin("net.minecraft:launchwrapper:1.12", FORGE_MAVEN).unwrap(),
            MOJANG_LIBRARIES_MAVEN
        );
        for coordinate in [
            "org.ow2.asm:asm-all:5.0.3",
            "lzma:lzma:0.0.1",
            "net.sf.jopt-simple:jopt-simple:4.6",
            "com.google.guava:guava:17.0",
            "org.apache.commons:commons-lang3:3.3.2",
            "java3d:vecmath:1.5.2",
            "net.sf.trove4j:trove4j:3.0.3",
            "org.lwjgl.lwjgl:lwjgl:2.9.0",
            "com.paulscode:soundsystem:20120107",
            "argo:argo:2.25_fixed",
            "org.bouncycastle:bcprov-jdk15on:1.47",
        ] {
            assert_eq!(
                default_maven_origin(coordinate, FORGE_MAVEN).unwrap(),
                MOJANG_LIBRARIES_MAVEN,
                "coordinate {coordinate}"
            );
        }
        assert_eq!(
            default_maven_origin("net.minecraftforge:forge:1.12.2-14.23.5.2860", FORGE_MAVEN)
                .unwrap(),
            FORGE_MAVEN
        );
        assert_eq!(
            default_maven_origin("net.neoforged:neoforge:21.1.172", NEOFORGE_MAVEN).unwrap(),
            NEOFORGE_MAVEN
        );
        assert!(default_maven_origin("com.example:unknown:1.0", FORGE_MAVEN).is_err());

        assert_eq!(
            declared_maven_base(None, "net.minecraft:launchwrapper:1.12", FORGE_MAVEN).unwrap(),
            MOJANG_LIBRARIES_MAVEN
        );
        assert_eq!(
            declared_maven_base(Some(""), "net.minecraft:launchwrapper:1.12", FORGE_MAVEN).unwrap(),
            MOJANG_LIBRARIES_MAVEN
        );
        assert!(
            declared_maven_base(Some("   "), "net.minecraft:launchwrapper:1.12", FORGE_MAVEN)
                .is_err()
        );
        assert_eq!(
            declared_maven_base(
                Some("https://libraries.minecraft.net/"),
                "net.minecraft:launchwrapper:1.12",
                FORGE_MAVEN
            )
            .unwrap(),
            "https://libraries.minecraft.net/"
        );
    }

    #[test]
    fn historical_forge_profile_checksum_candidates_include_mojang_origin() {
        let path = "net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar";
        let mut lib = library(
            "https://maven.minecraftforge.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar",
            path,
        );
        lib.name = "net.minecraft:launchwrapper:1.12".to_string();
        let candidates = repository_checksum_candidates(
            &lib,
            "https://maven.minecraftforge.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar",
            FORGE_HOSTS,
        );
        assert_eq!(candidates[0], lib.url);
        assert_eq!(
            candidates[1],
            "https://libraries.minecraft.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar"
        );

        let mut unknown = library(
            "https://maven.minecraftforge.net/com/example/unknown/1.0/unknown-1.0.jar",
            "com/example/unknown/1.0/unknown-1.0.jar",
        );
        unknown.name = "com.example:unknown:1.0".to_string();
        assert_eq!(
            repository_checksum_candidates(
                &unknown,
                "https://maven.minecraftforge.net/com/example/unknown/1.0/unknown-1.0.jar",
                FORGE_HOSTS
            )
            .len(),
            1,
            "unknown coordinates must not gain a guessed origin"
        );
    }

    #[test]
    fn legacy_forge_classifier_directory_mismatch_resolves_by_filename() {
        // Real Forge 1.13.2/1.14.4/1.15.2 metadata: the official URL puts the
        // `-shadowed`/`-service` classifier under the base version directory,
        // while the metadata `path` names the classifier directory. The
        // destination must stay on `lib.path`.
        let cases = [
            (
                "https://maven.minecraftforge.net/net/minecraftforge/accesstransformers/0.16.0/accesstransformers-0.16.0-shadowed.jar",
                "net/minecraftforge/accesstransformers/0.16.0-shadowed/accesstransformers-0.16.0-shadowed.jar",
            ),
            (
                "https://maven.minecraftforge.net/net/minecraftforge/eventbus/0.9.3/eventbus-0.9.3-service.jar",
                "net/minecraftforge/eventbus/0.9.3-service/eventbus-0.9.3-service.jar",
            ),
        ];
        for (url, path) in cases {
            let lib = library(url, path);
            assert_eq!(
                resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").unwrap(),
                url,
                "the official source URL must be preserved for {path}"
            );
        }
    }

    #[test]
    fn basename_match_does_not_weaken_host_scheme_or_filename_policy() {
        let metadata_path = "net/minecraftforge/accesstransformers/0.16.0-shadowed/accesstransformers-0.16.0-shadowed.jar";
        let mut lib = library(
            "https://maven.minecraftforge.net/net/minecraftforge/accesstransformers/0.16.0/other.jar",
            metadata_path,
        );
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());

        lib.url = "https://evil.example/accesstransformers-0.16.0-shadowed.jar".to_string();
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());

        lib.url =
            "http://maven.minecraftforge.net/accesstransformers-0.16.0-shadowed.jar".to_string();
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());

        lib.url =
            "https://user:pass@maven.minecraftforge.net/accesstransformers-0.16.0-shadowed.jar"
                .to_string();
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());

        lib.url = "https://maven.minecraftforge.net/net/minecraftforge/accesstransformers/0.16.0/accesstransformers-0.16.0-shadowed.jar/".to_string();
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());

        lib.url =
            "https://maven.minecraftforge.net/accesstransformers-0.16.0-shadowed.jar".to_string();
        lib.path = "net/minecraftforge/accesstransformers/0.16.0-shadowed/".to_string();
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());
    }

    #[test]
    fn root_maven_base_appends_the_safe_path() {
        let lib = library(
            "https://maven.minecraftforge.net/",
            "net/minecraftforge/example/1.0/example-1.0.jar",
        );
        assert_eq!(
            resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").unwrap(),
            "https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar"
        );
    }

    #[test]
    fn subdomain_and_query_artifact_urls_are_rejected() {
        let mut lib = library(
            "https://evil.maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar",
            "net/minecraftforge/example/1.0/example-1.0.jar",
        );
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());

        lib.url =
            "https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar?x=1"
                .to_string();
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());
    }

    #[test]
    fn ambiguous_and_untrusted_artifact_urls_are_rejected() {
        let mut lib = library(
            "https://maven.minecraftforge.net/wrong/path.jar",
            "net/minecraftforge/example/1.0/example-1.0.jar",
        );
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());

        lib.url = "https://evil.example/example-1.0.jar".to_string();
        lib.path = "example-1.0.jar".to_string();
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());

        lib.url = "https://maven.minecraftforge.net/".to_string();
        lib.path = "../escape.jar".to_string();
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());
    }

    #[tokio::test]
    async fn ensure_library_accepts_metadata_sha512_sha256_and_sha1() {
        let _proxy = direct_proxy_lock().await;
        let body = b"metadata-artifact";
        for algorithm in ["sha512", "sha256", "sha1"] {
            let digest = match algorithm {
                "sha512" => sha512_hex(body),
                "sha256" => sha256_hex(body),
                _ => sha1_hex(body),
            };
            let server = spawn_mock(vec![MockSpec::ok(body)]).await;
            let dir = mock_dir(&format!("artifact_metadata_{algorithm}"));
            let path = format!("net/minecraftforge/example/1.0/example-1.0-{algorithm}.jar");
            let mut lib = library(&format!("{}{}", base(&server), path), &path);
            match algorithm {
                "sha512" => lib.sha512 = Some(digest.clone()),
                "sha256" => lib.sha256 = Some(digest.clone()),
                _ => lib.sha1 = Some(digest.clone()),
            }
            lib.checksum_source = Some("metadata".to_string());
            lib.size = Some(body.len() as u64);
            let outcome = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
                .await
                .unwrap();
            assert!(outcome.downloaded, "{algorithm}");
            assert_eq!(std::fs::read(dir.join(&path)).unwrap(), body);
            assert_eq!(server.hits.load(Ordering::SeqCst), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn mismatched_maven_directory_publishes_to_metadata_path_and_verifies() {
        let _proxy = direct_proxy_lock().await;
        let body = b"shadowed-classifier";
        let digest = sha1_hex(body);
        let server = spawn_mock(vec![MockSpec::ok(body)]).await;
        let dir = mock_dir("artifact_mismatched_directory");
        let path = "net/minecraftforge/accesstransformers/0.16.0-shadowed/accesstransformers-0.16.0-shadowed.jar";
        let url = format!(
            "{}net/minecraftforge/accesstransformers/0.16.0/accesstransformers-0.16.0-shadowed.jar",
            base(&server)
        );
        let mut lib = library(&url, path);
        lib.sha1 = Some(digest);
        lib.checksum_source = Some("metadata".to_string());
        lib.size = Some(body.len() as u64);

        let outcome = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
            .await
            .unwrap();
        assert!(outcome.downloaded);
        assert_eq!(std::fs::read(dir.join(path)).unwrap(), body);
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        assert_clean(
            &part_paths(&dir.join(path)).0,
            &part_paths(&dir.join(path)).1,
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn malformed_metadata_checksum_fails_without_network() {
        let dir = mock_dir("artifact_malformed_metadata");
        let server = spawn_mock(vec![MockSpec::status(500)]).await;
        let path = "net/minecraftforge/example/1.0/example-1.0.jar";
        let mut lib = library(&format!("{}{}", base(&server), path), path);
        lib.sha512 = Some("not-a-hash".to_string());
        lib.checksum_source = Some("metadata".to_string());

        let err = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
            .await
            .expect_err("malformed metadata checksum must fail");
        assert!(
            err.to_string().to_lowercase().contains("sha512"),
            "got: {err}"
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_library_sidecar_fails_closed_without_structural_fallback() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::status(404),
        ])
        .await;
        let dir = mock_dir("artifact_missing_sidecar");
        let path = "net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar";
        let mut lib = library(&format!("{}{}", base(&server), path), path);

        let error = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
            .await
            .expect_err("no sidecar on any permitted extension must fail closed");
        assert!(
            error.to_string().contains("No repository checksum"),
            "got: {error}"
        );
        assert!(!dir.join(path).exists());
        assert_eq!(server.hits.load(Ordering::SeqCst), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sidecar_redirect_to_an_untrusted_host_is_rejected() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::redirect(
            "https://evil.example/artifact.sha512",
        )])
        .await;
        let dir = mock_dir("artifact_redirect");
        let dest = dir.join("installer.jar");
        let candidate = format!("{}installer.jar", base(&server));

        let err = download_verified_installer(&[candidate], &dest, FORGE_HOSTS, "installer")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("302"), "got: {err}");
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_library_verifies_repository_sidecar_and_publishes() {
        let _proxy = direct_proxy_lock().await;
        let body = b"forge-artifact";
        let hash = sha512_hex(body);
        let server = spawn_mock(vec![MockSpec::ok(hash.as_bytes()), MockSpec::ok(body)]).await;
        let dir = mock_dir("artifact_repository");
        let path = "net/minecraftforge/example/1.0/example-1.0.jar";
        let url = format!("{}{}", base(&server), path);
        let mut lib = library(&url, path);

        let outcome = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
            .await
            .unwrap();
        assert!(outcome.downloaded);
        assert!(outcome.metadata_updated);
        assert_eq!(lib.sha512.as_deref(), Some(hash.as_str()));
        assert_eq!(lib.checksum_source.as_deref(), Some("repository"));
        assert_eq!(std::fs::read(dir.join(path)).unwrap(), body);
        assert_clean(
            &part_paths(&dir.join(path)).0,
            &part_paths(&dir.join(path)).1,
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn whitespace_only_artifact_url_is_malformed_not_embedded() {
        let lib = library("   ", "net/minecraftforge/example/1.0/example-1.0.jar");
        assert!(resolve_library_artifact_url(&lib, FORGE_HOSTS, "Forge").is_err());
    }

    #[tokio::test]
    async fn whitespace_only_artifact_url_is_not_treated_as_embedded() {
        let dir = mock_dir("artifact_whitespace_url");
        let mut lib = library("   ", "net/minecraftforge/example/1.0/example-1.0.jar");
        let error = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
            .await
            .expect_err("whitespace URL must not select the embedded path");
        assert!(error.to_string().contains("whitespace"), "got: {error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn empty_url_embedded_library_is_never_silently_accepted() {
        let dir = mock_dir("artifact_embedded_empty");
        let path = "net/minecraftforge/forge/1.16.5-test/forge-1.16.5-test-universal.jar";
        let mut lib = library("", path);
        lib.sha1 = Some(sha1_hex(b"embedded-bytes"));
        lib.checksum_source = Some("metadata".to_string());
        lib.size = Some(b"embedded-bytes".len() as u64);

        let missing = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
            .await
            .expect_err("a missing embedded library must be an error");
        assert!(
            missing.to_string().contains("missing"),
            "unexpected error: {missing}"
        );

        let dest = dir.join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"wrong-bytes!").unwrap();
        let corrupt = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
            .await
            .expect_err("a corrupt embedded library must be an error");
        assert!(
            corrupt.to_string().contains("checksum") || corrupt.to_string().contains("size"),
            "unexpected error: {corrupt}"
        );

        std::fs::write(&dest, b"embedded-bytes").unwrap();
        let outcome = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
            .await
            .expect("a valid embedded library must verify");
        assert!(outcome.embedded);
        assert!(!outcome.downloaded);
        assert_eq!(std::fs::read(&dest).unwrap(), b"embedded-bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_library_mismatch_preserves_existing_file() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![
            MockSpec::ok("0".repeat(128).as_bytes()),
            MockSpec::ok(b"hostile"),
            MockSpec::ok(b"hostile"),
            MockSpec::ok(b"hostile"),
            MockSpec::ok(b"hostile"),
            MockSpec::ok(b"hostile"),
        ])
        .await;
        let dir = mock_dir("artifact_mismatch");
        let path = "net/minecraftforge/example/1.0/example-1.0.jar";
        let dest = dir.join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"working").unwrap();
        let mut lib = library(&format!("{}{}", base(&server), path), path);

        let err = ensure_library(&mut lib, &dir, FORGE_HOSTS, "Forge")
            .await
            .expect_err("wrong sidecar checksum must fail");
        assert!(err.to_string().contains("SHA512 mismatch"), "got: {err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"working");
        assert_clean(&part_paths(&dest).0, &part_paths(&dest).1);
        assert_eq!(server.hits.load(Ordering::SeqCst), 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn installer_requires_a_verified_sidecar_and_publishes_atomically() {
        let _proxy = direct_proxy_lock().await;
        let body = b"installer-bytes";
        let hash = sha512_hex(body);
        let server = spawn_mock(vec![MockSpec::ok(hash.as_bytes()), MockSpec::ok(body)]).await;
        let dir = mock_dir("artifact_installer");
        let dest = dir.join("installer.jar");
        let candidate = format!("{}installer.jar", base(&server));

        let verification =
            download_verified_installer(&[candidate], &dest, FORGE_HOSTS, "installer")
                .await
                .unwrap();
        assert_eq!(verification.checksum.algorithm(), "sha512");
        assert_eq!(verification.source, "repository");
        assert!(verification.url.ends_with("installer.jar"));
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_clean(&part_paths(&dest).0, &part_paths(&dest).1);
    }

    #[tokio::test]
    async fn installer_redownload_preserves_old_file_on_failure_and_replaces_on_success() {
        let _proxy = direct_proxy_lock().await;
        let dir = mock_dir("artifact_installer_replace");
        let dest = dir.join("installer.jar");
        std::fs::write(&dest, b"previous-good-installer").unwrap();
        let failing = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::status(404),
        ])
        .await;
        let candidate = format!("{}installer.jar", base(&failing));
        let error = download_verified_installer(&[candidate], &dest, FORGE_HOSTS, "installer")
            .await
            .expect_err("a metadata-only failure must fail closed");
        assert!(error.to_string().contains("No repository checksum"));
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"previous-good-installer",
            "a failed re-download must not destroy the old installer"
        );

        let body = b"new-verified-installer";
        let hash = sha512_hex(body);
        let succeeding = spawn_mock(vec![MockSpec::ok(hash.as_bytes()), MockSpec::ok(body)]).await;
        let candidate = format!("{}installer.jar", base(&succeeding));
        download_verified_installer(&[candidate], &dest, FORGE_HOSTS, "installer")
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_clean(&part_paths(&dest).0, &part_paths(&dest).1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn installer_without_a_sidecar_fails_closed() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::status(404),
        ])
        .await;
        let dir = mock_dir("artifact_installer_missing");
        let dest = dir.join("installer.jar");
        let candidate = format!("{}installer.jar", base(&server));

        let err = download_verified_installer(&[candidate], &dest, FORGE_HOSTS, "installer")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("No repository checksum"),
            "got: {err}"
        );
        assert!(!dest.exists());
        assert_clean(&part_paths(&dest).0, &part_paths(&dest).1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

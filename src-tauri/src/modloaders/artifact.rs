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

/// Resolve a persisted `LoaderLibrary` URL without ever concatenating a path
/// onto a URL that already ends with that path.
///
/// Supported persisted forms:
/// - full artifact URL ending in `library.path` (current Forge/NeoForge);
/// - root Maven base URL, e.g. `https://maven.minecraftforge.net/` + `path`
///   (defensive compatibility with old/hand-edited profiles).
///
/// An ambiguous URL is rejected rather than guessed.
pub(crate) fn resolve_library_artifact_url(
    lib: &LoaderLibrary,
    allowed_hosts: &[&str],
    label: &str,
) -> Result<String> {
    if lib.url.trim().is_empty() {
        return Ok(String::new());
    }
    crate::import::check_safe_remote_path(&lib.path)
        .map_err(|e| LauncherError::ModLoader(format!("Unsafe library path for {label}: {e}")))?;

    let mut parsed = validate_artifact_url(&lib.url, allowed_hosts, label)?;
    let wanted = lib.path.trim_start_matches('/');
    let current = parsed.path().trim_start_matches('/');

    if !current.is_empty() && current.ends_with(wanted) {
        return Ok(parsed.to_string());
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

async fn fetch_repository_checksum(
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

    // An empty URL means the artifact is embedded in the installer and is
    // extracted by the processor pipeline, not downloaded from Maven.
    if lib.url.trim().is_empty() {
        return Ok(EnsureOutcome::default());
    }

    let url = resolve_library_artifact_url(lib, allowed_hosts, label)?;
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
            let (hash, source) = fetch_repository_checksum(&url, label).await?;
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
        crate::download::download_file_sized_verified(&url, &dest, &hash, size).await?;
    } else {
        crate::download::download_file_verified(&url, &dest, &hash).await?;
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
        metadata_updated |= ensure_library(lib, libraries_dir, allowed_hosts, label)
            .await?
            .metadata_updated;
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
) -> Result<()> {
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
        let (hash, _source) = match fetch_repository_checksum(parsed.as_str(), label).await {
            Ok(value) => value,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        match crate::download::download_file_verified(parsed.as_str(), dest, &hash).await {
            Ok(()) => return Ok(()),
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

        download_verified_installer(&[candidate], &dest, FORGE_HOSTS, "installer")
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_clean(&part_paths(&dest).0, &part_paths(&dest).1);
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

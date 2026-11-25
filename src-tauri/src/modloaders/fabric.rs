use crate::error::{LauncherError, Result};
use crate::integrity::HashSpec;
use crate::modloaders::{LoaderLibrary, LoaderProfile, LoaderVersionPage};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The only Fabric Maven origin this launcher downloads artifacts from.
const FABRIC_MAVEN_BASE: &str = "https://maven.fabricmc.net/";
const MAX_CHECKSUM_BODY_BYTES: usize = 4096;

/// Fabric Meta API response for loader versions
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct FabricLoaderVersion {
    pub separator: String,
    pub build: u32,
    pub maven: String,
    pub version: String,
    /// Defensive default: if Fabric's API ever drops the `stable` field
    /// we still want the whole array to deserialize.
    #[serde(default)]
    pub stable: bool,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FabricGameVersion {
    version: String,
}

/// Fabric profile from meta API
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FabricProfile {
    id: String,
    #[serde(rename = "mainClass")]
    main_class: String,
    libraries: Vec<FabricLibrary>,
    arguments: Option<FabricArguments>,
}

#[derive(Debug, Deserialize)]
struct FabricLibrary {
    name: String,
    url: String,
    /// Fabric's profile JSON currently publishes SHA-1 for most libraries.
    /// Stronger fields are tolerated if Fabric adds them later.
    #[serde(default, alias = "sha-1")]
    sha1: Option<String>,
    #[serde(default, alias = "sha-256")]
    sha256: Option<String>,
    #[serde(default, alias = "sha-512")]
    sha512: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct FabricArguments {
    #[serde(default)]
    game: Vec<String>,
    #[serde(default)]
    jvm: Vec<String>,
}

/// Fetch a page of available Fabric loader versions.
///
/// Uses Fabric's official API (`meta.fabricmc.net`) directly rather
/// than the Prism mirror, because the mirror can be outdated and miss
/// newer Fabric Loader releases. The official API always returns the
/// complete, up-to-date list.
///
/// Results are cached in-process after the first fetch so subsequent
/// pages are instant.
static FABRIC_VERSIONS_CACHE: OnceLock<Mutex<Option<Vec<FabricLoaderVersion>>>> = OnceLock::new();

/// Supported Minecraft versions, cached for 10 minutes.
type SupportedVersionsSlot = OnceLock<Mutex<Option<(Instant, Vec<String>)>>>;
static SUPPORTED_VERSIONS_CACHE: SupportedVersionsSlot = OnceLock::new();
const SUPPORTED_CACHE_TTL: Duration = Duration::from_secs(600);

/// Minecraft versions officially supported by Fabric (`/v2/versions/game`).
/// Fabric supports 1.14+ only — versions like 1.13 are rejected by the API.
pub async fn supported_versions() -> Result<Vec<String>> {
    {
        let cache = SUPPORTED_VERSIONS_CACHE.get_or_init(|| Mutex::new(None));
        let guard = cache
            .lock()
            .map_err(|e| LauncherError::ModLoader(e.to_string()))?;
        if let Some((fetched_at, versions)) = guard.as_ref() {
            if fetched_at.elapsed() < SUPPORTED_CACHE_TTL {
                return Ok(versions.clone());
            }
        }
    }
    let client = crate::download::global_http_client()?;
    let versions: Vec<FabricGameVersion> = crate::download::send_with_fallback(
        client.get("https://meta.fabricmc.net/v2/versions/game"),
    )
    .await
    .map_err(|e| LauncherError::ModLoader(format!("Failed to fetch Fabric game versions: {}", e)))?
    .json()
    .await
    .map_err(|e| {
        LauncherError::ModLoader(format!("Failed to parse Fabric game versions: {}", e))
    })?;
    let list: Vec<String> = versions.into_iter().map(|v| v.version).collect();
    let cache = SUPPORTED_VERSIONS_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache
        .lock()
        .map_err(|e| LauncherError::ModLoader(e.to_string()))?;
    *guard = Some((Instant::now(), list.clone()));
    Ok(list)
}

/// Whether the given Minecraft version is officially supported by Fabric.
pub async fn is_supported(mc_version: &str) -> Result<bool> {
    Ok(supported_versions().await?.iter().any(|v| v == mc_version))
}

/// Fetch a page of Fabric loader versions for a specific Minecraft version.
/// Returns an empty page when Fabric does not support the version.
pub async fn get_loader_versions_for(
    mc_version: &str,
    offset: usize,
    limit: usize,
) -> Result<LoaderVersionPage> {
    if !is_supported(mc_version).await? {
        return Ok(LoaderVersionPage {
            versions: Vec::new(),
            total: 0,
        });
    }
    get_loader_versions(offset, limit).await
}

pub async fn get_loader_versions(offset: usize, limit: usize) -> Result<LoaderVersionPage> {
    use std::cmp::Ordering;

    fn compare_versions(a: &str, b: &str) -> Ordering {
        let a_parts: Vec<u32> = a.split('.').filter_map(|s| s.parse().ok()).collect();
        let b_parts: Vec<u32> = b.split('.').filter_map(|s| s.parse().ok()).collect();
        let len = a_parts.len().max(b_parts.len());
        for i in 0..len {
            let a_val = a_parts.get(i).copied().unwrap_or(0);
            let b_val = b_parts.get(i).copied().unwrap_or(0);
            if a_val != b_val {
                return a_val.cmp(&b_val);
            }
        }
        a.cmp(b)
    }

    let cache = FABRIC_VERSIONS_CACHE.get_or_init(|| Mutex::new(None));

    // Fetch if not cached
    {
        let guard = cache
            .lock()
            .map_err(|e| LauncherError::ModLoader(e.to_string()))?;
        if guard.is_some() {
            let all = guard.as_ref().unwrap();
            let total = all.len();
            let page: Vec<_> = all
                .iter()
                .skip(offset)
                .take(limit)
                .map(|v| super::LoaderVersion {
                    version: v.version.clone(),
                    stable: v.stable,
                })
                .collect();
            return Ok(LoaderVersionPage {
                versions: page,
                total,
            });
        }
    }

    // Fetch from Fabric official API
    let client = crate::download::global_http_client()?;
    let versions: Vec<FabricLoaderVersion> = crate::download::send_with_fallback(
        client.get("https://meta.fabricmc.net/v2/versions/loader"),
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "launcher", "Failed to fetch Fabric loader versions: {}", e);
        LauncherError::ModLoader(e.to_string())
    })?
    .json()
    .await
    .map_err(|e| {
        tracing::error!(target: "launcher", "Failed to parse Fabric loader versions: {}", e);
        LauncherError::ModLoader(e.to_string())
    })?;

    // Sort newest-first by semver
    let mut sorted = versions;
    sorted.sort_by(|a, b| compare_versions(&b.version, &a.version));

    let total = sorted.len();
    let page: Vec<_> = sorted
        .iter()
        .skip(offset)
        .take(limit)
        .map(|v| super::LoaderVersion {
            version: v.version.clone(),
            stable: v.stable,
        })
        .collect();

    // Cache the full sorted list
    {
        let mut guard = cache
            .lock()
            .map_err(|e| LauncherError::ModLoader(e.to_string()))?;
        *guard = Some(sorted);
    }

    Ok(LoaderVersionPage {
        versions: page,
        total,
    })
}

/// Fetch game versions supported by Fabric
#[allow(dead_code)]
pub async fn get_game_versions() -> Result<Vec<String>> {
    let client = crate::download::global_http_client()?;
    let versions: Vec<FabricGameVersion> = crate::download::send_with_fallback(
        client.get("https://meta.fabricmc.net/v2/versions/game"),
    )
    .await?
    .json()
    .await?;

    Ok(versions.into_iter().map(|v| v.version).collect())
}

/// Validate the only Fabric Maven base this launcher will use.
fn fabric_maven_base(raw: &str) -> Result<String> {
    let raw = raw.trim();
    // Test-only: a controlled loopback origin lets the checksum/download
    // pipeline be exercised end to end. Release builds always require the
    // official HTTPS origin below.
    #[cfg(test)]
    if let Ok(parsed) = reqwest::Url::parse(raw) {
        if parsed.scheme() == "http"
            && parsed.host_str() == Some("127.0.0.1")
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && matches!(parsed.path(), "" | "/")
        {
            return Ok(format!("http://127.0.0.1:{}/", parsed.port().unwrap_or(80)));
        }
    }

    let parsed = reqwest::Url::parse(raw).map_err(|e| {
        LauncherError::ModLoader(format!("Invalid Fabric Maven URL '{}': {}", raw, e))
    })?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("maven.fabricmc.net")
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err(LauncherError::ModLoader(format!(
            "Fabric library URL is not the official Maven repository: {}",
            raw
        )));
    }
    Ok(FABRIC_MAVEN_BASE.to_string())
}

/// Apply the global HTTPS/host allowlist to a Fabric artifact URL.
fn validate_fabric_artifact_url(_base: &str, url: &str) -> Result<()> {
    #[cfg(test)]
    if _base.starts_with("http://127.0.0.1:") {
        return Ok(());
    }
    crate::download::validate_download_url(url)?;
    Ok(())
}

fn validate_maven_component(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains("..")
        || value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '@' | '[' | ']' | ' '))
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
    {
        return Err(LauncherError::ModLoader(format!(
            "Invalid Maven {label} in Fabric profile: {value:?}"
        )));
    }
    Ok(())
}

/// Convert a Fabric Maven coordinate into a safe path under `libraries/`.
/// `group:artifact:version[:classifier][@jar]` is the only supported form.
fn fabric_maven_path(name: &str) -> Result<String> {
    let parts: Vec<&str> = name.split(':').collect();
    if !(3..=4).contains(&parts.len()) {
        return Err(LauncherError::ModLoader(format!(
            "Invalid Fabric Maven coordinate: {name:?}"
        )));
    }
    let group = parts[0];
    let artifact = parts[1];
    // The only supported extension is a trailing `@jar`, attached to the last
    // coordinate component (Gradle-style `...:version[:classifier]@jar`).
    let last = *parts.last().expect("coordinate has at least three parts");
    let last_without_extension = last.strip_suffix("@jar").unwrap_or(last);
    if last_without_extension.contains('@')
        || parts[..parts.len() - 1]
            .iter()
            .any(|component| component.contains('@'))
    {
        return Err(LauncherError::ModLoader(format!(
            "Unsupported Maven extension in Fabric coordinate: {name:?}"
        )));
    }
    let (version, classifier) = if parts.len() == 4 {
        (parts[2], Some(last_without_extension))
    } else {
        (last_without_extension, None)
    };
    validate_maven_component(group, "group")?;
    for component in group.split('.') {
        if component.is_empty() {
            return Err(LauncherError::ModLoader(format!(
                "Invalid Maven group in Fabric profile: {group:?}"
            )));
        }
    }
    validate_maven_component(artifact, "artifact")?;
    validate_maven_component(version, "version")?;
    if let Some(classifier) = classifier {
        validate_maven_component(classifier, "classifier")?;
    }

    let group_path = group.replace('.', "/");
    let file_name = match classifier {
        Some(classifier) => format!("{artifact}-{version}-{classifier}.jar"),
        None => format!("{artifact}-{version}.jar"),
    };
    let path = format!("{group_path}/{artifact}/{version}/{file_name}");
    crate::import::check_safe_remote_path(&path)
        .map_err(|e| LauncherError::ModLoader(format!("Unsafe Fabric library path: {e}")))?;
    Ok(path)
}

fn present_hash(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn metadata_checksum(lib: &FabricLibrary) -> Result<Option<HashSpec>> {
    let mut hashes = HashMap::new();
    for (key, value) in [
        ("sha1", present_hash(&lib.sha1)),
        ("sha256", present_hash(&lib.sha256)),
        ("sha512", present_hash(&lib.sha512)),
    ] {
        if let Some(value) = value {
            hashes.insert(key.to_string(), value);
        }
    }
    HashSpec::from_metadata(&hashes).map_err(|e| {
        LauncherError::ModLoader(format!("Invalid Fabric checksum for {}: {e}", lib.name))
    })
}

fn parse_repository_checksum(algorithm: &str, body: &str) -> Result<HashSpec> {
    HashSpec::parse(algorithm, body.trim()).map_err(|e| {
        LauncherError::ModLoader(format!(
            "Invalid {} repository checksum for Fabric artifact: {e}",
            algorithm.to_ascii_uppercase()
        ))
    })
}

/// Fetch the strongest checksum sidecar from the same Maven origin as the
/// artifact. This is a repository-consistency check, not independent proof
/// that the repository itself is trustworthy.
async fn fetch_repository_checksum(base: &str, path: &str) -> Result<(HashSpec, &'static str)> {
    let base = fabric_maven_base(base)?;
    crate::import::check_safe_remote_path(path)
        .map_err(|e| LauncherError::ModLoader(format!("Unsafe Fabric library path: {e}")))?;
    let artifact_url = format!("{base}{path}");
    validate_fabric_artifact_url(&base, &artifact_url)?;
    // A checksum is only a repository-consistency signal while it comes from
    // the exact Maven origin that serves the artifact. The client's redirect
    // policy never leaves `maven.fabricmc.net`, not even for another host on
    // the global allowlist.
    let client = crate::download::http_client_for_exact_host("maven.fabricmc.net")?;

    for (extension, algorithm) in [
        (".sha512", "sha512"),
        (".sha256", "sha256"),
        (".sha1", "sha1"),
    ] {
        let checksum_url = format!("{artifact_url}{extension}");
        let mut response = crate::download::send_with_fallback_same_host(
            client.get(&checksum_url).timeout(Duration::from_secs(20)),
            "maven.fabricmc.net",
        )
        .await
        .map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to fetch Fabric repository checksum {}: {}",
                checksum_url, e
            ))
        })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            continue;
        }
        if !response.status().is_success() {
            return Err(LauncherError::ModLoader(format!(
                "Fabric repository checksum {} returned {}",
                checksum_url,
                response.status()
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to read Fabric repository checksum {}: {}",
                checksum_url, e
            ))
        })? {
            if body.len().saturating_add(chunk.len()) > MAX_CHECKSUM_BODY_BYTES {
                return Err(LauncherError::ModLoader(format!(
                    "Fabric repository checksum {} is too large",
                    checksum_url
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let text = std::str::from_utf8(&body).map_err(|e| {
            LauncherError::ModLoader(format!(
                "Fabric repository checksum {} is not UTF-8: {}",
                checksum_url, e
            ))
        })?;
        return Ok((parse_repository_checksum(algorithm, text)?, "repository"));
    }

    Err(LauncherError::ModLoader(format!(
        "Fabric artifact {} has no repository checksum; refusing to download it",
        path
    )))
}

fn fabric_artifact_url(lib: &LoaderLibrary) -> Result<String> {
    let base = fabric_maven_base(&lib.url)?;
    crate::import::check_safe_remote_path(&lib.path)
        .map_err(|e| LauncherError::ModLoader(format!("Unsafe Fabric library path: {e}")))?;
    let url = format!("{base}{}", lib.path);
    validate_fabric_artifact_url(&base, &url)?;
    Ok(url)
}

/// Turn one profile entry into a persisted, checksum-carrying library.
///
/// Fabric metadata provides SHA-1 for most artifacts but not for the loader
/// and intermediary. Those are resolved from the same Maven origin and stored
/// with an explicit `repository` provenance marker: the checksum proves the
/// bytes match what that repository serves, not that the repository itself is
/// an independent publisher.
async fn resolve_profile_library(lib: FabricLibrary) -> Result<LoaderLibrary> {
    let path = fabric_maven_path(&lib.name)?;
    let base = fabric_maven_base(&lib.url)?;
    let sha1 = present_hash(&lib.sha1);
    let sha256 = present_hash(&lib.sha256);
    let sha512 = present_hash(&lib.sha512);
    let metadata = metadata_checksum(&lib)?;
    let (sha1, sha256, sha512, checksum_source) = if metadata.is_some() {
        (sha1, sha256, sha512, Some("metadata".to_string()))
    } else {
        let (hash, source) = fetch_repository_checksum(&base, &path).await?;
        let hex = hash.hex().to_string();
        let fields = match hash.algorithm() {
            "sha1" => (Some(hex), None, None),
            "sha256" => (None, Some(hex), None),
            "sha512" => (None, None, Some(hex)),
            _ => {
                return Err(LauncherError::ModLoader(format!(
                    "Unsupported repository checksum algorithm for {}",
                    lib.name
                )))
            }
        };
        (fields.0, fields.1, fields.2, Some(source.to_string()))
    };
    Ok(LoaderLibrary {
        name: lib.name,
        url: base,
        path,
        sha1,
        sha256,
        sha512,
        checksum_source,
        size: lib.size.filter(|size| *size > 0),
    })
}

/// Get Fabric profile for a specific MC version + loader version.
///
/// Every library is resolved to a typed checksum before the profile is saved:
/// metadata checksums are preferred, otherwise the same-origin Maven checksum
/// sidecar is recorded explicitly as a repository checksum.
pub async fn get_profile(mc_version: &str, loader_version: &str) -> Result<LoaderProfile> {
    super::validate_loader_request(mc_version, loader_version)?;
    if !is_supported(mc_version).await? {
        return Err(LauncherError::ModLoader(format!(
            "Fabric does not support Minecraft {} (supported: 1.14 and newer)",
            mc_version
        )));
    }
    let client = crate::download::global_http_client()?;
    let url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
        mc_version, loader_version
    );

    let profile: FabricProfile = crate::download::send_with_fallback(client.get(&url))
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to fetch Fabric profile: {}", e)))?
        .json()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse Fabric profile: {}", e)))?;

    let mut libraries = Vec::with_capacity(profile.libraries.len());
    for lib in profile.libraries {
        libraries.push(resolve_profile_library(lib).await?);
    }

    let (game_args, jvm_args) = match profile.arguments {
        Some(args) => (args.game, args.jvm),
        None => (Vec::new(), Vec::new()),
    };

    Ok(LoaderProfile {
        main_class: profile.main_class,
        libraries,
        jvm_args,
        game_args,
        legacy_args: false,
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct EnsureOutcome {
    pub downloaded: bool,
    /// The library was missing a persisted checksum and the one loaded from
    /// the repository was written into the profile. Callers that own the
    /// instance should save it so the network lookup happens only once.
    pub metadata_updated: bool,
}

/// Ensure one Fabric library exists and matches its typed checksum.
/// Returns whether a file was downloaded and whether the profile gained a
/// repository checksum.
async fn ensure_library_mut(
    lib: &mut LoaderLibrary,
    libraries_dir: &Path,
) -> Result<EnsureOutcome> {
    crate::import::check_safe_remote_path(&lib.path)
        .map_err(|e| LauncherError::ModLoader(format!("Unsafe Fabric library path: {e}")))?;
    let dest = libraries_dir.join(&lib.path);
    if !dest.starts_with(libraries_dir) {
        return Err(LauncherError::ModLoader(format!(
            "Fabric library path escapes the libraries directory: {}",
            lib.path
        )));
    }
    crate::import::ensure_no_reparse_points(libraries_dir, &dest)?;

    let mut metadata_updated = false;
    let (hash, source) = match lib.checksum()? {
        Some(hash) => (
            hash,
            lib.checksum_source
                .as_deref()
                .unwrap_or("metadata")
                .to_string(),
        ),
        None => {
            // Older instance profiles predate the persisted Fabric repository
            // checksum. Resolve it now rather than trusting mere existence,
            // then keep it so later launches need no network lookup.
            let (hash, source) = fetch_repository_checksum(&lib.url, &lib.path).await?;
            write_checksum(lib, &hash, source);
            metadata_updated = true;
            (hash, source.to_string())
        }
    };
    let url = fabric_artifact_url(lib)?;
    let existed = dest.is_file();
    if existed && hash.verify_file(&dest).unwrap_or(false) {
        return Ok(EnsureOutcome {
            downloaded: false,
            metadata_updated,
        });
    }
    if dest.exists() && !dest.is_file() {
        return Err(LauncherError::ModLoader(format!(
            "Fabric library destination is not a regular file: {}",
            dest.display()
        )));
    }
    if existed {
        tracing::warn!(
            target: "launcher",
            "Fabric library {} is corrupt; replacing it (checksum source: {})",
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
        "Fabric library {} verified ({}, source: {})",
        lib.name,
        hash.algorithm(),
        source
    );
    Ok(EnsureOutcome {
        downloaded: true,
        metadata_updated,
    })
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

/// Ensure every Fabric library exists and matches its typed checksum.
/// Returns `Ok(true)` when a file was (re)downloaded, `Ok(false)` when the
/// existing file was already valid.
pub(crate) async fn ensure_library(lib: &LoaderLibrary, libraries_dir: &Path) -> Result<bool> {
    let mut lib = lib.clone();
    Ok(ensure_library_mut(&mut lib, libraries_dir)
        .await?
        .downloaded)
}

/// Ensure all libraries, enriching legacy in-memory profiles with repository
/// checksums. Returns `Ok(true)` when the caller should persist the profile.
pub(crate) async fn ensure_libraries(
    profile: &mut LoaderProfile,
    libraries_dir: &Path,
) -> Result<bool> {
    let mut metadata_updated = false;
    for lib in &mut profile.libraries {
        metadata_updated |= ensure_library_mut(lib, libraries_dir)
            .await?
            .metadata_updated;
    }
    Ok(metadata_updated)
}

/// Install Fabric for an instance
pub async fn install(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &std::path::Path,
) -> Result<LoaderProfile> {
    tracing::info!(target: "launcher", "Installing Fabric for MC {} (loader {})", mc_version, loader_version);
    let mut profile = get_profile(mc_version, loader_version).await?;
    ensure_libraries(&mut profile, libraries_dir).await?;
    tracing::info!(target: "launcher", "Fabric install completed for MC {}", mc_version);
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_library(
        name: &str,
        sha1: Option<&str>,
        sha256: Option<&str>,
        sha512: Option<&str>,
    ) -> FabricLibrary {
        FabricLibrary {
            name: name.to_string(),
            url: "https://maven.fabricmc.net/".to_string(),
            sha1: sha1.map(str::to_string),
            sha256: sha256.map(str::to_string),
            sha512: sha512.map(str::to_string),
            size: None,
        }
    }

    #[test]
    fn maven_path_accepts_real_fabric_coordinates() {
        assert_eq!(
            fabric_maven_path("org.ow2.asm:asm:9.10.1").unwrap(),
            "org/ow2/asm/asm/9.10.1/asm-9.10.1.jar"
        );
        assert_eq!(
            fabric_maven_path("net.fabricmc:sponge-mixin:0.17.4+mixin.0.8.7").unwrap(),
            "net/fabricmc/sponge-mixin/0.17.4+mixin.0.8.7/sponge-mixin-0.17.4+mixin.0.8.7.jar"
        );
        assert_eq!(
            fabric_maven_path("example:artifact:1.0:natives-windows").unwrap(),
            "example/artifact/1.0/artifact-1.0-natives-windows.jar"
        );
        assert_eq!(
            fabric_maven_path("example:artifact:1.0@jar").unwrap(),
            "example/artifact/1.0/artifact-1.0.jar"
        );
        assert_eq!(
            fabric_maven_path("example:artifact:1.0:natives-windows@jar").unwrap(),
            "example/artifact/1.0/artifact-1.0-natives-windows.jar"
        );
    }

    #[test]
    fn maven_path_rejects_traversal_and_malformed_coordinates() {
        for invalid in [
            "",
            "group",
            "group:artifact",
            "group:artifact:",
            "group::1.0",
            "../group:artifact:1.0",
            "group:../artifact:1.0",
            "group:artifact:../../1.0",
            "group:artifact:1.0:../../classifier",
            "group/evil:artifact:1.0",
            "group:artifact:1.0@zip",
            "group:artifact:1.0@jar:classifier",
            "group:artifact:1.0:classifier@zip",
            "group:artifact:1.0:classifier:extra",
            "group..evil:artifact:1.0",
            "group:artifact:1.0 bad",
        ] {
            assert!(
                fabric_maven_path(invalid).is_err(),
                "coordinate must be rejected: {:?}",
                invalid
            );
        }
    }

    #[test]
    fn maven_base_accepts_only_the_official_https_origin() {
        for valid in [
            "https://maven.fabricmc.net/",
            "https://maven.fabricmc.net",
            "  https://maven.fabricmc.net/  ",
        ] {
            assert_eq!(fabric_maven_base(valid).unwrap(), FABRIC_MAVEN_BASE);
        }
        for invalid in [
            "http://maven.fabricmc.net/",
            "https://evil.example/",
            "https://maven.fabricmc.net:8443/",
            "https://user@maven.fabricmc.net/",
            "https://maven.fabricmc.net/path/",
            "https://maven.fabricmc.net/?q=1",
        ] {
            assert!(
                fabric_maven_base(invalid).is_err(),
                "base must be rejected: {:?}",
                invalid
            );
        }
    }

    #[test]
    fn metadata_checksum_prefers_the_strongest_algorithm() {
        let sha1 = "a".repeat(40);
        let sha256 = "b".repeat(64);
        let sha512 = "c".repeat(128);

        let all = profile_library("example:all:1", Some(&sha1), Some(&sha256), Some(&sha512));
        assert_eq!(
            metadata_checksum(&all).unwrap().unwrap().algorithm(),
            "sha512"
        );
        let no_sha512 = profile_library("example:no512:1", Some(&sha1), Some(&sha256), None);
        assert_eq!(
            metadata_checksum(&no_sha512).unwrap().unwrap().algorithm(),
            "sha256"
        );
        let only_sha1 = profile_library("example:sha1:1", Some(&sha1), None, None);
        assert_eq!(
            metadata_checksum(&only_sha1).unwrap().unwrap().algorithm(),
            "sha1"
        );
        let none = profile_library("example:none:1", None, None, None);
        assert!(metadata_checksum(&none).unwrap().is_none());
        let empty = profile_library("example:empty:1", Some(""), Some(""), Some(""));
        assert!(metadata_checksum(&empty).unwrap().is_none());

        let malformed_stronger =
            profile_library("example:bad:1", Some(&sha1), None, Some("not-a-hash"));
        assert!(
            metadata_checksum(&malformed_stronger).is_err(),
            "a malformed SHA-512 must not fall back to SHA-1"
        );
    }

    #[test]
    fn repository_checksum_parser_accepts_all_algorithms_and_rejects_bad_values() {
        let sha1 = "a".repeat(40);
        let sha256 = "b".repeat(64);
        let sha512 = "c".repeat(128);
        assert_eq!(
            parse_repository_checksum("sha1", &format!("  {sha1}\n"))
                .unwrap()
                .algorithm(),
            "sha1"
        );
        assert_eq!(
            parse_repository_checksum("sha256", &sha256)
                .unwrap()
                .algorithm(),
            "sha256"
        );
        assert_eq!(
            parse_repository_checksum("sha512", &sha512)
                .unwrap()
                .algorithm(),
            "sha512"
        );
        for (algorithm, value) in [
            ("sha1", String::new()),
            ("sha512", "not-a-hash".to_string()),
            ("sha256", "a".repeat(128)),
            ("sha1", "a".repeat(39)),
        ] {
            assert!(
                parse_repository_checksum(algorithm, &value).is_err(),
                "malformed {algorithm} checksum must fail"
            );
        }
    }

    #[test]
    fn artifact_url_is_limited_to_the_official_fabric_maven() {
        let mut lib = LoaderLibrary {
            name: "org.ow2.asm:asm:9.10.1".to_string(),
            url: "https://maven.fabricmc.net/".to_string(),
            path: "org/ow2/asm/asm/9.10.1/asm-9.10.1.jar".to_string(),
            sha1: None,
            sha256: None,
            sha512: None,
            checksum_source: None,
            size: None,
        };
        assert_eq!(
            fabric_artifact_url(&lib).unwrap(),
            "https://maven.fabricmc.net/org/ow2/asm/asm/9.10.1/asm-9.10.1.jar"
        );
        lib.url = "https://evil.example/".to_string();
        assert!(fabric_artifact_url(&lib).is_err());
        lib.url = "https://maven.fabricmc.net/".to_string();
        lib.path = "../escape.jar".to_string();
        assert!(fabric_artifact_url(&lib).is_err());
    }

    // ── Mock-HTTP regression tests ──────────────────────────────
    //
    // These run the real download pipeline against a loopback origin. The
    // loopback allowance is compiled only under `cfg(test)`; release builds
    // still require the official HTTPS Maven origin.

    use crate::download::tests::{
        assert_clean, direct_proxy_lock, mock_dir, sha1_hex, sha256_hex, sha512_hex, spawn_mock,
        MockServer, MockSpec,
    };
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        PathBuf::from(value)
    }

    fn mock_base(server: &MockServer) -> String {
        server.url.trim_end_matches("artifact.bin").to_string()
    }

    fn loader_library(base: &str, path: &str) -> LoaderLibrary {
        LoaderLibrary {
            name: "example:artifact:1.0".to_string(),
            url: base.to_string(),
            path: path.to_string(),
            sha1: None,
            sha256: None,
            sha512: None,
            checksum_source: None,
            size: None,
        }
    }

    fn set_metadata_hash(lib: &mut LoaderLibrary, algorithm: &str, hex: &str) {
        match algorithm {
            "sha1" => lib.sha1 = Some(hex.to_string()),
            "sha256" => lib.sha256 = Some(hex.to_string()),
            "sha512" => lib.sha512 = Some(hex.to_string()),
            other => panic!("unsupported test algorithm {other}"),
        }
        lib.checksum_source = Some("metadata".to_string());
    }

    fn digest(algorithm: &str, bytes: &[u8]) -> String {
        match algorithm {
            "sha1" => sha1_hex(bytes),
            "sha256" => sha256_hex(bytes),
            "sha512" => sha512_hex(bytes),
            other => panic!("unsupported test algorithm {other}"),
        }
    }

    fn profile_with(libraries: Vec<LoaderLibrary>) -> LoaderProfile {
        LoaderProfile {
            main_class: "net.fabricmc.loader.impl.launch.knot.KnotClient".to_string(),
            libraries,
            jvm_args: Vec::new(),
            game_args: Vec::new(),
            legacy_args: false,
        }
    }

    #[tokio::test]
    async fn ensure_library_verifies_metadata_sha1_sha256_and_sha512() {
        let _proxy = direct_proxy_lock().await;
        let body = b"fabric-library-body";
        for algorithm in ["sha1", "sha256", "sha512"] {
            let expected = digest(algorithm, body);
            let server = spawn_mock(vec![MockSpec::ok(body)]).await;
            let dir = mock_dir(&format!("fabric_meta_{algorithm}"));
            let path = format!("net/fabricmc/example/1.0/example-1.0-{algorithm}.jar");
            let dest = dir.join(&path);
            let mut lib = loader_library(&mock_base(&server), &path);
            set_metadata_hash(&mut lib, algorithm, &expected);

            assert!(ensure_library(&lib, &dir).await.unwrap());
            assert_eq!(std::fs::read(&dest).unwrap(), body);
            assert_clean(
                &with_suffix(&dest, ".part"),
                &with_suffix(&dest, ".part.meta"),
            );
            assert_eq!(
                server.hits.load(Ordering::SeqCst),
                1,
                "{algorithm} download must not be repeated when it verifies"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn ensure_library_skips_a_valid_existing_file_without_network() {
        let _proxy = direct_proxy_lock().await;
        let body = b"already-installed";
        let dir = mock_dir("fabric_existing_valid");
        let path = "net/fabricmc/example/1.0/example-1.0.jar";
        let dest = dir.join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, body).unwrap();
        let server = spawn_mock(vec![MockSpec::status(500)]).await;
        let mut lib = loader_library(&mock_base(&server), path);
        set_metadata_hash(&mut lib, "sha256", &sha256_hex(body));

        assert!(
            !ensure_library(&lib, &dir).await.unwrap(),
            "an already verified file must not be reported as downloaded"
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_library_replaces_a_corrupt_existing_file() {
        let _proxy = direct_proxy_lock().await;
        let body = b"repaired-library";
        let dir = mock_dir("fabric_existing_corrupt");
        let path = "net/fabricmc/example/1.0/example-1.0.jar";
        let dest = dir.join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"truncated").unwrap();
        let server = spawn_mock(vec![MockSpec::ok(body)]).await;
        let mut lib = loader_library(&mock_base(&server), path);
        set_metadata_hash(&mut lib, "sha256", &sha256_hex(body));

        assert!(ensure_library(&lib, &dir).await.unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_clean(
            &with_suffix(&dest, ".part"),
            &with_suffix(&dest, ".part.meta"),
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_library_mismatch_preserves_the_previous_file_and_cleans_temp() {
        let _proxy = direct_proxy_lock().await;
        let body = b"hostile-body";
        let dir = mock_dir("fabric_mismatch_rollback");
        let path = "net/fabricmc/example/1.0/example-1.0.jar";
        let dest = dir.join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"previous-good").unwrap();
        let server = spawn_mock(vec![MockSpec::ok(body); 5]).await;
        let mut lib = loader_library(&mock_base(&server), path);
        set_metadata_hash(&mut lib, "sha256", &"0".repeat(64));

        let err = ensure_library(&lib, &dir)
            .await
            .expect_err("a checksum mismatch must not publish the download");
        assert!(err.to_string().contains("SHA256 mismatch"), "got: {err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(
            &with_suffix(&dest, ".part"),
            &with_suffix(&dest, ".part.meta"),
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_library_upgrades_a_legacy_profile_with_a_repository_checksum() {
        let _proxy = direct_proxy_lock().await;
        let body = b"legacy-fabric-loader";
        let expected = sha512_hex(body);
        let server = spawn_mock(vec![MockSpec::ok(expected.as_bytes()), MockSpec::ok(body)]).await;
        let dir = mock_dir("fabric_legacy_repository");
        let path = "net/fabricmc/fabric-loader/0.19.5/fabric-loader-0.19.5.jar";
        let dest = dir.join(path);
        let mut lib = loader_library(&mock_base(&server), path);

        let outcome = ensure_library_mut(&mut lib, &dir).await.unwrap();
        assert!(outcome.downloaded);
        assert!(outcome.metadata_updated);
        assert_eq!(lib.sha512.as_deref(), Some(expected.as_str()));
        assert_eq!(lib.sha1, None);
        assert_eq!(lib.sha256, None);
        assert_eq!(lib.checksum_source.as_deref(), Some("repository"));
        assert_eq!(
            lib.checksum().unwrap().unwrap().algorithm(),
            "sha512",
            "the persisted repository checksum must drive later verifications"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(
            server.hits.load(Ordering::SeqCst),
            2,
            "one checksum request, one artifact request"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn online_migration_serializes_the_checksum_and_reuses_it_locally() {
        let _proxy = direct_proxy_lock().await;
        let body = b"legacy-fabric-loader";
        let expected = sha512_hex(body);
        let server = spawn_mock(vec![MockSpec::ok(expected.as_bytes()), MockSpec::ok(body)]).await;
        let dir = mock_dir("fabric_migration_persistence");
        let path = "net/fabricmc/fabric-loader/0.19.5/fabric-loader-0.19.5.jar";
        let mut profile = profile_with(vec![loader_library(&mock_base(&server), path)]);

        assert!(ensure_libraries(&mut profile, &dir).await.unwrap());
        let saved = serde_json::to_value(&profile).unwrap();
        assert_eq!(saved["libraries"][0]["sha512"], expected.as_str());
        assert_eq!(saved["libraries"][0]["checksum_source"], "repository");
        assert!(saved["libraries"][0].get("sha256").is_none());
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);

        // The migrated profile now verifies the file locally: the mock has no
        // response left and would return 500 on any further request.
        assert!(!ensure_libraries(&mut profile, &dir).await.unwrap());
        assert_eq!(
            server.hits.load(Ordering::SeqCst),
            2,
            "the second pass must use the persisted checksum, not the network"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn failed_offline_migration_leaves_profile_json_and_files_unchanged() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::status(404),
        ])
        .await;
        let dir = mock_dir("fabric_migration_failure_shape");
        let path = "net/fabricmc/intermediary/1.21.11/intermediary-1.21.11.jar";
        let mut profile = profile_with(vec![loader_library(&mock_base(&server), path)]);
        let before = serde_json::to_value(&profile).unwrap();

        let err = ensure_libraries(&mut profile, &dir).await.unwrap_err();
        assert!(
            err.to_string().contains("no repository checksum"),
            "got: {err}"
        );
        let after = serde_json::to_value(&profile).unwrap();
        assert_eq!(
            before, after,
            "a failed migration must not add a fake checksum to the profile"
        );
        assert!(profile.libraries[0].checksum().unwrap().is_none());
        assert!(!dir.join(path).exists());
        assert_eq!(server.hits.load(Ordering::SeqCst), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_library_refuses_a_missing_repository_checksum() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::status(404),
        ])
        .await;
        let dir = mock_dir("fabric_repository_missing");
        let path = "net/fabricmc/intermediary/1.21.11/intermediary-1.21.11.jar";
        let dest = dir.join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"working-file").unwrap();
        let mut lib = loader_library(&mock_base(&server), path);

        let err = ensure_library_mut(&mut lib, &dir)
            .await
            .expect_err("no repository checksum must fail closed");
        assert!(
            err.to_string().contains("no repository checksum"),
            "got: {err}"
        );
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"working-file",
            "an offline migration failure must not delete a working file"
        );
        assert!(lib.checksum().unwrap().is_none());
        assert_eq!(lib.checksum_source, None);
        assert!(!with_suffix(&dest, ".part").exists());
        assert_eq!(server.hits.load(Ordering::SeqCst), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_library_rejects_a_malformed_persisted_checksum_without_network() {
        let dir = mock_dir("fabric_malformed_persisted");
        let path = "net/fabricmc/example/1.0/example-1.0.jar";
        let dest = dir.join(path);
        let server = spawn_mock(vec![MockSpec::status(500)]).await;
        let mut lib = loader_library(&mock_base(&server), path);
        lib.sha512 = Some("not-a-hash".to_string());

        let err = ensure_library(&lib, &dir)
            .await
            .expect_err("a malformed non-empty checksum must fail");
        assert!(
            err.to_string().to_lowercase().contains("sha512"),
            "got: {err}"
        );
        assert!(!dest.exists());
        assert_eq!(server.hits.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_library_rejects_traversal_paths_before_network() {
        let dir = mock_dir("fabric_traversal");
        for path in ["../escape.jar", "net/fabricmc/../../escape.jar"] {
            let lib = loader_library(FABRIC_MAVEN_BASE, path);
            let err = ensure_library(&lib, &dir)
                .await
                .expect_err("a traversal path must be rejected");
            assert!(
                err.to_string().to_lowercase().contains("unsafe"),
                "got: {err}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_library_rejects_an_untrusted_artifact_origin() {
        let dir = mock_dir("fabric_untrusted_origin");
        let mut lib = loader_library("https://evil.example/", "example/artifact/1.0/a.jar");
        lib.sha1 = Some("a".repeat(40));
        let err = ensure_library(&lib, &dir)
            .await
            .expect_err("only the official Fabric Maven may serve libraries");
        assert!(
            err.to_string().contains("official Maven repository"),
            "got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn repository_checksum_refuses_a_cross_origin_redirect() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::redirect(
            "https://evil.example/payload.sha512",
        )])
        .await;

        let err = fetch_repository_checksum(
            &mock_base(&server),
            "net/fabricmc/fabric-loader/0.19.5/fabric-loader-0.19.5.jar",
        )
        .await
        .expect_err("a checksum must come from the exact Maven origin");
        assert!(err.to_string().contains("302"), "got: {err}");
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ensure_library_refuses_an_untrusted_redirect() {
        let _proxy = direct_proxy_lock().await;
        let body = b"redirected-body";
        let dir = mock_dir("fabric_untrusted_redirect");
        let path = "net/fabricmc/example/1.0/example-1.0.jar";
        let dest = dir.join(path);
        // `download_file_verified` retries a terminal HTTP error; provide one
        // redirect response per attempt instead of letting the mock fall back
        // to 500s with exponential backoff.
        let server = spawn_mock(vec![
            MockSpec::redirect("https://evil.example/payload.jar");
            5
        ])
        .await;
        let mut lib = loader_library(&mock_base(&server), path);
        set_metadata_hash(&mut lib, "sha256", &sha256_hex(body));

        let err = ensure_library(&lib, &dir)
            .await
            .expect_err("a redirect away from the Maven origin must be refused");
        assert!(
            err.to_string().contains("redirect target is not allowed"),
            "got: {err}"
        );
        assert!(!dest.exists());
        assert_clean(
            &with_suffix(&dest, ".part"),
            &with_suffix(&dest, ".part.meta"),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ensure_libraries_handles_multiple_and_mixed_provenance() {
        let _proxy = direct_proxy_lock().await;
        let first_body = b"asm-library";
        let second_body = b"intermediary-library";
        let first_digest = sha1_hex(first_body);
        let second_digest = sha256_hex(second_body);
        // Order: metadata lib downloads first, then the legacy lib resolves a
        // repository checksum and downloads.
        let server = spawn_mock(vec![
            MockSpec::ok(first_body),
            MockSpec::status(404),
            MockSpec::ok(second_digest.as_bytes()),
            MockSpec::ok(second_body),
        ])
        .await;
        let dir = mock_dir("fabric_multiple");
        let base = mock_base(&server);
        let first_path = "org/ow2/asm/asm/9.10.1/asm-9.10.1.jar";
        let second_path = "net/fabricmc/intermediary/1.21.11/intermediary-1.21.11.jar";
        let mut first = loader_library(&base, first_path);
        set_metadata_hash(&mut first, "sha1", &first_digest);
        let second = loader_library(&base, second_path);
        let mut profile = profile_with(vec![first, second]);

        assert!(ensure_libraries(&mut profile, &dir).await.unwrap());
        assert_eq!(std::fs::read(dir.join(first_path)).unwrap(), first_body);
        assert_eq!(std::fs::read(dir.join(second_path)).unwrap(), second_body);
        assert_eq!(
            profile.libraries[1].sha256.as_deref(),
            Some(second_digest.as_str())
        );
        assert_eq!(
            profile.libraries[1].checksum_source.as_deref(),
            Some("repository")
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn profile_library_keeps_metadata_checksum_provenance() {
        let digest = "a".repeat(40);
        let resolved = resolve_profile_library(FabricLibrary {
            name: "org.ow2.asm:asm:9.10.1".to_string(),
            url: FABRIC_MAVEN_BASE.to_string(),
            sha1: Some(digest.clone()),
            sha256: None,
            sha512: None,
            size: Some(12345),
        })
        .await
        .unwrap();
        assert_eq!(resolved.path, "org/ow2/asm/asm/9.10.1/asm-9.10.1.jar");
        assert_eq!(resolved.sha1.as_deref(), Some(digest.as_str()));
        assert_eq!(resolved.checksum_source.as_deref(), Some("metadata"));
        assert_eq!(resolved.size, Some(12345));
        assert_eq!(resolved.checksum().unwrap().unwrap().algorithm(), "sha1");
    }

    #[tokio::test]
    async fn profile_library_rejects_malformed_metadata_without_weaker_fallback() {
        let err = resolve_profile_library(FabricLibrary {
            name: "org.ow2.asm:asm:9.10.1".to_string(),
            url: FABRIC_MAVEN_BASE.to_string(),
            sha1: Some("a".repeat(40)),
            sha256: None,
            sha512: Some("not-a-hash".to_string()),
            size: None,
        })
        .await
        .expect_err("a malformed SHA-512 must not fall back to SHA-1");
        assert!(
            err.to_string().to_lowercase().contains("sha512"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_profile_library_records_repository_sha512() {
        let _proxy = direct_proxy_lock().await;
        let body = b"fabric-artifact";
        let expected = sha512_hex(body);
        let server = spawn_mock(vec![MockSpec::ok(expected.as_bytes())]).await;
        let resolved = resolve_profile_library(FabricLibrary {
            name: "net.fabricmc:fabric-loader:0.19.5".to_string(),
            url: mock_base(&server),
            sha1: None,
            sha256: None,
            sha512: None,
            size: None,
        })
        .await
        .unwrap();

        assert_eq!(
            resolved.path,
            "net/fabricmc/fabric-loader/0.19.5/fabric-loader-0.19.5.jar"
        );
        assert_eq!(resolved.sha512.as_deref(), Some(expected.as_str()));
        assert_eq!(resolved.checksum_source.as_deref(), Some("repository"));
        assert_eq!(resolved.checksum().unwrap().unwrap().algorithm(), "sha512");
    }

    #[tokio::test]
    async fn resolve_profile_library_treats_empty_metadata_as_absent() {
        let _proxy = direct_proxy_lock().await;
        let body = b"fabric-artifact";
        let expected = sha1_hex(body);
        let server = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::ok(expected.as_bytes()),
        ])
        .await;
        let resolved = resolve_profile_library(FabricLibrary {
            name: "net.fabricmc:intermediary:1.21.11".to_string(),
            url: mock_base(&server),
            sha1: Some(String::new()),
            sha256: None,
            sha512: None,
            size: None,
        })
        .await
        .unwrap();

        assert_eq!(resolved.sha1.as_deref(), Some(expected.as_str()));
        assert_eq!(resolved.checksum_source.as_deref(), Some("repository"));
    }
}

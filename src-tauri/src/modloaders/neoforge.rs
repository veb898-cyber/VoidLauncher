use super::processor_cache::{sha256_bytes, ProcessorCache};
use super::{LoaderLibrary, LoaderProfile, LoaderVersionPage};
use crate::error::{LauncherError, Result};
use crate::versions::maven_to_path;
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use tauri::Emitter;

/// Maven group path + version used in the artifact URL for a given NeoForge
/// `neo_version` (bare, e.g. `47.1.106` or `21.4.157`) for a Minecraft version.
///
/// NeoForge 1.20.1 (the 47.x generation) is a fork of Forge and lives under
/// the `net.neoforged:forge` coordinate *with* the Minecraft version prefixed
/// to the artifact version (maven path `net/neoforged/forge/{mc}-{loader}/...`,
/// artifact `forge-{mc}-{loader}.jar`). Every newer NeoForge (1.20.2+,
/// renumbered to 20.x) uses its own `net.neoforged:neoforge` coordinate with
/// the bare loader version (maven path `net/neoforged/neoforge/{loader}/...`,
/// artifact `neoforge-{loader}.jar`). Verifying against
/// `maven.neoforged.net`: only the 1.20.1 line exists under `neoforged/forge`,
/// everything else is under `neoforged/neoforge`.
///
/// Returns `(group_path, maven_version)` — e.g. `("net/neoforged/forge",
/// "1.20.1-47.1.106")` for 1.20.1, or `("net/neoforged/neoforge", "21.4.157")`
/// for modern NeoForge.
fn neoforge_maven(mc_version: &str, neo_version: &str) -> (String, String) {
    if mc_version == "1.20.1" {
        (
            "net/neoforged/forge".to_string(),
            format!("{}-{}", mc_version, neo_version),
        )
    } else {
        (
            "net/neoforged/neoforge".to_string(),
            neo_version.to_string(),
        )
    }
}

/// NeoForge install profile (same structure as Minecraft version JSON)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct NeoForgeInstallProfile {
    #[serde(default)]
    id: String,
    #[serde(rename = "mainClass", default)]
    main_class: String,
    #[serde(default)]
    libraries: Vec<NeoForgeLibrary>,
    #[serde(default)]
    arguments: Option<NeoForgeArguments>,
    #[serde(rename = "minecraftArguments", default)]
    minecraft_arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct NeoForgeLibrary {
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    downloads: Option<NeoForgeDownloads>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct NeoForgeDownloads {
    artifact: Option<NeoForgeArtifact>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct NeoForgeArtifact {
    path: Option<String>,
    url: Option<String>,
    sha1: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    sha512: Option<String>,
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct NeoForgeArguments {
    #[serde(default)]
    game: Vec<serde_json::Value>,
    #[serde(default)]
    jvm: Vec<serde_json::Value>,
}

/// Fetch a page of available NeoForge versions for a MC version.
///
/// We delegate to the Prism metadata mirror (`prism_meta`) rather than
/// hitting `maven.neoforged.net/api/maven/versions/releases/...` directly.
/// The Maven API endpoint is notoriously slow from many regions
/// (60s+ on cold cache, frequent outright timeouts) which is what
/// caused NeoForge to appear to "load forever" in the wizard. The
/// Prism mirror is a curated JSON file served from a single fast host
/// and returns in single-digit seconds.
///
/// The mirror filters by `requires.net.minecraft` for us, so passing
/// `Some(mc_version)` is enough — we no longer need the manual
/// `21.1`-prefix string matching that the old code did.
///
/// The wizard drives this with infinite scroll: it asks for
/// `PAGE_SIZE` items, appends them, and asks for the next page at
/// `accumulator.length`. The underlying `prism_meta` call caches
/// the parsed 1633-entry index on first request so every page
/// after the first is instant.
///
/// On fetch/parse failure the underlying `prism_meta` call returns an
/// error; the wizard shows it with a retry button (a stale disk cache is
/// used first when available).
pub async fn get_loader_versions(
    mc_version: &str,
    offset: usize,
    limit: usize,
) -> Result<LoaderVersionPage> {
    super::prism_meta::fetch_loader_versions("net.neoforged", Some(mc_version), offset, limit).await
}

const MAX_STANDALONE_PROFILE_BYTES: usize = 8 * 1024 * 1024;

fn standalone_profile_timeout() -> std::time::Duration {
    if cfg!(test) {
        std::time::Duration::from_millis(200)
    } else {
        std::time::Duration::from_secs(30)
    }
}

fn standalone_profile_url(mc_version: &str, neo_version: &str) -> String {
    let (group_path, maven_version) = neoforge_maven(mc_version, neo_version);
    format!(
        "https://maven.neoforged.net/releases/{}/{}/{}-{}.json",
        group_path,
        maven_version,
        group_path.rsplit('/').next().unwrap_or("neoforge"),
        maven_version
    )
}

/// Fetch a legacy standalone NeoForge profile under a strict trust policy.
///
/// The profile is metadata, so it is accepted only from the official HTTPS
/// host, with exact-host redirects, a bounded response and a publisher
/// checksum sidecar. Any failure returns `None`; the caller then uses the
/// strictly stronger path: a checksum-verified installer JAR.
async fn fetch_standalone_profile(profile_url: &str) -> Result<Option<NeoForgeInstallProfile>> {
    let parsed = reqwest::Url::parse(profile_url)
        .map_err(|e| LauncherError::ModLoader(format!("Invalid NeoForge profile URL: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| LauncherError::ModLoader("NeoForge profile URL has no host".into()))?;
    if parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(LauncherError::ModLoader(
            "NeoForge profile URL has an unsafe shape".into(),
        ));
    }
    #[cfg(not(test))]
    if parsed.scheme() != "https" || host != "maven.neoforged.net" {
        return Err(LauncherError::ModLoader(
            "NeoForge profile URL is outside the official Maven origin".into(),
        ));
    }
    #[cfg(test)]
    if !((parsed.scheme() == "http" && host == "127.0.0.1")
        || (parsed.scheme() == "https" && host == "maven.neoforged.net"))
    {
        return Err(LauncherError::ModLoader(
            "NeoForge profile URL is outside the allowed test/original origins".into(),
        ));
    }

    let (checksum, _source) = match super::artifact::fetch_repository_checksum(
        profile_url,
        "NeoForge standalone profile",
    )
    .await
    {
        Ok(value) => value,
        Err(e) => {
            tracing::debug!(
                target: "launcher",
                "NeoForge standalone profile has no usable checksum; using verified installer instead: {}",
                e
            );
            return Ok(None);
        }
    };

    let client = crate::download::http_client_for_exact_host(host)?;
    let mut response = match crate::download::send_with_fallback_same_host(
        client
            .get(profile_url)
            .timeout(standalone_profile_timeout()),
        host,
    )
    .await
    {
        Ok(response) => response,
        Err(e) => {
            tracing::debug!(
                target: "launcher",
                "NeoForge standalone profile request failed; using verified installer instead: {}",
                e
            );
            return Ok(None);
        }
    };
    if !response.status().is_success() {
        tracing::debug!(
            target: "launcher",
            "NeoForge standalone profile returned {}; using verified installer instead",
            response.status()
        );
        return Ok(None);
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| {
        LauncherError::ModLoader(format!("Failed to read NeoForge standalone profile: {e}"))
    })? {
        if body.len().saturating_add(chunk.len()) > MAX_STANDALONE_PROFILE_BYTES {
            return Err(LauncherError::ModLoader(
                "NeoForge standalone profile is too large".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    if !checksum.verify_bytes(&body) {
        tracing::warn!(
            target: "launcher",
            "NeoForge standalone profile failed its publisher checksum; using verified installer instead"
        );
        return Ok(None);
    }
    let profile = serde_json::from_slice(&body).map_err(|e| {
        LauncherError::ModLoader(format!("Failed to parse NeoForge standalone profile: {e}"))
    })?;
    Ok(Some(profile))
}

/// Get NeoForge launch profile.
///
/// The legacy standalone JSON is only an optimization. It must be checksum
/// verified and fetched with an exact-host client; otherwise the verified
/// installer JAR is authoritative. The old global-client/global-redirect path
/// is intentionally gone.
pub async fn get_profile(mc_version: &str, neo_version: &str) -> Result<LoaderProfile> {
    let (_, maven_version) = neoforge_maven(mc_version, neo_version);
    let data_dir = dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("VoidLauncher");
    let cache = ProcessorCache::new(&data_dir, "neoforge", mc_version, &maven_version)?;
    get_profile_cached(mc_version, neo_version, &cache).await
}

pub(crate) async fn get_profile_cached(
    mc_version: &str,
    neo_version: &str,
    cache: &ProcessorCache,
) -> Result<LoaderProfile> {
    let profile_url = standalone_profile_url(mc_version, neo_version);
    match fetch_standalone_profile(&profile_url).await {
        Ok(Some(profile)) => return build_profile(profile),
        Ok(None) => {}
        Err(e) => {
            tracing::debug!(
                target: "launcher",
                "NeoForge standalone profile rejected ({}); using verified installer instead",
                e
            );
        }
    }
    let (profile, _installer_path) = download_installer(mc_version, neo_version, cache).await?;
    build_profile(profile)
}

/// Download the NeoForge installer JAR (with mirror fallbacks) and return
/// the parsed `version.json` profile plus the installer's on-disk path.
///
/// The installer path is NOT deleted here: the processor pipeline in
/// `install` needs the JAR (it carries `data/client.lzma` and the
/// `install_profile.json` processor list).
async fn download_installer(
    mc_version: &str,
    neo_version: &str,
    cache: &ProcessorCache,
) -> Result<(NeoForgeInstallProfile, PathBuf)> {
    download_installer_inner(mc_version, neo_version, cache, false).await
}

/// Вариант для `ensure_processor_jars_inner`, который уже владеет cache
/// operation lock.
async fn download_installer_locked(
    mc_version: &str,
    neo_version: &str,
    cache: &ProcessorCache,
) -> Result<(NeoForgeInstallProfile, PathBuf)> {
    download_installer_inner(mc_version, neo_version, cache, true).await
}

async fn download_installer_inner(
    mc_version: &str,
    neo_version: &str,
    cache: &ProcessorCache,
    locked: bool,
) -> Result<(NeoForgeInstallProfile, PathBuf)> {
    super::validate_loader_request(mc_version, neo_version)?;
    let (group_path, maven_version) = neoforge_maven(mc_version, neo_version);
    let artifact = group_path.rsplit('/').next().unwrap_or("neoforge");
    let rest = format!(
        "{}/{}/{}-{}-installer.jar",
        group_path, maven_version, artifact, maven_version
    );
    let installer_urls = vec![
        format!("{}{}", super::artifact::NEOFORGE_MAVEN, rest),
        format!("{}{}", super::artifact::NEOFORGE_MIRROR_MAVEN, rest),
    ];

    tracing::info!(target: "launcher", "Fetching NeoForge installer JAR: {}", installer_urls[0]);

    // Both the official origin and the sanctioned mirror have checksum
    // sidecars. The persistent cache keeps the verified installer and its
    // provenance across %TEMP% cleanup.
    if locked {
        cache
            .ensure_installer_locked(
                &installer_urls,
                super::artifact::NEOFORGE_HOSTS,
                &format!("NeoForge installer {maven_version}"),
            )
            .await?;
    } else {
        cache
            .ensure_installer(
                &installer_urls,
                super::artifact::NEOFORGE_HOSTS,
                &format!("NeoForge installer {maven_version}"),
            )
            .await?;
    }
    let installer_path = cache.installer.clone();

    if !super::forge::installer_jar_is_valid(&installer_path) {
        let _ = std::fs::remove_file(&installer_path);
        let _ = std::fs::remove_file(&cache.installer_identity);
        if locked {
            cache
                .ensure_installer_locked(
                    &installer_urls,
                    super::artifact::NEOFORGE_HOSTS,
                    &format!("NeoForge installer {maven_version}"),
                )
                .await?;
        } else {
            cache
                .ensure_installer(
                    &installer_urls,
                    super::artifact::NEOFORGE_HOSTS,
                    &format!("NeoForge installer {maven_version}"),
                )
                .await?;
        }
        if !super::forge::installer_jar_is_valid(&cache.installer) {
            let _ = std::fs::remove_file(&cache.installer);
            let _ = std::fs::remove_file(&cache.installer_identity);
            return Err(LauncherError::ModLoader(format!(
                "Downloaded NeoForge installer {maven_version} is not a valid installer archive"
            )));
        }
    }

    let jar_bytes = std::fs::read(&installer_path).map_err(|e| {
        LauncherError::ModLoader(format!("Failed to read NeoForge installer JAR: {}", e))
    })?;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid NeoForge installer JAR: {}", e)))?;

    let mut install_profile_str = String::new();
    let mut version_json_str = String::new();
    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            if entry.name().ok().as_deref() == Some("install_profile.json") {
                entry
                    .read_to_string(&mut install_profile_str)
                    .map_err(|e| {
                        LauncherError::ModLoader(format!(
                            "Failed to read install_profile.json: {}",
                            e
                        ))
                    })?;
            } else if entry.name().ok().as_deref() == Some("version.json") {
                entry.read_to_string(&mut version_json_str).map_err(|e| {
                    LauncherError::ModLoader(format!("Failed to read version.json: {}", e))
                })?;
            }
        }
    }

    if install_profile_str.is_empty() {
        return Err(LauncherError::ModLoader(
            "NeoForge installer JAR missing install_profile.json".into(),
        ));
    }

    // NeoForge installer JAR structure:
    //   install_profile.json — installer metadata, points to the actual version JSON via "json" field
    //   version.json — the real version profile with mainClass, libraries, arguments
    // The install_profile.json has "json": "/version.json" pointing to the version profile.
    // We read version.json from the JAR as the actual profile.

    let profile: NeoForgeInstallProfile = if !version_json_str.is_empty() {
        // version.json is the actual version profile (same format as Minecraft version JSON)
        serde_json::from_str(&version_json_str).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to parse NeoForge version.json: {}", e))
        })?
    } else {
        // Fallback: try install_profile.json directly
        let parsed: serde_json::Value =
            serde_json::from_str(&install_profile_str).map_err(|e| {
                LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e))
            })?;
        if let Some(version_info) = parsed.get("versionInfo") {
            serde_json::from_value(version_info.clone()).map_err(|e| {
                LauncherError::ModLoader(format!("Failed to parse NeoForge versionInfo: {}", e))
            })?
        } else {
            serde_json::from_value(parsed).map_err(|e| {
                LauncherError::ModLoader(format!("Failed to parse NeoForge install profile: {}", e))
            })?
        }
    };

    Ok((profile, installer_path))
}

fn build_profile(profile: NeoForgeInstallProfile) -> Result<LoaderProfile> {
    let mut libraries = Vec::new();
    for lib in &profile.libraries {
        let (url, path, sha1, sha256, sha512, checksum_source, size) =
            if let Some(downloads) = &lib.downloads {
                if let Some(artifact) = &downloads.artifact {
                    let lib_path = artifact
                        .path
                        .clone()
                        .unwrap_or_else(|| maven_to_path(&lib.name));
                    let base = super::artifact::declared_maven_base(
                        artifact.url.as_deref(),
                        &lib.name,
                        super::artifact::NEOFORGE_MAVEN,
                    )?;
                    let lib_url = super::artifact::join_maven_url(&base, &lib_path);
                    let sha1 = artifact.sha1.clone().filter(|s| !s.is_empty());
                    let sha256 = artifact.sha256.clone().filter(|s| !s.is_empty());
                    let sha512 = artifact.sha512.clone().filter(|s| !s.is_empty());
                    let source = super::artifact::has_metadata_checksum(
                        sha1.as_deref(),
                        sha256.as_deref(),
                        sha512.as_deref(),
                    )
                    .then(|| "metadata".to_string());
                    (
                        lib_url,
                        lib_path,
                        sha1,
                        sha256,
                        sha512,
                        source,
                        artifact.size,
                    )
                } else {
                    let path = maven_to_path(&lib.name);
                    let base = super::artifact::declared_maven_base(
                        lib.url.as_deref(),
                        &lib.name,
                        super::artifact::NEOFORGE_MAVEN,
                    )?;
                    (
                        super::artifact::join_maven_url(&base, &path),
                        path,
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                }
            } else {
                let path = maven_to_path(&lib.name);
                let base = super::artifact::declared_maven_base(
                    lib.url.as_deref(),
                    &lib.name,
                    super::artifact::NEOFORGE_MAVEN,
                )?;
                (
                    super::artifact::join_maven_url(&base, &path),
                    path,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            };

        libraries.push(LoaderLibrary {
            name: lib.name.clone(),
            url,
            path,
            sha1,
            sha256,
            sha512,
            checksum_source,
            size,
        });
    }

    // Loader metadata is remote input too: a path containing `..`, an
    // absolute path or a drive prefix must never reach a filesystem join.
    for lib in &libraries {
        if crate::import::check_safe_remote_path(&lib.path).is_err() {
            return Err(LauncherError::ModLoader(format!(
                "Unsafe library path in NeoForge profile: {}",
                lib.path
            )));
        }
    }

    let (game_args, jvm_args) = if let Some(args) = &profile.arguments {
        let game = args
            .game
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let jvm = args
            .jvm
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        (game, jvm)
    } else {
        (Vec::new(), Vec::new())
    };

    Ok(LoaderProfile {
        main_class: profile.main_class,
        libraries,
        jvm_args,
        game_args,
        legacy_args: false,
    })
}

/// Persistent installer cache for an existing instance.
pub(crate) fn installer_cache(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
) -> Option<ProcessorCache> {
    let (_, maven_version) = neoforge_maven(mc_version, loader_version);
    let data_dir = libraries_dir.parent()?;
    ProcessorCache::new(data_dir, "neoforge", mc_version, &maven_version).ok()
}

/// Client JAR path used by NeoForge generations whose `install_profile.json`
/// names the loader's own `:client` classifier (20.x/21.x and the 1.20.1
/// `net.neoforged:forge` fork). Modern NeoForge (26.x) moved the output to
/// `net.neoforged:minecraft-client-patched`, so this is a fallback only — the
/// installer metadata is authoritative whenever it is available.
fn legacy_client_jar_path(libraries_dir: &Path, mc_version: &str, loader_version: &str) -> PathBuf {
    let (group_path, maven_version) = neoforge_maven(mc_version, loader_version);
    let artifact = group_path.rsplit('/').next().unwrap_or("neoforge");
    libraries_dir
        .join(&group_path)
        .join(&maven_version)
        .join(format!("{}-{}-client.jar", artifact, maven_version))
}

/// Relative library path of the patched client declared by
/// `install_profile.json` → `data.PATCHED.client`.
///
/// The value is a Maven coordinate, either bare
/// (`net.neoforged:minecraft-client-patched:26.3.0.99`) or in Maven bracket
/// form (`[net.neoforged:neoforge:21.4.157:client]`). It comes from remote
/// metadata, so anything that is not a plain coordinate (placeholders,
/// extensions, path separators, traversal) is rejected rather than joined
/// into the libraries directory.
fn patched_client_rel_path(install_profile: &serde_json::Value) -> Option<PathBuf> {
    let raw = install_profile
        .get("data")?
        .get("PATCHED")?
        .get("client")?
        .as_str()?
        .trim();
    let coord = match raw.strip_prefix('[') {
        Some(inner) => inner.strip_suffix(']')?.trim(),
        None => raw,
    };
    if coord.is_empty()
        || coord.contains('{')
        || coord.contains('\\')
        || coord.contains('/')
        || coord.contains('@')
        || coord.split(':').count() < 3
    {
        tracing::warn!(
            target: "launcher",
            "Ignoring unusable NeoForge PATCHED coordinate: {}",
            raw
        );
        return None;
    }
    let rel = maven_to_path(coord);
    if crate::import::check_safe_remote_path(&rel).is_err() {
        tracing::warn!(
            target: "launcher",
            "Ignoring unsafe NeoForge PATCHED coordinate: {}",
            raw
        );
        return None;
    }
    Some(PathBuf::from(rel))
}

/// Best-effort location used by NeoForge 26.x (`minecraft-client-patched`),
/// needed when the cached installer is gone: verification must not report a
/// healthy install as broken just because `%TEMP%` was cleaned. The path is
/// version-scoped, so it cannot point at another loader version's output.
fn modern_client_jar_guess(
    libraries_dir: &Path,
    mc_version: &str,
    loader_version: &str,
) -> PathBuf {
    let (_, maven_version) = neoforge_maven(mc_version, loader_version);
    libraries_dir
        .join("net/neoforged/minecraft-client-patched")
        .join(&maven_version)
        .join(format!("minecraft-client-patched-{}.jar", maven_version))
}

/// Every path the processor-generated client JAR may occupy, in priority
/// order. With installer metadata at hand the declared output is
/// authoritative; the legacy path stays as a fallback so instances installed
/// by older launcher builds keep working. Without metadata (cached installer
/// gone), the known 26.x layout is checked before the legacy one.
pub(crate) fn expected_client_jar_paths(
    libraries_dir: &Path,
    mc_version: &str,
    loader_version: &str,
    install_profile: Option<&serde_json::Value>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    match install_profile.and_then(patched_client_rel_path) {
        Some(rel) => paths.push(libraries_dir.join(rel)),
        None => {
            // 1.20.1 has no `minecraft-client-patched` layout; skip the noise.
            if mc_version != "1.20.1" {
                paths.push(modern_client_jar_guess(
                    libraries_dir,
                    mc_version,
                    loader_version,
                ));
            }
        }
    }
    let legacy = legacy_client_jar_path(libraries_dir, mc_version, loader_version);
    if !paths.contains(&legacy) {
        paths.push(legacy);
    }
    paths
}

/// First candidate that is a structurally valid JAR.
pub(crate) fn find_client_jar(paths: &[PathBuf]) -> Option<PathBuf> {
    paths
        .iter()
        .find(|path| super::forge::is_usable_jar(path))
        .cloned()
}

/// Read only `install_profile.json` from an installer JAR, for fast-path
/// checks that must not download anything.
pub(crate) fn read_install_profile_from_installer(
    installer_path: &Path,
) -> Option<serde_json::Value> {
    let bytes = std::fs::read(installer_path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let text = super::forge::read_entry_from_jar(&mut archive, "install_profile.json")?;
    serde_json::from_str(&text).ok()
}

/// Read-only generation inspection for Deep Verify. It reuses the same
/// installer metadata and journal checks as Forge without downloading or
/// mutating anything.
pub(crate) fn inspect_generation(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
) -> super::processor_cache::GenerationInspection {
    let (group_path, maven_version) = neoforge_maven(mc_version, loader_version);
    super::forge::inspect_processor_generation(
        "neoforge",
        mc_version,
        &maven_version,
        &maven_version,
        &group_path,
        libraries_dir,
        versions_dir,
        false,
    )
}

/// Self-heal NeoForge at launch: if the processor-generated client JAR is
/// missing/corrupt (e.g. quarantined by antivirus), re-run the installer.
/// The validated installer is cached, so a repair does not re-download it.
pub async fn ensure_processor_jars(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
    app: Option<&tauri::AppHandle>,
) -> Result<()> {
    ensure_processor_jars_inner(mc_version, loader_version, libraries_dir, versions_dir, app).await
}

async fn ensure_processor_jars_inner(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
    app: Option<&tauri::AppHandle>,
) -> Result<()> {
    // The client JAR path is derived from both versions before any network
    // call, so a tampered instance.json must not reach the filesystem.
    super::validate_loader_request(mc_version, loader_version)?;
    let (group_path, maven_version) = neoforge_maven(mc_version, loader_version);
    let data_dir = libraries_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let cache = ProcessorCache::new(&data_dir, "neoforge", mc_version, &maven_version)?;
    // Serialize the complete prepare -> recover -> stage -> publish sequence
    // for every instance that shares this loader version.
    let cache_lock = cache.acquire_operation_lock().await;
    cache.prepare()?;
    cache.recover_pending_transaction(libraries_dir)?;

    let vd = versions_dir.join(mc_version);
    let client_jar = if vd.join("client.jar").exists() {
        vd.join("client.jar")
    } else {
        vd.join(format!("{}.jar", mc_version))
    };
    let root_dir = versions_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| versions_dir.to_path_buf());
    if !client_jar.exists() {
        return Err(LauncherError::ModLoader(format!(
            "Client JAR not found at {:?}",
            client_jar
        )));
    }

    // The verified installer is the authoritative metadata source. The
    // standalone JSON remains only a strict optimization in get_profile().
    let (_profile, installer_path) =
        download_installer_locked(mc_version, loader_version, &cache).await?;
    let install_profile_str = read_install_profile_string(&installer_path)?;
    let install_profile: serde_json::Value =
        serde_json::from_str(&install_profile_str).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to parse install_profile.json: {e}"))
        })?;
    let install_profile_sha256 = sha256_bytes(install_profile_str.as_bytes());
    let has_processors = install_profile
        .get("processors")
        .and_then(|p| p.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !has_processors {
        return Ok(());
    }

    let vars = super::forge::build_processor_vars(
        &install_profile,
        &installer_path,
        libraries_dir,
        mc_version,
        &client_jar,
        &root_dir,
        &group_path,
        &maven_version,
    );
    let plan = super::forge::processor_output_plan(&install_profile, &vars, libraries_dir)?;
    if plan.is_empty() {
        return Err(LauncherError::ModLoader(
            "NeoForge installer declares processors but no verifiable outputs".into(),
        ));
    }
    let expected = super::forge::expected_outputs_from_plan(&plan, libraries_dir)?;
    let processors = super::forge::processor_identities_from_install_profile(&install_profile)?;
    let installer_identity = cache.load_installer_identity()?.ok_or_else(|| {
        LauncherError::ModLoader("NeoForge installer provenance is missing from the cache".into())
    })?;

    if let Some(manifest) = cache.load_generation_manifest()? {
        if let Ok(processor_libraries) =
            super::forge::processor_library_identities(&install_profile, libraries_dir)
        {
            if cache
                .validate_generation_manifest(
                    &manifest,
                    "neoforge",
                    mc_version,
                    &maven_version,
                    &installer_identity,
                    &install_profile_sha256,
                    &client_jar,
                    libraries_dir,
                    &processors,
                    &processor_libraries,
                    &expected,
                )
                .is_ok()
            {
                tracing::debug!(
                    target: "launcher",
                    "NeoForge processor outputs match persistent provenance; skipping regeneration"
                );
                return Ok(());
            }
        }
    }

    tracing::info!(
        target: "launcher",
        "NeoForge processor provenance is missing or stale; regenerating outputs..."
    );

    tracing::info!(target: "launcher", "Downloading NeoForge processor libraries...");
    if let Some(libraries) = install_profile.get("libraries").and_then(|l| l.as_array()) {
        let pl_total = libraries.len();
        for (i, lib_val) in libraries.iter().enumerate() {
            if let Some(app_handle) = app {
                let _ = app_handle.emit("loader-install-progress", serde_json::json!({
                    "stage": "downloading",
                    "message": format!("Downloading processor libraries ({}/{})", i + 1, pl_total),
                }));
            }
            super::forge::download_processor_lib(
                lib_val,
                libraries_dir,
                super::artifact::NEOFORGE_MAVEN,
                super::artifact::NEOFORGE_HOSTS,
            )
            .await?;
        }
    }

    // Extract data files (client.lzma etc.) from the installer.
    let jar_bytes = std::fs::read(&installer_path)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to read installer JAR: {e}")))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid installer JAR: {e}")))?;
    if let Some(data_map) = install_profile.get("data").and_then(|d| d.as_object()) {
        for (key, entry) in data_map {
            let client_val = entry.get("client").and_then(|c| c.as_str()).unwrap_or("");
            if client_val.starts_with('/') {
                let jar_path = client_val.trim_start_matches('/');
                let fname = Path::new(jar_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("data");
                let key_lower = key.to_lowercase();
                if crate::import::check_safe_file_name(&key_lower).is_err() {
                    return Err(LauncherError::ModLoader(format!(
                        "NeoForge install profile has an unsafe data key: {key}"
                    )));
                }
                let dest = super::forge::data_file_path(
                    libraries_dir,
                    &group_path,
                    &maven_version,
                    &key_lower,
                    fname,
                );
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if !super::forge::extract_entry_from_jar(&mut archive, jar_path, &dest) {
                    return Err(LauncherError::ModLoader(format!(
                        "NeoForge installer is missing data file '{jar_path}' required by the processors"
                    )));
                }
            }
        }
    }
    drop(archive);

    let (output_specs, output_overrides) = super::forge::stage_processor_outputs(&cache, &plan)?;
    let java_path = super::forge::ensure_java_for_processors(&data_dir, mc_version, app).await?;
    tracing::info!(target: "launcher", "Running NeoForge processors with Java: {:?}", java_path);
    let profile_json = install_profile.clone();
    let installer = installer_path.clone();
    let java = java_path.clone();
    let libs = libraries_dir.to_path_buf();
    let ver = mc_version.to_string();
    let cjar = client_jar.clone();
    let root = root_dir.clone();
    let data_base_owned = group_path.clone();
    let data_ver = maven_version.clone();
    let (pipeline, cache_lock) = cache
        .run_with_owned_operation_lock(cache_lock, move || {
            super::forge::run_forge_processors(super::forge::ForgeProcessorInputs {
                install_profile: &profile_json,
                installer_path: &installer,
                java_path: &java,
                libraries_dir: &libs,
                mc_version: &ver,
                client_jar: &cjar,
                root_dir: &root,
                data_base: &data_base_owned,
                data_version: &data_ver,
                output_overrides: Some(&output_overrides),
            })
        })
        .await?;
    let _cache_lock = cache_lock;
    pipeline?;

    cache.publish_outputs(libraries_dir, &output_specs)?;
    super::forge::verify_processor_outputs(&plan)?;
    let processor_libraries =
        super::forge::processor_library_identities(&install_profile, libraries_dir)?;
    let manifest = super::forge::build_generation_manifest(
        "neoforge",
        mc_version,
        &maven_version,
        installer_identity,
        install_profile_sha256,
        &client_jar,
        libraries_dir,
        &plan,
        processors,
        processor_libraries,
    )?;
    cache.write_generation_manifest(&manifest)?;
    tracing::info!(
        target: "launcher",
        "NeoForge processor outputs verified and published ({} outputs)",
        plan.len()
    );
    Ok(())
}

fn read_install_profile_string(installer_path: &Path) -> Result<String> {
    let bytes = std::fs::read(installer_path)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to read installer JAR: {e}")))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid installer JAR: {e}")))?;
    super::forge::read_entry_from_jar(&mut archive, "install_profile.json").ok_or_else(|| {
        LauncherError::ModLoader("NeoForge installer JAR missing install_profile.json".into())
    })
}

/// Install NeoForge — download all libraries, run the installer processors
/// to generate the client JAR, and return the launch profile.
///
/// NeoForge 20.2+ (MC 1.20.5+) does NOT publish a pre-built client JAR on
/// Maven; it must be generated by running the installer's processor
/// pipeline (`jarsplitter` → `AutoRenamingTool` → `binarypatcher`) the same
/// way the official installer and Prism Launcher's ForgeWrapper do.
pub async fn install(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
    app: Option<&tauri::AppHandle>,
) -> Result<LoaderProfile> {
    tracing::info!(target: "launcher", "Installing NeoForge for MC {} (loader {})", mc_version, loader_version);

    let (_, maven_version) = neoforge_maven(mc_version, loader_version);
    let data_dir = libraries_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let cache = ProcessorCache::new(&data_dir, "neoforge", mc_version, &maven_version)?;
    {
        // This early check runs before the installer download. It is not a
        // substitute for the guard held by `ensure_processor_jars_inner`;
        // acquire the same cache lock so recovery cannot overlap publication.
        let _cache_lock = cache.acquire_operation_lock().await;
        cache.recover_pending_transaction(libraries_dir)?;
    }

    // Download installer (kept in the persistent cache for the processor pipeline)
    let (raw_profile, _installer_path) =
        download_installer(mc_version, loader_version, &cache).await?;
    let mut profile = build_profile(raw_profile)?;
    super::artifact::ensure_libraries(
        &mut profile,
        libraries_dir,
        super::artifact::NEOFORGE_HOSTS,
        "NeoForge",
    )
    .await?;

    ensure_processor_jars_inner(mc_version, loader_version, libraries_dir, versions_dir, app)
        .await?;

    // NOTE: the processor-generated client JAR (neoforge-<ver>-client.jar)
    // must NOT go on the classpath. The official version.json does not list
    // it, and FML's ProductionClientProvider resolves it itself via
    // LibraryFinder (coordinate net.neoforged:neoforge:<ver>:client) and
    // merges it into the "minecraft" module. Putting it on the classpath
    // makes bootstraplauncher create a second module named "neoforge"
    // (from the JAR file name), which aborts JPMS resolution with
    // "Module neoforge reads another module named neoforge".

    tracing::info!(target: "launcher", "NeoForge install completed for MC {}", mc_version);
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_neoforge_test_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A structurally valid JAR (see `forge::is_usable_jar`).
    fn write_jar(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        zw.start_file(
            "META-INF/MANIFEST.MF",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zw.write_all(b"Manifest-Version: 1.0\n").unwrap();
        zw.start_file("padding.bin", zip::write::SimpleFileOptions::default())
            .unwrap();
        // High-entropy padding keeps the archive above the 1 KiB floor even
        // when the writer compresses it.
        let padding: Vec<u8> = (0..8192u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        zw.write_all(&padding).unwrap();
        zw.finish().unwrap();
    }

    fn profile_with_patched(value: &str) -> serde_json::Value {
        serde_json::json!({
            "data": {
                "PATCHED": { "client": value, "server": value },
                "BINPATCH": { "client": "/data/client.lzma" }
            }
        })
    }

    /// NeoForge 1.20.1 (the `net.neoforged:forge` fork) must use the MC-prefixed
    /// gradle-style version under the forge group.
    #[test]
    fn build_profile_carries_typed_checksum_provenance() {
        let profile = NeoForgeInstallProfile {
            id: "test".to_string(),
            main_class: "test.Main".to_string(),
            libraries: vec![NeoForgeLibrary {
                name: "net.neoforged:example:1.0".to_string(),
                url: None,
                downloads: Some(NeoForgeDownloads {
                    artifact: Some(NeoForgeArtifact {
                        path: Some("net/neoforged/example/1.0/example-1.0.jar".to_string()),
                        url: Some(
                            "https://maven.neoforged.net/releases/net/neoforged/example/1.0/example-1.0.jar"
                                .to_string(),
                        ),
                        sha1: Some("a".repeat(40)),
                        sha256: Some("b".repeat(64)),
                        sha512: Some("c".repeat(128)),
                        size: Some(7),
                    }),
                }),
            }],
            arguments: None,
            minecraft_arguments: None,
        };
        let built = build_profile(profile).unwrap();
        let lib = &built.libraries[0];
        assert_eq!(lib.checksum_source.as_deref(), Some("metadata"));
        assert_eq!(lib.checksum().unwrap().unwrap().algorithm(), "sha512");
        assert_eq!(lib.size, Some(7));
    }

    #[test]
    fn neoforge_maven_uses_forge_fork_for_1_20_1() {
        let (group, version) = neoforge_maven("1.20.1", "47.1.106");
        assert_eq!(group, "net/neoforged/forge");
        assert_eq!(version, "1.20.1-47.1.106");
    }

    /// Modern NeoForge (renumbered, e.g. 1.20.2+ / 20.x line) uses its own
    /// `neoforge` group with the bare loader version.
    #[test]
    fn neoforge_maven_uses_neoforge_group_for_modern_versions() {
        let (group, version) = neoforge_maven("1.20.4", "20.4.251");
        assert_eq!(group, "net/neoforged/neoforge");
        assert_eq!(version, "20.4.251");
    }

    /// 1.21.4 (the currently targeted modern release) also uses the bare version.
    #[test]
    fn neoforge_maven_uses_neoforge_group_for_1_21_4() {
        let (group, version) = neoforge_maven("1.21.4", "21.4.157");
        assert_eq!(group, "net/neoforged/neoforge");
        assert_eq!(version, "21.4.157");
    }

    /// Other old-generation forks around 1.20.1 (e.g. 1.18/1.19 Forge-compat)
    /// are not part of NeoForge — only 1.20.1 gates on the forge fork.
    #[test]
    fn neoforge_maven_1_20_2_uses_neoforge_group() {
        let (group, version) = neoforge_maven("1.20.2", "20.2.93");
        assert_eq!(group, "net/neoforged/neoforge");
        assert_eq!(version, "20.2.93");
    }

    #[test]
    fn patched_client_path_follows_modern_installer_metadata() {
        let profile = profile_with_patched("[net.neoforged:minecraft-client-patched:26.3.0.99]");
        let rel = patched_client_rel_path(&profile).expect("modern coordinate must parse");
        assert_eq!(
            rel,
            PathBuf::from(
                "net/neoforged/minecraft-client-patched/26.3.0.99/minecraft-client-patched-26.3.0.99.jar"
            )
        );
    }

    #[test]
    fn patched_client_path_follows_legacy_installer_metadata() {
        // NeoForge 21.x (and the 1.20.1 forge fork) write the loader's own
        // `:client` artifact; that must still be recognized.
        let modern = profile_with_patched("[net.neoforged:neoforge:21.4.157:client]");
        assert_eq!(
            patched_client_rel_path(&modern).unwrap(),
            PathBuf::from("net/neoforged/neoforge/21.4.157/neoforge-21.4.157-client.jar")
        );

        let fork = profile_with_patched("[net.neoforged:forge:1.20.1-47.1.106:client]");
        assert_eq!(
            patched_client_rel_path(&fork).unwrap(),
            PathBuf::from("net/neoforged/forge/1.20.1-47.1.106/forge-1.20.1-47.1.106-client.jar")
        );
    }

    #[test]
    fn patched_client_path_rejects_unsafe_or_missing_metadata() {
        for raw in [
            "../escape:1.0",
            "[net.neoforged:neoforge:21.4.157:client@jar]",
            "[net.neoforged/../../neoforge:21.4.157:client]",
            "net.neoforged:neoforge",
            "{PATCHED}",
        ] {
            assert!(
                patched_client_rel_path(&profile_with_patched(raw)).is_none(),
                "coordinate {raw:?} must be rejected"
            );
        }
        let empty = serde_json::json!({ "data": {} });
        assert!(patched_client_rel_path(&empty).is_none());
    }

    #[test]
    fn expected_client_jar_paths_cover_both_layouts_without_metadata() {
        let dir = temp_dir("expected_paths");
        let paths = expected_client_jar_paths(&dir, "26.3", "26.3.0.99", None);
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            dir.join(
                "net/neoforged/minecraft-client-patched/26.3.0.99/minecraft-client-patched-26.3.0.99.jar"
            )
        );
        assert_eq!(
            paths[1],
            dir.join("net/neoforged/neoforge/26.3.0.99/neoforge-26.3.0.99-client.jar")
        );

        // Metadata is authoritative: only it and the legacy fallback remain.
        let profile = profile_with_patched("[net.neoforged:minecraft-client-patched:26.3.0.99]");
        let paths = expected_client_jar_paths(&dir, "26.3", "26.3.0.99", Some(&profile));
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            dir.join(
                "net/neoforged/minecraft-client-patched/26.3.0.99/minecraft-client-patched-26.3.0.99.jar"
            )
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_client_jar_accepts_the_metadata_path_and_skips_invalid_files() {
        let dir = temp_dir("find_jar");
        let profile = profile_with_patched("[net.neoforged:minecraft-client-patched:26.3.0.99]");
        let paths = expected_client_jar_paths(&dir, "26.3", "26.3.0.99", Some(&profile));

        // A tiny/truncated file is not an install.
        std::fs::create_dir_all(paths[1].parent().unwrap()).unwrap();
        std::fs::write(&paths[1], b"not a zip").unwrap();
        assert!(find_client_jar(&paths).is_none());

        write_jar(&paths[0]);
        assert_eq!(find_client_jar(&paths).as_deref(), Some(paths[0].as_path()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    use crate::download::tests::{direct_proxy_lock, sha1_hex, spawn_mock, MockSpec};

    fn mock_profile_base(server: &crate::download::tests::MockServer) -> String {
        server.url.trim_end_matches("artifact.bin").to_string()
    }

    fn profile_body() -> &'static [u8] {
        br#"{"id":"test","mainClass":"test.Main","libraries":[]}"#
    }

    fn checksum_specs(body: &[u8]) -> Vec<MockSpec> {
        vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::ok(sha1_hex(body).as_bytes()),
        ]
    }

    async fn spawn_hanging_profile_server(body: &'static [u8]) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hash = sha1_hex(body);
        tokio::spawn(async move {
            for index in 0..4 {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                if index < 3 {
                    let (head, body) = if index < 2 {
                        ("HTTP/1.1 404 Not Found".to_string(), Vec::new())
                    } else {
                        ("HTTP/1.1 200 OK".to_string(), hash.as_bytes().to_vec())
                    };
                    let response = format!(
                        "{head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                } else {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        });
        format!("http://{addr}/profile.json")
    }

    #[tokio::test]
    async fn standalone_profile_requires_a_sidecar_and_uses_exact_host() {
        let _proxy = direct_proxy_lock().await;
        let body = profile_body();
        let mut specs = checksum_specs(body);
        specs.push(MockSpec::ok(body));
        let server = spawn_mock(specs).await;
        let url = format!("{}profile.json", mock_profile_base(&server));
        let profile = fetch_standalone_profile(&url).await.unwrap().unwrap();
        assert_eq!(profile.main_class, "test.Main");
    }

    #[tokio::test]
    async fn standalone_profile_rejects_cross_host_and_http_redirects() {
        let _proxy = direct_proxy_lock().await;
        let body = profile_body();
        for target in [
            "https://evil.example/profile.json".to_string(),
            "http://example.invalid/profile.json".to_string(),
        ] {
            let mut specs = checksum_specs(body);
            specs.push(MockSpec::redirect(&target));
            let server = spawn_mock(specs).await;
            let url = format!("{}profile.json", mock_profile_base(&server));
            assert!(
                fetch_standalone_profile(&url).await.unwrap().is_none(),
                "redirect to {target} must not produce an accepted profile"
            );
        }
    }

    #[tokio::test]
    async fn standalone_profile_uses_the_https_exact_host_redirect_policy() {
        let _proxy = direct_proxy_lock().await;
        // The production policy accepts a same-host HTTPS redirect.
        assert!(crate::download::same_host_redirect_allowed(
            &reqwest::Url::parse("https://maven.neoforged.net/releases/profile.json").unwrap(),
            "maven.neoforged.net",
            0,
        ));
        // The mock server is HTTP, so the exact-host policy must stop even a
        // same-host redirect instead of weakening the scheme check.
        let body = profile_body();
        let server = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::ok(sha1_hex(body).as_bytes()),
            MockSpec::redirect("/final.json"),
            MockSpec::ok(body),
        ])
        .await;
        let url = format!("{}profile.json", mock_profile_base(&server));
        assert!(fetch_standalone_profile(&url).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn standalone_profile_rejects_malformed_json_and_absent_checksum() {
        let _proxy = direct_proxy_lock().await;
        let body = b"not-json";
        let server = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::ok(sha1_hex(body).as_bytes()),
            MockSpec::ok(body),
        ])
        .await;
        let url = format!("{}profile.json", mock_profile_base(&server));
        assert!(fetch_standalone_profile(&url).await.is_err());

        let server = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::status(404),
        ])
        .await;
        let url = format!("{}profile.json", mock_profile_base(&server));
        assert!(fetch_standalone_profile(&url).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn standalone_profile_timeout_falls_back_to_the_installer() {
        let _proxy = direct_proxy_lock().await;
        let url = spawn_hanging_profile_server(profile_body()).await;
        assert!(fetch_standalone_profile(&url).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn standalone_profile_404_5xx_and_network_failure_fall_back() {
        let _proxy = direct_proxy_lock().await;
        let body = profile_body();
        for status in [404u16, 503u16] {
            let mut specs = checksum_specs(body);
            specs.push(MockSpec::status(status));
            let server = spawn_mock(specs).await;
            let url = format!("{}profile.json", mock_profile_base(&server));
            assert!(fetch_standalone_profile(&url).await.unwrap().is_none());
        }
        assert!(fetch_standalone_profile("http://127.0.0.1:9/profile.json")
            .await
            .unwrap()
            .is_none());
    }
}

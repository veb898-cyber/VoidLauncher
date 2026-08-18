use serde::Deserialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use crate::error::{LauncherError, Result};
use crate::modloaders::{LoaderLibrary, LoaderProfile, LoaderVersionPage};
use crate::versions::maven_to_path;

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
static SUPPORTED_VERSIONS_CACHE: OnceLock<Mutex<Option<(Instant, Vec<String>)>>> = OnceLock::new();
const SUPPORTED_CACHE_TTL: Duration = Duration::from_secs(600);

/// Minecraft versions officially supported by Fabric (`/v2/versions/game`).
/// Fabric supports 1.14+ only — versions like 1.13 are rejected by the API.
pub async fn supported_versions() -> Result<Vec<String>> {
    {
        let cache = SUPPORTED_VERSIONS_CACHE.get_or_init(|| Mutex::new(None));
        let guard = cache.lock().map_err(|e| LauncherError::ModLoader(e.to_string()))?;
        if let Some((fetched_at, versions)) = guard.as_ref() {
            if fetched_at.elapsed() < SUPPORTED_CACHE_TTL {
                return Ok(versions.clone());
            }
        }
    }
    let client = crate::download::global_http_client();
    let versions: Vec<FabricGameVersion> = client
        .get("https://meta.fabricmc.net/v2/versions/game")
        .send()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to fetch Fabric game versions: {}", e)))?
        .json()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse Fabric game versions: {}", e)))?;
    let list: Vec<String> = versions.into_iter().map(|v| v.version).collect();
    let cache = SUPPORTED_VERSIONS_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().map_err(|e| LauncherError::ModLoader(e.to_string()))?;
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
        return Ok(LoaderVersionPage { versions: Vec::new(), total: 0 });
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
        let guard = cache.lock().map_err(|e| LauncherError::ModLoader(e.to_string()))?;
        if guard.is_some() {
            let all = guard.as_ref().unwrap();
            let total = all.len();
            let page: Vec<_> = all.iter().skip(offset).take(limit).map(|v| {
                super::LoaderVersion {
                    version: v.version.clone(),
                    stable: v.stable,
                }
            }).collect();
            return Ok(LoaderVersionPage { versions: page, total });
        }
    }

    // Fetch from Fabric official API
    let client = crate::download::global_http_client();
    let versions: Vec<FabricLoaderVersion> = client
        .get("https://meta.fabricmc.net/v2/versions/loader")
        .send()
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
    let page: Vec<_> = sorted.iter().skip(offset).take(limit).map(|v| {
        super::LoaderVersion {
            version: v.version.clone(),
            stable: v.stable,
        }
    }).collect();

    // Cache the full sorted list
    {
        let mut guard = cache.lock().map_err(|e| LauncherError::ModLoader(e.to_string()))?;
        *guard = Some(sorted);
    }

    Ok(LoaderVersionPage { versions: page, total })
}

/// Fetch game versions supported by Fabric
#[allow(dead_code)]
pub async fn get_game_versions() -> Result<Vec<String>> {
    let client = crate::download::global_http_client();
    let versions: Vec<FabricGameVersion> = client
        .get("https://meta.fabricmc.net/v2/versions/game")
        .send()
        .await?
        .json()
        .await?;

    Ok(versions.into_iter().map(|v| v.version).collect())
}

/// Get Fabric profile for a specific MC version + loader version
pub async fn get_profile(mc_version: &str, loader_version: &str) -> Result<LoaderProfile> {
    if !is_supported(mc_version).await? {
        return Err(LauncherError::ModLoader(format!(
            "Fabric does not support Minecraft {} (supported: 1.14 and newer)",
            mc_version
        )));
    }
    let client = crate::download::global_http_client();
    let url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
        mc_version, loader_version
    );

    let profile: FabricProfile = client
        .get(&url)
        .send()
        .await
        .map_err(|e| {
            LauncherError::ModLoader(format!("Failed to fetch Fabric profile: {}", e))
        })?
        .json()
        .await
        .map_err(|e| {
            LauncherError::ModLoader(format!("Failed to parse Fabric profile: {}", e))
        })?;

    let libraries = profile
        .libraries
        .into_iter()
        .map(|lib| {
            let path = maven_to_path(&lib.name);
            LoaderLibrary {
                name: lib.name,
                url: lib.url,
                path,
                sha1: None,
                size: None,
            }
        })
        .collect();

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

/// Install Fabric for an instance
pub async fn install(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &std::path::Path,
) -> Result<LoaderProfile> {
    tracing::info!(target: "launcher", "Installing Fabric for MC {} (loader {})", mc_version, loader_version);
    let profile = get_profile(mc_version, loader_version).await?;

    for lib in &profile.libraries {
        let lib_path = libraries_dir.join(&lib.path);
        if !lib_path.exists() {
            let url = format!("{}{}", lib.url, lib.path);
            if let Err(e) = crate::download::download_file(&url, &lib_path, "").await {
                tracing::warn!(target: "launcher", "Failed to download Fabric library {}: {}", lib.name, e);
                return Err(e);
            }
        }
    }

    tracing::info!(target: "launcher", "Fabric install completed for MC {}", mc_version);
    Ok(profile)
}

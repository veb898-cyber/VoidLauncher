use serde::Deserialize;
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
/// We delegate to the Prism metadata mirror (`prism_meta`) rather than
/// hitting `meta.fabricmc.net` directly: the mirror is faster, more
/// reliable, and returns the same data in a uniform shape. Fabric
/// Loader versions are universal across MC versions (they only pin
/// `net.fabricmc.intermediary`, not a specific MC), so we pass `None`
/// for the MC filter.
///
/// The wizard drives this with infinite scroll: it asks for
/// `PAGE_SIZE` items, appends them, and asks for the next page at
/// `accumulator.length`. The underlying `prism_meta` call caches
/// the parsed 251-entry index on first request so every page
/// after the first is instant.
///
/// On fetch/parse failure the underlying `prism_meta` call logs the
/// error and returns an empty page with `total = 0` — see
/// `prism_meta::fetch_loader_versions` for details.
pub async fn get_loader_versions(offset: usize, limit: usize) -> Result<LoaderVersionPage> {
    super::prism_meta::fetch_loader_versions("net.fabricmc.fabric-loader", None, offset, limit).await
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

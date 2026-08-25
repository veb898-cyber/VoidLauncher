#[allow(dead_code)]
pub mod fabric;
pub mod forge;
pub mod neoforge;
mod prism_meta;

use serde::{Deserialize, Serialize};
use crate::error::Result;
use std::path::Path;

/// Information about a mod loader version
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoaderVersion {
    pub version: String,
    pub stable: bool,
}

/// A page of loader versions plus the total count of matching
/// versions across all pages. The wizard uses this for infinite
/// scroll: it shows `versions` immediately, then asks for the next
/// `PAGE_SIZE` items starting at `versions.length` until the
/// accumulator reaches `total`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoaderVersionPage {
    pub versions: Vec<LoaderVersion>,
    pub total: usize,
}

/// Profile data that modifies the launch configuration
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoaderProfile {
    /// Main class override
    pub main_class: String,
    /// Additional libraries to add to classpath
    pub libraries: Vec<LoaderLibrary>,
    /// Additional JVM arguments
    pub jvm_args: Vec<String>,
    /// Additional game arguments
    pub game_args: Vec<String>,
    /// True when game_args came from the legacy `minecraftArguments` string
    /// (MC <= 1.12.2 style) and therefore REPLACE the vanilla game
    /// arguments instead of being appended to them.
    #[serde(default)]
    pub legacy_args: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoaderLibrary {
    pub name: String,
    pub url: String,
    pub path: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

/// Get the loader profile for a given loader type
#[allow(dead_code)]
pub async fn get_profile(
    loader: &str,
    mc_version: &str,
    loader_version: &str,
) -> Result<LoaderProfile> {
    match loader {
        "Fabric" => fabric::get_profile(mc_version, loader_version).await,
        "Forge" => forge::get_profile(mc_version, loader_version).await,
        "NeoForge" => neoforge::get_profile(mc_version, loader_version).await,
        _ => Err(crate::error::LauncherError::ModLoader(format!(
            "Unknown loader: {}",
            loader
        ))),
    }
}

/// Install a mod loader and return its profile
#[allow(dead_code)]
pub async fn install_loader(
    loader: &str,
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
    app: Option<&tauri::AppHandle>,
) -> Result<LoaderProfile> {
    // Forge/NeoForge need the vanilla client JAR on disk to patch it into
    // the loader version. Download it first when it's missing, so installing
    // a loader works even right after a pack import (before the first launch).
    if loader == "Forge" || loader == "NeoForge" {
        ensure_client_jar(mc_version, libraries_dir, versions_dir).await?;
    }
    match loader {
        "Fabric" => fabric::install(mc_version, loader_version, libraries_dir).await,
        "Forge" => forge::install(mc_version, loader_version, libraries_dir, versions_dir).await,
        "NeoForge" => neoforge::install(mc_version, loader_version, libraries_dir, versions_dir, app).await,
        _ => Err(crate::error::LauncherError::ModLoader(format!(
            "Unknown loader: {}",
            loader
        ))),
    }
}

/// Download the vanilla client JAR (and its libraries) for `mc_version` so a
/// subsequent Forge/NeoForge install has the file it needs to patch. The JAR
/// is normally fetched at first launch; importing a pack must not depend on
/// that, and neither should an auto-loader-install at launch.
async fn ensure_client_jar(
    mc_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
) -> Result<()> {
    let vd = versions_dir.join(mc_version);
    if vd.join("client.jar").exists() || vd.join(format!("{}.jar", mc_version)).exists() {
        return Ok(());
    }
    tracing::info!(
        target: "launcher",
        "Vanilla client for {} not on disk — downloading before loader install...",
        mc_version
    );
    let manifest = crate::versions::fetch_version_manifest().await?;
    let url = manifest
        .versions
        .iter()
        .find(|v| v.id == mc_version)
        .map(|v| v.url.clone())
        .ok_or_else(|| {
            crate::error::LauncherError::ModLoader(format!(
                "Version {} not found in manifest",
                mc_version
            ))
        })?;
    let info = crate::versions::fetch_version_info(&url).await?;
    let libraries_dir_buf = libraries_dir.to_path_buf();
    let versions_dir_buf = versions_dir.to_path_buf();
    let files = crate::versions::collect_downloads(&info, &libraries_dir_buf, &versions_dir_buf);
    let missing: Vec<_> = files
        .into_iter()
        .filter(|(_, path, _, _)| !path.exists())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    crate::download::download_files(missing, |completed, total, _bytes_done, _bytes_total, _msg| {
        tracing::info!(
            target: "launcher",
            "Vanilla client download {}/{}",
            completed,
            total
        );
    })
    .await?;
    Ok(())
}

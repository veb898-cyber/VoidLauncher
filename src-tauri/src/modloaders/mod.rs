#[allow(dead_code)]
pub mod fabric;
#[allow(dead_code)]
pub mod quilt;
pub mod forge;
pub mod neoforge;

use serde::{Deserialize, Serialize};
use crate::error::Result;
use std::path::Path;

/// Information about a mod loader version
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoaderVersion {
    pub version: String,
    pub stable: bool,
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
        "Quilt" => quilt::get_profile(mc_version, loader_version).await,
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
) -> Result<LoaderProfile> {
    match loader {
        "Fabric" => fabric::install(mc_version, loader_version, libraries_dir).await,
        "Quilt" => quilt::install(mc_version, loader_version, libraries_dir).await,
        "Forge" => forge::install(mc_version, loader_version, libraries_dir).await,
        "NeoForge" => neoforge::install(mc_version, loader_version, libraries_dir).await,
        _ => Err(crate::error::LauncherError::ModLoader(format!(
            "Unknown loader: {}",
            loader
        ))),
    }
}

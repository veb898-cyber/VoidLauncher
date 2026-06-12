use serde::Deserialize;
use crate::error::{LauncherError, Result};
use crate::versions::maven_to_path;
use super::{LoaderVersionPage, LoaderProfile, LoaderLibrary};
use std::path::Path;

/// NeoForge install profile (same structure as Minecraft version JSON)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct NeoForgeInstallProfile {
    id: String,
    #[serde(rename = "mainClass")]
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
/// On fetch/parse failure the underlying `prism_meta` call logs the
/// error and returns an empty page with `total = 0`.
pub async fn get_loader_versions(
    mc_version: &str,
    offset: usize,
    limit: usize,
) -> Result<LoaderVersionPage> {
    super::prism_meta::fetch_loader_versions("net.neoforged", Some(mc_version), offset, limit).await
}

/// Get NeoForge launch profile
pub async fn get_profile(_mc_version: &str, neo_version: &str) -> Result<LoaderProfile> {
    let client = crate::download::global_http_client();
    let profile_url = format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{}/neoforge-{}.json",
        neo_version, neo_version
    );

    let profile: NeoForgeInstallProfile = client
        .get(&profile_url)
        .send()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to fetch NeoForge profile: {}", e)))?
        .json()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse NeoForge profile: {}", e)))?;

    let mut libraries = Vec::new();
    for lib in &profile.libraries {
        let (url, path) = if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let lib_path = artifact.path.clone().unwrap_or_else(|| maven_to_path(&lib.name));
                let lib_url = artifact.url.clone().unwrap_or_else(|| {
                    format!("https://maven.neoforged.net/{}", lib_path)
                });
                (lib_url, lib_path)
            } else {
                let path = maven_to_path(&lib.name);
                let base = lib.url.clone().unwrap_or_else(|| "https://maven.neoforged.net/".into());
                let base = if base.ends_with('/') { base } else { format!("{}/", base) };
                (format!("{}{}", base, path), path)
            }
        } else {
            let path = maven_to_path(&lib.name);
            let base = lib.url.clone().unwrap_or_else(|| "https://maven.neoforged.net/".into());
            let base = if base.ends_with('/') { base } else { format!("{}/", base) };
            (format!("{}{}", base, path), path)
        };

        libraries.push(LoaderLibrary {
            name: lib.name.clone(),
            url,
            path,
            sha1: None,
            size: None,
        });
    }

    let (game_args, jvm_args) = if let Some(args) = &profile.arguments {
        let game = args.game.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let jvm = args.jvm.iter()
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
    })
}

/// Install NeoForge — download all libraries and return profile
pub async fn install(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
) -> Result<LoaderProfile> {
    tracing::info!(target: "launcher", "Installing NeoForge for MC {} (loader {})", mc_version, loader_version);
    let profile = get_profile(mc_version, loader_version).await?;

    for lib in &profile.libraries {
        let lib_path = libraries_dir.join(&lib.path);
        if !lib_path.exists() {
            if let Err(e) = crate::download::download_file(&lib.url, &lib_path, "").await {
                tracing::warn!(target: "launcher", "Failed to download NeoForge library {}: {}", lib.name, e);
                return Err(e);
            }
        }
    }

    tracing::info!(target: "launcher", "NeoForge install completed for MC {}", mc_version);
    Ok(profile)
}

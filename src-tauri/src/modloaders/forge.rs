use serde::Deserialize;
use crate::error::{LauncherError, Result};
use crate::versions::maven_to_path;
use super::{LoaderVersionPage, LoaderProfile, LoaderLibrary};
use std::path::Path;

/// Forge installer profile (same structure as a Minecraft version JSON)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeInstallProfile {
    id: String,
    #[serde(rename = "mainClass")]
    main_class: String,
    #[serde(default)]
    libraries: Vec<ForgeLibrary>,
    #[serde(default)]
    arguments: Option<ForgeArguments>,
    #[serde(rename = "minecraftArguments", default)]
    minecraft_arguments: Option<String>,
    #[serde(default)]
    inherits_from: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeLibrary {
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    downloads: Option<ForgeDownloads>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeDownloads {
    artifact: Option<ForgeArtifact>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeArtifact {
    path: Option<String>,
    url: Option<String>,
    sha1: Option<String>,
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeArguments {
    #[serde(default)]
    game: Vec<serde_json::Value>,
    #[serde(default)]
    jvm: Vec<serde_json::Value>,
}

/// Fetch a page of available Forge versions for a MC version.
///
/// We delegate to the Prism metadata mirror (`prism_meta`) rather than
/// parsing `files.minecraftforge.net/.../promotions_slim.json`. The
/// `promotions_slim.json` API only contains the *recommended* and
/// *latest* Forge builds per MC version — to get the full list of
/// every Forge release for a given MC version, you'd have to do a
/// second request to `maven-metadata.xml`. The Prism mirror already
/// has the complete, curated list in one fetch and includes the
/// `requires.net.minecraft` constraint we need to filter by MC
/// version.
///
/// The wizard drives this with infinite scroll: it asks for
/// `PAGE_SIZE` items, appends them, and asks for the next page at
/// `accumulator.length`. The underlying `prism_meta` call caches
/// the parsed 4968-entry index on first request so every page
/// after the first is instant (no re-download of the 1.96 MB
/// file).
///
/// On fetch/parse failure the underlying `prism_meta` call logs the
/// error and returns an empty page with `total = 0`.
pub async fn get_loader_versions(
    mc_version: &str,
    offset: usize,
    limit: usize,
) -> Result<LoaderVersionPage> {
    super::prism_meta::fetch_loader_versions("net.minecraftforge", Some(mc_version), offset, limit).await
}

/// Get Forge launch profile
pub async fn get_profile(mc_version: &str, forge_version: &str) -> Result<LoaderProfile> {
    let client = crate::download::global_http_client();
    let full_version = if forge_version.contains('-') {
        forge_version.to_string()
    } else {
        format!("{}-{}", mc_version, forge_version)
    };

    let profile_url = format!(
        "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}.json",
        full_version, full_version
    );

    let profile: ForgeInstallProfile = client
        .get(&profile_url)
        .send()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to fetch Forge profile: {}", e)))?
        .json()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse Forge profile: {}", e)))?;

    let mut libraries = Vec::new();
    for lib in &profile.libraries {
        let (url, path) = if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let lib_path = artifact.path.clone().unwrap_or_else(|| maven_to_path(&lib.name));
                let lib_url = artifact.url.clone().unwrap_or_else(|| {
                    format!("https://maven.minecraftforge.net/{}", lib_path)
                });
                (lib_url, lib_path)
            } else {
                // Fallback to Maven pattern
                let path = maven_to_path(&lib.name);
                let base = lib.url.clone().unwrap_or_else(|| "https://maven.minecraftforge.net/".into());
                let base = if base.ends_with('/') { base } else { format!("{}/", base) };
                (format!("{}{}", base, path), path)
            }
        } else {
            let path = maven_to_path(&lib.name);
            let base = lib.url.clone().unwrap_or_else(|| "https://maven.minecraftforge.net/".into());
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

    // Extract JVM and game args
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

/// Install Forge — download all libraries and return profile
pub async fn install(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
) -> Result<LoaderProfile> {
    tracing::info!(target: "launcher", "Installing Forge for MC {} (loader {})", mc_version, loader_version);
    let profile = get_profile(mc_version, loader_version).await?;

    for lib in &profile.libraries {
        let lib_path = libraries_dir.join(&lib.path);
        if !lib_path.exists() {
            if let Err(e) = crate::download::download_file(&lib.url, &lib_path, "").await {
                tracing::warn!(target: "launcher", "Failed to download Forge library {}: {}", lib.name, e);
                return Err(e);
            }
        }
    }

    tracing::info!(target: "launcher", "Forge install completed for MC {}", mc_version);
    Ok(profile)
}

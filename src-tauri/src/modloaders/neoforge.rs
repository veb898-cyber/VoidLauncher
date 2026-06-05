use serde::Deserialize;
use crate::error::{LauncherError, Result};
use crate::versions::maven_to_path;
use super::{LoaderVersion, LoaderProfile, LoaderLibrary};
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

/// Fetch available NeoForge versions for a MC version
pub async fn get_loader_versions(mc_version: &str) -> Result<Vec<LoaderVersion>> {
    let client = crate::download::global_http_client();
    let url = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to fetch NeoForge versions: {}", e)))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse NeoForge versions: {}", e)))?;

    let mut versions = Vec::new();
    if let Some(version_list) = resp.get("versions").and_then(|v| v.as_array()) {
        // NeoForge versions map to MC versions differently
        // MC 1.21 → NeoForge 21.x, MC 1.21.1 → NeoForge 21.1.x, etc.
        let mc_parts: Vec<&str> = mc_version.split('.').collect();
        let neo_prefix = if mc_parts.len() >= 2 && mc_parts[0] == "1" {
            mc_parts[1..].join(".")
        } else {
            mc_version.to_string()
        };

        for ver in version_list {
            if let Some(v) = ver.as_str() {
                if v.starts_with(&neo_prefix) && !v.contains('-') {
                    versions.push(LoaderVersion {
                        version: v.to_string(),
                        stable: true,
                    });
                }
            }
        }
    }

    versions.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(versions)
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
    let profile = get_profile(mc_version, loader_version).await?;

    for lib in &profile.libraries {
        let lib_path = libraries_dir.join(&lib.path);
        if !lib_path.exists() {
            crate::download::download_file(&lib.url, &lib_path, "").await?;
        }
    }

    Ok(profile)
}

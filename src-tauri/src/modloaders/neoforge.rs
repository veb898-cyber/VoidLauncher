use serde::Deserialize;
use crate::error::{LauncherError, Result};
use crate::versions::maven_to_path;
use super::{LoaderVersionPage, LoaderProfile, LoaderLibrary};
use std::io::Read;
use std::path::Path;
use tauri::Emitter;

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

/// Get NeoForge launch profile.
///
/// For NeoForge 21.x+ (MC 1.20.5+), the install profile JSON is no longer
/// published as a standalone `.json` on Maven. It lives inside the installer
/// JAR (`neoforge-{version}-installer.jar`) as `install_profile.json`.
///
/// We first try the legacy standalone JSON (works for older NeoForge),
/// then fall back to extracting from the installer JAR.
pub async fn get_profile(_mc_version: &str, neo_version: &str) -> Result<LoaderProfile> {
    let client = crate::download::global_http_client();

    // Try legacy standalone JSON first
    let profile_url = format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{}/neoforge-{}.json",
        neo_version, neo_version
    );

    if let Ok(resp) = client.get(&profile_url).send().await {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                if let Ok(profile) = serde_json::from_str::<NeoForgeInstallProfile>(&text) {
                    return build_profile(profile);
                }
            }
        }
    }

    // Fallback: download installer JAR and extract install_profile.json
    let installer_url = format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{}/neoforge-{}-installer.jar",
        neo_version, neo_version
    );

    tracing::info!(target: "launcher", "Fetching NeoForge installer JAR: {}", installer_url);

    // Use download_file with retries for reliable download
    let installer_path = std::env::temp_dir().join(format!("neoforge-{}-installer.jar", neo_version));
    let mut last_err = String::new();
    for attempt in 1..=3u32 {
        match crate::download::download_file(&installer_url, &installer_path, "").await {
            Ok(()) => break,
            Err(e) => {
                last_err = e.to_string();
                tracing::warn!(target: "launcher", "NeoForge installer download attempt {}/3 failed: {}", attempt, last_err);
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_secs(3 * attempt as u64)).await;
                }
            }
        }
    }

    if !installer_path.exists() {
        return Err(LauncherError::ModLoader(format!(
            "Failed to download NeoForge installer after 3 attempts: {}", last_err
        )));
    }

    let jar_bytes = std::fs::read(&installer_path)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to read NeoForge installer JAR: {}", e)))?;
    let _ = std::fs::remove_file(&installer_path);

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid NeoForge installer JAR: {}", e)))?;

    let mut install_profile_str = String::new();
    let mut version_json_str = String::new();
    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            if entry.name() == "install_profile.json" {
                entry.read_to_string(&mut install_profile_str)
                    .map_err(|e| LauncherError::ModLoader(format!("Failed to read install_profile.json: {}", e)))?;
            } else if entry.name() == "version.json" {
                entry.read_to_string(&mut version_json_str)
                    .map_err(|e| LauncherError::ModLoader(format!("Failed to read version.json: {}", e)))?;
            }
        }
    }

    if install_profile_str.is_empty() {
        return Err(LauncherError::ModLoader("NeoForge installer JAR missing install_profile.json".into()));
    }

    // NeoForge installer JAR structure:
    //   install_profile.json — installer metadata, points to the actual version JSON via "json" field
    //   version.json — the real version profile with mainClass, libraries, arguments
    // The install_profile.json has "json": "/version.json" pointing to the version profile.
    // We read version.json from the JAR as the actual profile.

    let profile: NeoForgeInstallProfile = if !version_json_str.is_empty() {
        // version.json is the actual version profile (same format as Minecraft version JSON)
        serde_json::from_str(&version_json_str)
            .map_err(|e| LauncherError::ModLoader(format!("Failed to parse NeoForge version.json: {}", e)))?
    } else {
        // Fallback: try install_profile.json directly
        let parsed: serde_json::Value = serde_json::from_str(&install_profile_str)
            .map_err(|e| LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e)))?;
        if let Some(version_info) = parsed.get("versionInfo") {
            serde_json::from_value(version_info.clone())
                .map_err(|e| LauncherError::ModLoader(format!("Failed to parse NeoForge versionInfo: {}", e)))?
        } else {
            serde_json::from_value(parsed)
                .map_err(|e| LauncherError::ModLoader(format!("Failed to parse NeoForge install profile: {}", e)))?
        }
    };

    build_profile(profile)
}

fn build_profile(profile: NeoForgeInstallProfile) -> Result<LoaderProfile> {
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
    app: Option<&tauri::AppHandle>,
) -> Result<LoaderProfile> {
    tracing::info!(target: "launcher", "Installing NeoForge for MC {} (loader {})", mc_version, loader_version);
    let profile = get_profile(mc_version, loader_version).await?;

    let total = profile.libraries.len();
    for (i, lib) in profile.libraries.iter().enumerate() {
        let lib_path = libraries_dir.join(&lib.path);
        if !lib_path.exists() {
            if let Some(app_handle) = app {
                let _ = app_handle.emit("loader-install-progress", serde_json::json!({
                    "stage": "downloading",
                    "message": format!("Downloading {} ({}/{})", lib.name, i + 1, total),
                }));
            }
            if let Err(e) = crate::download::download_file(&lib.url, &lib_path, "").await {
                tracing::warn!(target: "launcher", "Failed to download NeoForge library {}: {}", lib.name, e);
                return Err(e);
            }
        }
    }

    tracing::info!(target: "launcher", "NeoForge install completed for MC {}", mc_version);
    Ok(profile)
}

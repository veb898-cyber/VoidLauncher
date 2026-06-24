use serde::Deserialize;
use crate::error::{LauncherError, Result};
use crate::versions::maven_to_path;
use super::{LoaderVersionPage, LoaderProfile, LoaderLibrary};
use std::io::Read;
use std::path::Path;

/// Forge installer profile.
///
/// Old format (pre-1.21.5): contains `id`, `mainClass`, `libraries`, `arguments` directly.
/// New format (1.21.5+): processor-based installer without `id`; the actual profile
/// lives in a separate `version.json` file inside the installer JAR.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeInstallProfile {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "mainClass", default)]
    main_class: Option<String>,
    #[serde(default)]
    libraries: Vec<ForgeLibrary>,
    #[serde(default)]
    arguments: Option<ForgeArguments>,
    #[serde(rename = "minecraftArguments", default)]
    minecraft_arguments: Option<String>,
    #[serde(default)]
    inherits_from: Option<String>,
    /// New-format field: path to version.json inside the JAR
    #[serde(default)]
    json: Option<String>,
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

/// Get Forge launch profile.
///
/// For newer Forge versions (MC 1.21.5+), the standalone profile JSON is no
/// longer published on Maven. It lives inside the installer JAR
/// (`forge-{version}-installer.jar`) as `install_profile.json`.
/// We first try the legacy standalone JSON, then fall back to extracting
/// from the installer JAR.
pub async fn get_profile(mc_version: &str, forge_version: &str) -> Result<LoaderProfile> {
    let full_version = if forge_version.contains('-') {
        forge_version.to_string()
    } else {
        format!("{}-{}", mc_version, forge_version)
    };

    // Try standalone JSON first (works for older Forge versions)
    // Try multiple URL patterns similar to installer
    let json_urls = vec![
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}.json",
            full_version, full_version
        ),
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}.json",
            full_version, forge_version
        ),
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}.json",
            forge_version, forge_version
        ),
    ];
    let client = reqwest::Client::builder()
        .user_agent("VoidLauncher/0.1.5")
        .build()
        .map_err(|e| LauncherError::ModLoader(format!("Failed to create HTTP client: {}", e)))?;
    for json_url in &json_urls {
        if let Ok(resp) = client.get(json_url).send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    if !text.is_empty() {
                        if let Ok(profile) = serde_json::from_str::<ForgeInstallProfile>(&text) {
                            if let Some(main_class) = profile.main_class {
                                return build_profile(main_class, profile.libraries, profile.arguments);
                            }
                        }
                    }
                }
            }
        }
    }

    tracing::info!(target: "launcher", "Standalone JSON not available, trying Forge installer JAR for version {}", full_version);

    // Try multiple installer URL patterns. Newer Forge versions may use
    // different artifact names on Maven.
    let installer_urls = vec![
        // Pattern 1: forge-{MC}-{FORGE}-installer.jar  (standard)
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            full_version, full_version
        ),
        // Pattern 2: forge-{FORGE}-installer.jar  (some new versions)
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            full_version, forge_version
        ),
        // Pattern 3: directory uses forge version only (newest versions)
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            forge_version, forge_version
        ),
    ];

    let installer_path = std::env::temp_dir().join(format!("forge-{}-installer.jar", full_version));
    let mut last_err = String::new();
    let mut downloaded = false;

    for installer_url in &installer_urls {
        for attempt in 1..=3u32 {
            match crate::download::download_file(installer_url, &installer_path, "").await {
                Ok(()) => {
                    downloaded = true;
                    break;
                }
                Err(e) => {
                    last_err = e.to_string();
                    tracing::warn!(target: "launcher", "Forge installer download attempt {}/3 for {} failed: {}", attempt, installer_url, last_err);
                    if attempt < 3 {
                        tokio::time::sleep(std::time::Duration::from_secs(3 * attempt as u64)).await;
                    }
                }
            }
        }
        if downloaded { break; }
    }

    if !downloaded {
        return Err(LauncherError::ModLoader(format!(
            "Failed to download Forge installer after 3 attempts: {}", last_err
        )));
    }

    let jar_bytes = std::fs::read(&installer_path)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to read Forge installer JAR: {}", e)))?;
    let _ = std::fs::remove_file(&installer_path);

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid Forge installer JAR: {}", e)))?;

    fn read_entry(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                let entry_name = entry.name().replace('\\', "/");
                if entry_name == name || entry_name.ends_with(&format!("/{}", name)) {
                    let mut content = String::new();
                    entry.read_to_string(&mut content).ok()?;
                    return Some(content);
                }
            }
        }
        None
    }

    let install_profile_str = read_entry(&mut archive, "install_profile.json")
        .ok_or_else(|| LauncherError::ModLoader(format!(
            "Forge installer JAR for {} is missing install_profile.json",
            full_version
        )))?;

    let mut profile: ForgeInstallProfile = serde_json::from_str(&install_profile_str)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse Forge install_profile.json: {}", e)))?;

    // If the profile is in the new format (processor-based), try version.json
    if profile.main_class.is_none() || profile.libraries.is_empty() {
        if let Some(json_path) = &profile.json {
            if let Some(version_str) = read_entry(&mut archive, json_path) {
                let version_profile: ForgeInstallProfile = serde_json::from_str(&version_str)
                    .map_err(|e| LauncherError::ModLoader(format!("Failed to parse {}: {}", json_path, e)))?;
                if profile.main_class.is_none() {
                    profile.main_class = version_profile.main_class;
                }
                if profile.libraries.is_empty() {
                    profile.libraries = version_profile.libraries;
                }
                if profile.arguments.is_none() {
                    profile.arguments = version_profile.arguments;
                }
            }
        }

        // Try version.json directly if still no main_class
        if profile.main_class.is_none() || profile.libraries.is_empty() {
            if let Some(version_str) = read_entry(&mut archive, "version.json") {
                let version_profile: ForgeInstallProfile = serde_json::from_str(&version_str)
                    .map_err(|e| LauncherError::ModLoader(format!("Failed to parse version.json: {}", e)))?;
                if profile.main_class.is_none() {
                    profile.main_class = version_profile.main_class;
                }
                if profile.libraries.is_empty() {
                    profile.libraries = version_profile.libraries;
                }
                if profile.arguments.is_none() {
                    profile.arguments = version_profile.arguments;
                }
            }
        }
    }

    let main_class = profile.main_class
        .ok_or_else(|| LauncherError::ModLoader(format!(
            "Forge installer JAR for {} has no mainClass in install_profile.json or version.json",
            full_version
        )))?;

    build_profile(main_class, profile.libraries, profile.arguments)
}

fn build_profile(
    main_class: String,
    libs: Vec<ForgeLibrary>,
    arguments: Option<ForgeArguments>,
) -> Result<LoaderProfile> {
    let mut libraries = Vec::new();
    for lib in libs {
        let (url, path) = if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let lib_path = artifact.path.clone().unwrap_or_else(|| maven_to_path(&lib.name));
                let lib_url = artifact.url.clone().unwrap_or_else(|| {
                    format!("https://maven.minecraftforge.net/{}", lib_path)
                });
                (lib_url, lib_path)
            } else {
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

    let (game_args, jvm_args) = if let Some(args) = arguments {
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
        main_class,
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

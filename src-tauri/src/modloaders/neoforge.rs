use serde::Deserialize;
use crate::error::{LauncherError, Result};
use crate::versions::maven_to_path;
use super::{LoaderVersionPage, LoaderProfile, LoaderLibrary};
use std::io::Read;
use std::path::{Path, PathBuf};
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

    if let Ok(resp) = crate::download::send_with_fallback(client.get(&profile_url)).await {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                if let Ok(profile) = serde_json::from_str::<NeoForgeInstallProfile>(&text) {
                    return build_profile(profile);
                }
            }
        }
    }

    // Fallback: download installer JAR and extract install_profile.json
    let (profile, _installer_path) = download_installer(neo_version).await?;

    build_profile(profile)
}

/// Download the NeoForge installer JAR (with mirror fallbacks) and return
/// the parsed `version.json` profile plus the installer's on-disk path.
///
/// The installer path is NOT deleted here: the processor pipeline in
/// `install` needs the JAR (it carries `data/client.lzma` and the
/// `install_profile.json` processor list).
async fn download_installer(neo_version: &str) -> Result<(NeoForgeInstallProfile, PathBuf)> {
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

    Ok((profile, installer_path))
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
        legacy_args: false,
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

    // Download installer (kept on disk for the processor pipeline below)
    let (raw_profile, installer_path) = download_installer(loader_version).await?;
    let profile = build_profile(raw_profile)?;

    // The installer also carries install_profile.json with the full
    // processor library list (70 libs for 21.1.248, vs 47 in version.json).
    let install_profile_str = {
        let jar_bytes = std::fs::read(&installer_path)
            .map_err(|e| LauncherError::ModLoader(format!("Failed to read NeoForge installer JAR: {}", e)))?;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
            .map_err(|e| LauncherError::ModLoader(format!("Invalid NeoForge installer JAR: {}", e)))?;
        let mut s = String::new();
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                if entry.name() == "install_profile.json" {
                    entry.read_to_string(&mut s)
                        .map_err(|e| LauncherError::ModLoader(format!("Failed to read install_profile.json: {}", e)))?;
                }
            }
        }
        if s.is_empty() {
            return Err(LauncherError::ModLoader("NeoForge installer JAR missing install_profile.json".into()));
        }
        s
    };
    let install_profile: serde_json::Value = serde_json::from_str(&install_profile_str)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e)))?;

    let has_processors = install_profile.get("processors")
        .and_then(|p| p.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    // --- Step 1: download version.json launch libraries ---
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

    // --- Step 2: download the installer's processor libraries ---
    // install_profile.json libraries include everything the processors need
    // (installertools, jarsplitter, binarypatcher, AutoRenamingTool, srgutils,
    // SpecialSource, neoform zip, ...), plus the universal jar.
    if has_processors {
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
                if let Err(e) = super::forge::download_processor_lib(lib_val, libraries_dir, "https://maven.neoforged.net/").await {
                    tracing::warn!(target: "launcher", "NeoForge processor library download warning: {}", e);
                }
            }
        }
    }

    // --- Step 3: run the processor pipeline to generate the client JAR ---
    if has_processors {
        let data_base = "net/neoforged/neoforge";
        // PATCHED = [net.neoforged:neoforge:{version}:client] — the client JAR
        // lives in the plain loader-version dir (21.4.153, NOT 1.21.4-21.4.153
        // as Forge does).
        let client_path = libraries_dir.join("net").join("neoforged").join("neoforge")
            .join(loader_version).join(format!("neoforge-{}-client.jar", loader_version));

        if !client_path.exists() {
            tracing::info!(target: "launcher", "NeoForge client JAR missing, running installer processors...");

            // Extract data files (client.lzma etc.) from the installer into
            // <libraries>/net/neoforged/neoforge/_data/<key>/<file>
            let jar_bytes = std::fs::read(&installer_path)
                .map_err(|e| LauncherError::ModLoader(format!("Failed to read NeoForge installer JAR: {}", e)))?;
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
                .map_err(|e| LauncherError::ModLoader(format!("Invalid NeoForge installer JAR: {}", e)))?;
            if let Some(data_map) = install_profile.get("data").and_then(|d| d.as_object()) {
                for (key, entry) in data_map {
                    let client_val = entry.get("client").and_then(|c| c.as_str()).unwrap_or("");
                    if client_val.starts_with('/') {
                        let jar_path = client_val.trim_start_matches('/');
                        let fname = Path::new(jar_path).file_name().and_then(|n| n.to_str()).unwrap_or("data");
                        let dest = libraries_dir.join(data_base)
                            .join("_data").join(&key.to_lowercase()).join(fname);
                        if !dest.exists() {
                            if let Some(parent) = dest.parent() {
                                std::fs::create_dir_all(parent).ok();
                            }
                            if super::forge::extract_entry_from_jar(&mut archive, jar_path, &dest) {
                                tracing::debug!(target: "launcher", "Extracted data {} -> {}", jar_path, dest.display());
                            } else {
                                tracing::warn!(target: "launcher", "Failed to extract data file: {}", jar_path);
                            }
                        }
                    }
                }
            }
            drop(archive);

            // Locate the vanilla client JAR
            let vd = versions_dir.join(mc_version);
            let client_jar = if vd.join("client.jar").exists() {
                vd.join("client.jar")
            } else {
                vd.join(format!("{}.jar", mc_version))
            };
            if !client_jar.exists() {
                return Err(LauncherError::ModLoader(format!("Client JAR not found at {:?}", client_jar)));
            }

            let root_dir = versions_dir.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| versions_dir.to_path_buf());

            // Java 21 for NeoForge 20.2+ (installertools runs fine on 17 too,
            // but binarypatcher/ART need a recent JDK; the launch flow would
            // use 21 anyway).
            let data_dir = libraries_dir
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::env::temp_dir());
            if let Some(java_path) = super::forge::find_java_for(&data_dir, mc_version) {
                tracing::info!(target: "launcher", "Running NeoForge processors with Java: {:?}", java_path);
                if let Err(e) = super::forge::run_forge_processors(
                    &install_profile,
                    &installer_path,
                    &java_path,
                    libraries_dir,
                    mc_version,
                    &client_jar,
                    &root_dir,
                    data_base,
                ) {
                    tracing::warn!(target: "launcher", "NeoForge processor run failed: {}", e);
                }
            } else {
                tracing::warn!(target: "launcher", "Java not found — cannot run NeoForge processors");
            }

            if !client_path.exists() {
                return Err(LauncherError::ModLoader(
                    "NeoForge processors did not generate the client JAR".into()
                ));
            }
        } else {
            tracing::debug!(target: "launcher", "NeoForge client JAR already present");
        }
    }

    std::fs::remove_file(&installer_path).ok();

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

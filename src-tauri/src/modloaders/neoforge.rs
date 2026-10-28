use super::{LoaderLibrary, LoaderProfile, LoaderVersionPage};
use crate::error::{LauncherError, Result};
use crate::versions::maven_to_path;
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use tauri::Emitter;

/// Maven group path + version used in the artifact URL for a given NeoForge
/// `neo_version` (bare, e.g. `47.1.106` or `21.4.157`) for a Minecraft version.
///
/// NeoForge 1.20.1 (the 47.x generation) is a fork of Forge and lives under
/// the `net.neoforged:forge` coordinate *with* the Minecraft version prefixed
/// to the artifact version (maven path `net/neoforged/forge/{mc}-{loader}/...`,
/// artifact `forge-{mc}-{loader}.jar`). Every newer NeoForge (1.20.2+,
/// renumbered to 20.x) uses its own `net.neoforged:neoforge` coordinate with
/// the bare loader version (maven path `net/neoforged/neoforge/{loader}/...`,
/// artifact `neoforge-{loader}.jar`). Verifying against
/// `maven.neoforged.net`: only the 1.20.1 line exists under `neoforged/forge`,
/// everything else is under `neoforged/neoforge`.
///
/// Returns `(group_path, maven_version)` — e.g. `("net/neoforged/forge",
/// "1.20.1-47.1.106")` for 1.20.1, or `("net/neoforged/neoforge", "21.4.157")`
/// for modern NeoForge.
fn neoforge_maven(mc_version: &str, neo_version: &str) -> (String, String) {
    if mc_version == "1.20.1" {
        (
            "net/neoforged/forge".to_string(),
            format!("{}-{}", mc_version, neo_version),
        )
    } else {
        (
            "net/neoforged/neoforge".to_string(),
            neo_version.to_string(),
        )
    }
}

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
/// On fetch/parse failure the underlying `prism_meta` call returns an
/// error; the wizard shows it with a retry button (a stale disk cache is
/// used first when available).
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
pub async fn get_profile(mc_version: &str, neo_version: &str) -> Result<LoaderProfile> {
    let client = crate::download::global_http_client()?;

    let (group_path, maven_version) = neoforge_maven(mc_version, neo_version);

    // Try legacy standalone JSON first
    let profile_url = format!(
        "https://maven.neoforged.net/releases/{}/{}/{}-{}.json",
        group_path,
        maven_version,
        group_path.rsplit('/').next().unwrap_or("neoforge"),
        maven_version
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
    let (profile, _installer_path) = download_installer(mc_version, neo_version).await?;

    build_profile(profile)
}

/// Download the NeoForge installer JAR (with mirror fallbacks) and return
/// the parsed `version.json` profile plus the installer's on-disk path.
///
/// The installer path is NOT deleted here: the processor pipeline in
/// `install` needs the JAR (it carries `data/client.lzma` and the
/// `install_profile.json` processor list).
async fn download_installer(
    mc_version: &str,
    neo_version: &str,
) -> Result<(NeoForgeInstallProfile, PathBuf)> {
    super::validate_loader_request(mc_version, neo_version)?;
    let (group_path, maven_version) = neoforge_maven(mc_version, neo_version);
    let artifact = group_path.rsplit('/').next().unwrap_or("neoforge");
    let installer_url = format!(
        "https://maven.neoforged.net/releases/{}/{}/{}-{}-installer.jar",
        group_path, maven_version, artifact, maven_version
    );

    tracing::info!(target: "launcher", "Fetching NeoForge installer JAR: {}", installer_url);

    // Use download_file with retries for reliable download
    let installer_path = installer_cache_path(mc_version, neo_version);

    // Reuse a cached installer only when it is a real archive. The file is
    // kept after a successful install so repairing a processor JAR does not
    // re-download it, which also makes this validation matter in practice.
    if installer_path.exists() {
        if super::forge::installer_jar_is_valid(&installer_path) {
            tracing::debug!(target: "launcher", "Reusing existing NeoForge installer at {}", installer_path.display());
        } else {
            tracing::warn!(
                target: "launcher",
                "Cached NeoForge installer is not a valid archive, re-downloading: {}",
                installer_path.display()
            );
            let _ = std::fs::remove_file(&installer_path);
        }
    }

    let mut last_err = String::new();
    if !installer_path.exists() {
        for attempt in 1..=3u32 {
            match crate::download::download_file(&installer_url, &installer_path, "").await {
                Ok(()) => {
                    if super::forge::installer_jar_is_valid(&installer_path) {
                        break;
                    }
                    last_err = "downloaded file is not a valid installer archive".to_string();
                    let _ = std::fs::remove_file(&installer_path);
                    tracing::warn!(target: "launcher", "NeoForge installer download attempt {}/3 produced an invalid archive", attempt);
                    if attempt < 3 {
                        tokio::time::sleep(std::time::Duration::from_secs(3 * attempt as u64))
                            .await;
                    }
                }
                Err(e) => {
                    last_err = e.to_string();
                    tracing::warn!(target: "launcher", "NeoForge installer download attempt {}/3 failed: {}", attempt, last_err);
                    if attempt < 3 {
                        tokio::time::sleep(std::time::Duration::from_secs(3 * attempt as u64))
                            .await;
                    }
                }
            }
        }
    }

    if !installer_path.exists() {
        return Err(LauncherError::ModLoader(format!(
            "Failed to download NeoForge installer after 3 attempts: {}",
            last_err
        )));
    }

    let jar_bytes = std::fs::read(&installer_path).map_err(|e| {
        LauncherError::ModLoader(format!("Failed to read NeoForge installer JAR: {}", e))
    })?;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid NeoForge installer JAR: {}", e)))?;

    let mut install_profile_str = String::new();
    let mut version_json_str = String::new();
    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            if entry.name() == "install_profile.json" {
                entry
                    .read_to_string(&mut install_profile_str)
                    .map_err(|e| {
                        LauncherError::ModLoader(format!(
                            "Failed to read install_profile.json: {}",
                            e
                        ))
                    })?;
            } else if entry.name() == "version.json" {
                entry.read_to_string(&mut version_json_str).map_err(|e| {
                    LauncherError::ModLoader(format!("Failed to read version.json: {}", e))
                })?;
            }
        }
    }

    if install_profile_str.is_empty() {
        return Err(LauncherError::ModLoader(
            "NeoForge installer JAR missing install_profile.json".into(),
        ));
    }

    // NeoForge installer JAR structure:
    //   install_profile.json — installer metadata, points to the actual version JSON via "json" field
    //   version.json — the real version profile with mainClass, libraries, arguments
    // The install_profile.json has "json": "/version.json" pointing to the version profile.
    // We read version.json from the JAR as the actual profile.

    let profile: NeoForgeInstallProfile = if !version_json_str.is_empty() {
        // version.json is the actual version profile (same format as Minecraft version JSON)
        serde_json::from_str(&version_json_str).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to parse NeoForge version.json: {}", e))
        })?
    } else {
        // Fallback: try install_profile.json directly
        let parsed: serde_json::Value =
            serde_json::from_str(&install_profile_str).map_err(|e| {
                LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e))
            })?;
        if let Some(version_info) = parsed.get("versionInfo") {
            serde_json::from_value(version_info.clone()).map_err(|e| {
                LauncherError::ModLoader(format!("Failed to parse NeoForge versionInfo: {}", e))
            })?
        } else {
            serde_json::from_value(parsed).map_err(|e| {
                LauncherError::ModLoader(format!("Failed to parse NeoForge install profile: {}", e))
            })?
        }
    };

    Ok((profile, installer_path))
}

fn build_profile(profile: NeoForgeInstallProfile) -> Result<LoaderProfile> {
    let mut libraries = Vec::new();
    for lib in &profile.libraries {
        let (url, path) = if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let lib_path = artifact
                    .path
                    .clone()
                    .unwrap_or_else(|| maven_to_path(&lib.name));
                let lib_url = artifact
                    .url
                    .clone()
                    .unwrap_or_else(|| format!("https://maven.neoforged.net/{}", lib_path));
                (lib_url, lib_path)
            } else {
                let path = maven_to_path(&lib.name);
                let base = lib
                    .url
                    .clone()
                    .unwrap_or_else(|| "https://maven.neoforged.net/".into());
                let base = if base.ends_with('/') {
                    base
                } else {
                    format!("{}/", base)
                };
                (format!("{}{}", base, path), path)
            }
        } else {
            let path = maven_to_path(&lib.name);
            let base = lib
                .url
                .clone()
                .unwrap_or_else(|| "https://maven.neoforged.net/".into());
            let base = if base.ends_with('/') {
                base
            } else {
                format!("{}/", base)
            };
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

    // Loader metadata is remote input too: a path containing `..`, an
    // absolute path or a drive prefix must never reach a filesystem join.
    for lib in &libraries {
        if crate::import::check_safe_remote_path(&lib.path).is_err() {
            return Err(LauncherError::ModLoader(format!(
                "Unsafe library path in NeoForge profile: {}",
                lib.path
            )));
        }
    }

    let (game_args, jvm_args) = if let Some(args) = &profile.arguments {
        let game = args
            .game
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let jvm = args
            .jvm
            .iter()
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

/// Cached installer path. Keeping it after a successful install lets a
/// repair re-run the processors without downloading the installer again.
pub(crate) fn installer_cache_path(mc_version: &str, loader_version: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "neoforge-{}--{}-installer.jar",
        mc_version, loader_version
    ))
}

/// Client JAR path used by NeoForge generations whose `install_profile.json`
/// names the loader's own `:client` classifier (20.x/21.x and the 1.20.1
/// `net.neoforged:forge` fork). Modern NeoForge (26.x) moved the output to
/// `net.neoforged:minecraft-client-patched`, so this is a fallback only — the
/// installer metadata is authoritative whenever it is available.
fn legacy_client_jar_path(libraries_dir: &Path, mc_version: &str, loader_version: &str) -> PathBuf {
    let (group_path, maven_version) = neoforge_maven(mc_version, loader_version);
    let artifact = group_path.rsplit('/').next().unwrap_or("neoforge");
    libraries_dir
        .join(&group_path)
        .join(&maven_version)
        .join(format!("{}-{}-client.jar", artifact, maven_version))
}

/// Relative library path of the patched client declared by
/// `install_profile.json` → `data.PATCHED.client`.
///
/// The value is a Maven coordinate, either bare
/// (`net.neoforged:minecraft-client-patched:26.3.0.99`) or in Maven bracket
/// form (`[net.neoforged:neoforge:21.4.157:client]`). It comes from remote
/// metadata, so anything that is not a plain coordinate (placeholders,
/// extensions, path separators, traversal) is rejected rather than joined
/// into the libraries directory.
fn patched_client_rel_path(install_profile: &serde_json::Value) -> Option<PathBuf> {
    let raw = install_profile
        .get("data")?
        .get("PATCHED")?
        .get("client")?
        .as_str()?
        .trim();
    let coord = match raw.strip_prefix('[') {
        Some(inner) => inner.strip_suffix(']')?.trim(),
        None => raw,
    };
    if coord.is_empty()
        || coord.contains('{')
        || coord.contains('\\')
        || coord.contains('/')
        || coord.contains('@')
        || coord.split(':').count() < 3
    {
        tracing::warn!(
            target: "launcher",
            "Ignoring unusable NeoForge PATCHED coordinate: {}",
            raw
        );
        return None;
    }
    let rel = maven_to_path(coord);
    if crate::import::check_safe_remote_path(&rel).is_err() {
        tracing::warn!(
            target: "launcher",
            "Ignoring unsafe NeoForge PATCHED coordinate: {}",
            raw
        );
        return None;
    }
    Some(PathBuf::from(rel))
}

/// Best-effort location used by NeoForge 26.x (`minecraft-client-patched`),
/// needed when the cached installer is gone: verification must not report a
/// healthy install as broken just because `%TEMP%` was cleaned. The path is
/// version-scoped, so it cannot point at another loader version's output.
fn modern_client_jar_guess(
    libraries_dir: &Path,
    mc_version: &str,
    loader_version: &str,
) -> PathBuf {
    let (_, maven_version) = neoforge_maven(mc_version, loader_version);
    libraries_dir
        .join("net/neoforged/minecraft-client-patched")
        .join(&maven_version)
        .join(format!("minecraft-client-patched-{}.jar", maven_version))
}

/// Every path the processor-generated client JAR may occupy, in priority
/// order. With installer metadata at hand the declared output is
/// authoritative; the legacy path stays as a fallback so instances installed
/// by older launcher builds keep working. Without metadata (cached installer
/// gone), the known 26.x layout is checked before the legacy one.
pub(crate) fn expected_client_jar_paths(
    libraries_dir: &Path,
    mc_version: &str,
    loader_version: &str,
    install_profile: Option<&serde_json::Value>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    match install_profile.and_then(patched_client_rel_path) {
        Some(rel) => paths.push(libraries_dir.join(rel)),
        None => {
            // 1.20.1 has no `minecraft-client-patched` layout; skip the noise.
            if mc_version != "1.20.1" {
                paths.push(modern_client_jar_guess(
                    libraries_dir,
                    mc_version,
                    loader_version,
                ));
            }
        }
    }
    let legacy = legacy_client_jar_path(libraries_dir, mc_version, loader_version);
    if !paths.contains(&legacy) {
        paths.push(legacy);
    }
    paths
}

/// First candidate that is a structurally valid JAR.
pub(crate) fn find_client_jar(paths: &[PathBuf]) -> Option<PathBuf> {
    paths
        .iter()
        .find(|path| super::forge::is_usable_jar(path))
        .cloned()
}

/// Read only `install_profile.json` from an installer JAR, for fast-path
/// checks that must not download anything.
pub(crate) fn read_install_profile_from_installer(
    installer_path: &Path,
) -> Option<serde_json::Value> {
    let bytes = std::fs::read(installer_path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let text = super::forge::read_entry_from_jar(&mut archive, "install_profile.json")?;
    serde_json::from_str(&text).ok()
}

/// Self-heal NeoForge at launch: if the processor-generated client JAR is
/// missing/corrupt (e.g. quarantined by antivirus), re-run the installer.
/// The validated installer is cached, so a repair does not re-download it.
pub async fn ensure_processor_jars(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
) -> Result<()> {
    // The client JAR path is derived from both versions before any network
    // call, so a tampered instance.json must not reach the filesystem.
    super::validate_loader_request(mc_version, loader_version)?;

    // Fast path: read the output coordinate from the cached installer. This
    // is what makes a healthy 26.x install skip the pipeline even though its
    // JAR is not at the legacy `neoforge-<ver>-client.jar` location.
    let cached_profile =
        read_install_profile_from_installer(&installer_cache_path(mc_version, loader_version));
    let candidates = expected_client_jar_paths(
        libraries_dir,
        mc_version,
        loader_version,
        cached_profile.as_ref(),
    );
    if find_client_jar(&candidates).is_some() {
        return Ok(());
    }

    tracing::info!(
        target: "launcher",
        "NeoForge client JAR missing or unusable (checked {:?}), running installer processors to repair it",
        candidates
    );
    install(
        mc_version,
        loader_version,
        libraries_dir,
        versions_dir,
        None,
    )
    .await?;
    Ok(())
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
    let (raw_profile, installer_path) = download_installer(mc_version, loader_version).await?;
    let profile = build_profile(raw_profile)?;

    // The installer also carries install_profile.json with the full
    // processor library list (70 libs for 21.1.248, vs 47 in version.json).
    let install_profile_str = {
        let jar_bytes = std::fs::read(&installer_path).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to read NeoForge installer JAR: {}", e))
        })?;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes)).map_err(|e| {
            LauncherError::ModLoader(format!("Invalid NeoForge installer JAR: {}", e))
        })?;
        let mut s = String::new();
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                if entry.name() == "install_profile.json" {
                    entry.read_to_string(&mut s).map_err(|e| {
                        LauncherError::ModLoader(format!(
                            "Failed to read install_profile.json: {}",
                            e
                        ))
                    })?;
                }
            }
        }
        if s.is_empty() {
            return Err(LauncherError::ModLoader(
                "NeoForge installer JAR missing install_profile.json".into(),
            ));
        }
        s
    };
    let install_profile: serde_json::Value =
        serde_json::from_str(&install_profile_str).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e))
        })?;

    let has_processors = install_profile
        .get("processors")
        .and_then(|p| p.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    // --- Step 1: download version.json launch libraries ---
    let total = profile.libraries.len();
    for (i, lib) in profile.libraries.iter().enumerate() {
        let lib_path = libraries_dir.join(&lib.path);
        if !lib_path.exists() {
            if let Some(app_handle) = app {
                let _ = app_handle.emit(
                    "loader-install-progress",
                    serde_json::json!({
                        "stage": "downloading",
                        "message": format!("Downloading {} ({}/{})", lib.name, i + 1, total),
                    }),
                );
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
                if let Err(e) = super::forge::download_processor_lib(
                    lib_val,
                    libraries_dir,
                    "https://maven.neoforged.net/",
                )
                .await
                {
                    tracing::warn!(target: "launcher", "NeoForge processor library download warning: {}", e);
                }
            }
        }
    }

    // --- Step 3: run the processor pipeline to generate the client JAR ---
    if has_processors {
        // The loader's maven directory inside libraries_dir. For NeoForge
        // 1.20.1 this is the `net.neoforged:forge` fork ("net/neoforged/forge/<mc-loader>"),
        // for modern NeoForge it is "net/neoforged/neoforge/<loader>". The
        // installer data files are keyed by this path + the maven version.
        let (group_path, maven_version) = neoforge_maven(mc_version, loader_version);
        let data_base = group_path.clone();
        // The processor output is declared by the installer itself. Older
        // profiles name `net.neoforged:neoforge:<ver>:client` (which we keep
        // as a fallback), modern 26.x profiles name
        // `net.neoforged:minecraft-client-patched:<ver>`. Hard-coding either
        // layout made the other one fail right after a successful pipeline.
        let client_paths = expected_client_jar_paths(
            libraries_dir,
            mc_version,
            loader_version,
            Some(&install_profile),
        );

        if find_client_jar(&client_paths).is_none() {
            tracing::info!(
                target: "launcher",
                "NeoForge client JAR missing or unusable (checked {:?}), running installer processors...",
                client_paths
            );

            // Extract data files (client.lzma etc.) from the installer into
            // <libraries>/net/neoforged/neoforge/_data/<key>/<file>
            let jar_bytes = std::fs::read(&installer_path).map_err(|e| {
                LauncherError::ModLoader(format!("Failed to read NeoForge installer JAR: {}", e))
            })?;
            let mut archive =
                zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes)).map_err(|e| {
                    LauncherError::ModLoader(format!("Invalid NeoForge installer JAR: {}", e))
                })?;
            if let Some(data_map) = install_profile.get("data").and_then(|d| d.as_object()) {
                for (key, entry) in data_map {
                    let client_val = entry.get("client").and_then(|c| c.as_str()).unwrap_or("");
                    if client_val.starts_with('/') {
                        let jar_path = client_val.trim_start_matches('/');
                        let fname = Path::new(jar_path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("data");
                        // The data-map key becomes a directory component; a
                        // crafted ".." / separator must not escape _data.
                        let key_lower = key.to_lowercase();
                        if crate::import::check_safe_file_name(&key_lower).is_err() {
                            return Err(LauncherError::ModLoader(format!(
                                "NeoForge install profile has an unsafe data key: {key}"
                            )));
                        }
                        let dest = super::forge::data_file_path(
                            libraries_dir,
                            &data_base,
                            &maven_version,
                            &key_lower,
                            fname,
                        );
                        // Always (over)write: the directory is keyed by the
                        // loader version, so a correct file can only come
                        // from this loader, and overwriting clears leftovers
                        // of an interrupted extract.
                        if let Some(parent) = dest.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| {
                                LauncherError::ModLoader(format!(
                                    "Failed to create NeoForge data directory {}: {}",
                                    parent.display(),
                                    e
                                ))
                            })?;
                        }
                        if super::forge::extract_entry_from_jar(&mut archive, jar_path, &dest) {
                            tracing::debug!(target: "launcher", "Extracted data {} -> {}", jar_path, dest.display());
                        } else {
                            // Without this file the patcher cannot work; a
                            // warning here only delayed the failure until the
                            // pipeline reported a missing output.
                            return Err(LauncherError::ModLoader(format!(
                                "NeoForge installer is missing data file '{jar_path}' required by the processors"
                            )));
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
                return Err(LauncherError::ModLoader(format!(
                    "Client JAR not found at {:?}",
                    client_jar
                )));
            }

            let root_dir = versions_dir
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| versions_dir.to_path_buf());

            // Java 21 for NeoForge 20.2+ (installertools runs fine on 17 too,
            // but binarypatcher/ART need a recent JDK; the launch flow would
            // use 21 anyway).
            let data_dir = libraries_dir
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(std::env::temp_dir);
            if let Some(java_path) = super::forge::find_java_for(&data_dir, mc_version) {
                tracing::info!(target: "launcher", "Running NeoForge processors with Java: {:?}", java_path);
                // Long-lived subprocess pipeline (30-120s) — run on the
                // blocking pool so it can't stall the async runtime that
                // drives the UI and other downloads.
                let profile = install_profile.clone();
                let installer = installer_path.clone();
                let java = java_path.clone();
                let libs = libraries_dir.to_path_buf();
                let ver = mc_version.to_string();
                let cjar = client_jar.clone();
                let root = root_dir.clone();
                let data_base_owned = data_base.to_string();
                let data_ver = maven_version.clone();
                let pipeline = match tokio::task::spawn_blocking(move || {
                    super::forge::run_forge_processors(super::forge::ForgeProcessorInputs {
                        install_profile: &profile,
                        installer_path: &installer,
                        java_path: &java,
                        libraries_dir: &libs,
                        mc_version: &ver,
                        client_jar: &cjar,
                        root_dir: &root,
                        data_base: &data_base_owned,
                        data_version: &data_ver,
                    })
                })
                .await
                {
                    Ok(inner) => inner,
                    Err(e) => Err(LauncherError::ModLoader(format!(
                        "NeoForge processor task failed: {}",
                        e
                    ))),
                };
                if let Err(e) = pipeline {
                    // A failed processor leaves the pipeline half-applied —
                    // do not continue as if the install succeeded. Drop the
                    // (possibly partial) patched-client output so the next
                    // attempt re-runs the whole pipeline instead of finding
                    // it "already present".
                    tracing::warn!(target: "launcher", "NeoForge processor run failed: {}", e);
                    for path in &client_paths {
                        std::fs::remove_file(path).ok();
                    }
                    std::fs::remove_file(&installer_path).ok();
                    return Err(LauncherError::ModLoader(format!(
                        "NeoForge processor pipeline failed: {}",
                        e
                    )));
                }
            } else {
                // Without Java the pipeline cannot run; the old generic
                // "processors did not generate the client JAR" error hid the
                // actual, user-fixable cause.
                return Err(LauncherError::ModLoader(format!(
                    "No Java {} installation was found to run the NeoForge installers. Install a matching JDK (or let the launcher download its managed Java) and try again.",
                    super::forge::required_forge_java_major(mc_version)
                )));
            }

            if find_client_jar(&client_paths).is_none() {
                return Err(LauncherError::ModLoader(format!(
                    "NeoForge processors completed but produced no usable client JAR at any expected path: {}",
                    client_paths
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        } else {
            tracing::debug!(target: "launcher", "NeoForge client JAR already present");
        }
    }

    // Keep the validated installer cached so `ensure_processor_jars` can
    // repair a deleted client JAR without downloading it again.

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_neoforge_test_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A structurally valid JAR (see `forge::is_usable_jar`).
    fn write_jar(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        zw.start_file(
            "META-INF/MANIFEST.MF",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zw.write_all(b"Manifest-Version: 1.0\n").unwrap();
        zw.start_file("padding.bin", zip::write::SimpleFileOptions::default())
            .unwrap();
        // High-entropy padding keeps the archive above the 1 KiB floor even
        // when the writer compresses it.
        let padding: Vec<u8> = (0..8192u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        zw.write_all(&padding).unwrap();
        zw.finish().unwrap();
    }

    fn profile_with_patched(value: &str) -> serde_json::Value {
        serde_json::json!({
            "data": {
                "PATCHED": { "client": value, "server": value },
                "BINPATCH": { "client": "/data/client.lzma" }
            }
        })
    }

    /// NeoForge 1.20.1 (the `net.neoforged:forge` fork) must use the MC-prefixed
    /// gradle-style version under the forge group.
    #[test]
    fn neoforge_maven_uses_forge_fork_for_1_20_1() {
        let (group, version) = neoforge_maven("1.20.1", "47.1.106");
        assert_eq!(group, "net/neoforged/forge");
        assert_eq!(version, "1.20.1-47.1.106");
    }

    /// Modern NeoForge (renumbered, e.g. 1.20.2+ / 20.x line) uses its own
    /// `neoforge` group with the bare loader version.
    #[test]
    fn neoforge_maven_uses_neoforge_group_for_modern_versions() {
        let (group, version) = neoforge_maven("1.20.4", "20.4.251");
        assert_eq!(group, "net/neoforged/neoforge");
        assert_eq!(version, "20.4.251");
    }

    /// 1.21.4 (the currently targeted modern release) also uses the bare version.
    #[test]
    fn neoforge_maven_uses_neoforge_group_for_1_21_4() {
        let (group, version) = neoforge_maven("1.21.4", "21.4.157");
        assert_eq!(group, "net/neoforged/neoforge");
        assert_eq!(version, "21.4.157");
    }

    /// Other old-generation forks around 1.20.1 (e.g. 1.18/1.19 Forge-compat)
    /// are not part of NeoForge — only 1.20.1 gates on the forge fork.
    #[test]
    fn neoforge_maven_1_20_2_uses_neoforge_group() {
        let (group, version) = neoforge_maven("1.20.2", "20.2.93");
        assert_eq!(group, "net/neoforged/neoforge");
        assert_eq!(version, "20.2.93");
    }

    #[test]
    fn patched_client_path_follows_modern_installer_metadata() {
        let profile = profile_with_patched("[net.neoforged:minecraft-client-patched:26.3.0.99]");
        let rel = patched_client_rel_path(&profile).expect("modern coordinate must parse");
        assert_eq!(
            rel,
            PathBuf::from(
                "net/neoforged/minecraft-client-patched/26.3.0.99/minecraft-client-patched-26.3.0.99.jar"
            )
        );
    }

    #[test]
    fn patched_client_path_follows_legacy_installer_metadata() {
        // NeoForge 21.x (and the 1.20.1 forge fork) write the loader's own
        // `:client` artifact; that must still be recognized.
        let modern = profile_with_patched("[net.neoforged:neoforge:21.4.157:client]");
        assert_eq!(
            patched_client_rel_path(&modern).unwrap(),
            PathBuf::from("net/neoforged/neoforge/21.4.157/neoforge-21.4.157-client.jar")
        );

        let fork = profile_with_patched("[net.neoforged:forge:1.20.1-47.1.106:client]");
        assert_eq!(
            patched_client_rel_path(&fork).unwrap(),
            PathBuf::from("net/neoforged/forge/1.20.1-47.1.106/forge-1.20.1-47.1.106-client.jar")
        );
    }

    #[test]
    fn patched_client_path_rejects_unsafe_or_missing_metadata() {
        for raw in [
            "../escape:1.0",
            "[net.neoforged:neoforge:21.4.157:client@jar]",
            "[net.neoforged/../../neoforge:21.4.157:client]",
            "net.neoforged:neoforge",
            "{PATCHED}",
        ] {
            assert!(
                patched_client_rel_path(&profile_with_patched(raw)).is_none(),
                "coordinate {raw:?} must be rejected"
            );
        }
        let empty = serde_json::json!({ "data": {} });
        assert!(patched_client_rel_path(&empty).is_none());
    }

    #[test]
    fn expected_client_jar_paths_cover_both_layouts_without_metadata() {
        let dir = temp_dir("expected_paths");
        let paths = expected_client_jar_paths(&dir, "26.3", "26.3.0.99", None);
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            dir.join(
                "net/neoforged/minecraft-client-patched/26.3.0.99/minecraft-client-patched-26.3.0.99.jar"
            )
        );
        assert_eq!(
            paths[1],
            dir.join("net/neoforged/neoforge/26.3.0.99/neoforge-26.3.0.99-client.jar")
        );

        // Metadata is authoritative: only it and the legacy fallback remain.
        let profile = profile_with_patched("[net.neoforged:minecraft-client-patched:26.3.0.99]");
        let paths = expected_client_jar_paths(&dir, "26.3", "26.3.0.99", Some(&profile));
        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            dir.join(
                "net/neoforged/minecraft-client-patched/26.3.0.99/minecraft-client-patched-26.3.0.99.jar"
            )
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_client_jar_accepts_the_metadata_path_and_skips_invalid_files() {
        let dir = temp_dir("find_jar");
        let profile = profile_with_patched("[net.neoforged:minecraft-client-patched:26.3.0.99]");
        let paths = expected_client_jar_paths(&dir, "26.3", "26.3.0.99", Some(&profile));

        // A tiny/truncated file is not an install.
        std::fs::create_dir_all(paths[1].parent().unwrap()).unwrap();
        std::fs::write(&paths[1], b"not a zip").unwrap();
        assert!(find_client_jar(&paths).is_none());

        write_jar(&paths[0]);
        assert_eq!(find_client_jar(&paths).as_deref(), Some(paths[0].as_path()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

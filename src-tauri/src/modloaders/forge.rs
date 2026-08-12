use serde::Deserialize;
use crate::error::{LauncherError, Result};
use crate::versions::maven_to_path;
use super::{LoaderVersionPage, LoaderProfile, LoaderLibrary};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Forge installer profile
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
pub async fn get_loader_versions(
    mc_version: &str,
    offset: usize,
    limit: usize,
) -> Result<LoaderVersionPage> {
    super::prism_meta::fetch_loader_versions("net.minecraftforge", Some(mc_version), offset, limit).await
}

/// Download the Forge installer JAR, return path.
async fn download_installer(mc_version: &str, forge_version: &str) -> Result<PathBuf> {
    let full_version = if forge_version.contains('-') {
        forge_version.to_string()
    } else {
        format!("{}-{}", mc_version, forge_version)
    };

    // Legacy Forge (MC 1.8.x and older) publishes some builds under the
    // `{mc}-{build}-{mc}` maven version (e.g. 1.8.9-11.15.1.2318-1.8.9)
    // instead of `{mc}-{build}`.
    let legacy_full_version = if forge_version.contains('-') {
        forge_version.to_string()
    } else {
        format!("{}-{}-{}", mc_version, forge_version, mc_version)
    };

    let installer_urls = vec![
        format!("https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar", full_version, full_version),
        format!("https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar", legacy_full_version, legacy_full_version),
        format!("https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar", full_version, forge_version),
        format!("https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar", forge_version, forge_version),
    ];

    let installer_path = std::env::temp_dir().join(format!("forge-{}-installer.jar", full_version));

    // Reuse existing downloaded installer
    if installer_path.exists() && installer_path.metadata().map(|m| m.len() > 1000).unwrap_or(false) {
        tracing::debug!(target: "launcher", "Reusing existing installer at {}", installer_path.display());
        return Ok(installer_path);
    }

    let mut last_err = String::new();
    let mut downloaded = false;

    for installer_url in &installer_urls {
        for attempt in 1..=3u32 {
            match crate::download::download_file(installer_url, &installer_path, "").await {
                Ok(()) => { downloaded = true; break; }
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
        return Err(LauncherError::ModLoader(format!("Failed to download Forge installer after 3 attempts: {}", last_err)));
    }

    Ok(installer_path)
}

/// Find Java on the system for running the Forge installer processes.
fn find_java() -> Option<PathBuf> {
    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        let p = PathBuf::from(&java_home).join("bin").join("java.exe");
        if p.exists() { return Some(p); }
        let p = PathBuf::from(&java_home).join("bin").join("java");
        if p.exists() { return Some(p); }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            let p = PathBuf::from(dir).join("java.exe");
            if p.exists() { return Some(p); }
            let p = PathBuf::from(dir).join("java");
            if p.exists() { return Some(p); }
        }
    }
    let common = vec![
        r"C:\Program Files\Java",
        r"C:\Program Files\Eclipse Adoptium",
        r"C:\Program Files\Microsoft\jdk",
        r"C:\Program Files\OpenJDK",
        r"C:\Program Files\Amazon Corretto",
        r"C:\Program Files\BellSoft\LibericaJDK",
        r"C:\Program Files (x86)\Java",
        r"C:\Program Files (x86)\Eclipse Adoptium",
    ];
    for base in common {
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                let p = entry.path().join("bin").join("java.exe");
                if p.exists() { return Some(p); }
            }
        }
    }
    if std::process::Command::new("java").arg("-version").output().is_ok() {
        return Some(PathBuf::from("java"));
    }
    None
}

/// Java major version expected by Forge for a MC version:
/// 1.8–1.16.5 → 8, 1.17 → 16, 1.18–1.20.4 → 17, 1.20.5+ → 21.
pub(crate) fn required_forge_java_major(mc_version: &str) -> u32 {
    let parts: Vec<&str> = mc_version.trim_start_matches("1.").split('.').collect();
    let minor: u32 = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch: u32 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    match (minor, patch) {
        (m, _) if m < 17 => 8,
        (17, _) => 16,
        (m, p) if m < 20 || (m == 20 && p < 5) => 17,
        _ => 21,
    }
}

/// Pick a Java for the Forge installer processor pipeline: prefer the
/// same auto-detection the launch flow uses (`find_all_java_installations`
/// + `get_recommended_java` — exact major match, else closest higher),
/// so we never run old ForgeGradle with a too-new JVM (which fails with
/// cryptic "did not generate" errors). Falls back to any system Java.
pub(crate) fn find_java_for(data_dir: &Path, mc_version: &str) -> Option<PathBuf> {
    let want = required_forge_java_major(mc_version);
    let data_dir = data_dir.to_path_buf();
    let all = crate::launch::find_all_java_installations(&data_dir);
    match crate::java::get_recommended_java(Some(want), &all) {
        Some(j) => {
            tracing::debug!(target: "launcher", "Using Java {} (v{}) for Forge processors (want {})",
                j.major_version, j.version, want);
            Some(j.path)
        }
        None => find_java(),
    }
}

/// Convert a Maven artifact coordinate `group:name:version[:classifier][@ext]`
/// to a local filesystem path under libraries_dir.
pub(crate) fn artifact_to_path(artifact: &str, libraries_dir: &Path, ext: &str) -> PathBuf {
    let parts: Vec<&str> = artifact.split(':').collect();
    if parts.len() < 3 { return PathBuf::new(); }
    let group_path = parts[0].replace('.', "/");
    let artifact_name = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).copied().unwrap_or("");
    let filename = if classifier.is_empty() {
        format!("{}-{}.{}", artifact_name, version, ext)
    } else {
        format!("{}-{}-{}.{}", artifact_name, version, classifier, ext)
    };
    libraries_dir.join(&group_path).join(artifact_name).join(version).join(filename)
}

/// Parse an artifact reference like `[group:name:version:classifier@ext]` 
/// and resolve to a local path under libraries_dir.
pub(crate) fn resolve_artifact_ref(ref_str: &str, libraries_dir: &Path) -> PathBuf {
    let inner = ref_str.trim_start_matches('[').trim_end_matches(']');
    let (coord, ext) = if let Some(at_pos) = inner.rfind('@') {
        (&inner[..at_pos], &inner[at_pos+1..])
    } else {
        (inner, "jar")
    };
    artifact_to_path(coord, libraries_dir, ext)
}

/// Read a text entry from inside a JAR (zip) archive.
pub(crate) fn read_entry_from_jar<R: Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
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

/// Extract a binary entry from inside a JAR (zip) archive to a destination path.
pub(crate) fn extract_entry_from_jar<R: Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>, name: &str, dest: &Path) -> bool {
    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            let entry_name = entry.name().replace('\\', "/");
            if entry_name == name || entry_name.ends_with(&format!("/{}", name)) {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                if let Ok(mut file) = std::fs::File::create(dest) {
                    use std::io::Write;
                    let mut buf = Vec::new();
                    if entry.read_to_end(&mut buf).is_ok() && file.write_all(&buf).is_ok() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Read the main class from a JAR file's manifest.
pub(crate) fn get_jar_main_class(jar_path: &Path) -> Option<String> {
    let bytes = std::fs::read(jar_path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for line in read_entry_from_jar(&mut archive, "META-INF/MANIFEST.MF")?.lines() {
        if let Some(stripped) = line.strip_prefix("Main-Class: ") {
            return Some(stripped.trim().to_string());
        }
    }
    None
}

/// Download one processor library from the install_profile.json libraries list.
/// Returns Ok if already exists or downloaded successfully.
pub(crate) async fn download_processor_lib(
    lib_val: &serde_json::Value,
    libraries_dir: &Path,
    maven_base: &str,
) -> Result<()> {
    let name = lib_val.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if name.is_empty() { return Ok(()); }

    // Build URL and path from the downloads.artifact section
    let (url, path_str) = if let Some(downloads) = lib_val.get("downloads") {
        if let Some(artifact) = downloads.get("artifact") {
            let p = artifact.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
            let lib_path = if p.is_empty() { maven_to_path(name) } else { p };
            let lib_url = artifact.get("url")
                .and_then(|u| u.as_str())
                .filter(|u| !u.is_empty())
                .map(|u| u.to_string())
                .unwrap_or_else(|| format!("{}{}", maven_base, lib_path));
            (lib_url, lib_path)
        } else {
            let path = maven_to_path(name);
            let base = lib_val.get("url")
                .and_then(|u| u.as_str())
                .filter(|u| !u.is_empty())
                .unwrap_or(maven_base);
            let base = if base.ends_with('/') { base.to_string() } else { format!("{}/", base) };
            (format!("{}{}", base, path), path.to_string())
        }
    } else {
        let path = maven_to_path(name);
        let base = lib_val.get("url")
            .and_then(|u| u.as_str())
            .filter(|u| !u.is_empty())
            .unwrap_or(maven_base);
        let base = if base.ends_with('/') { base.to_string() } else { format!("{}/", base) };
        (format!("{}{}", base, path), path.to_string())
    };

    let lib_path = libraries_dir.join(&path_str);
    if lib_path.exists() { return Ok(()); }

    // If URL is empty, the file is embedded in the installer JAR. Don't download.
    if url.is_empty() || url == maven_base {
        return Ok(());
    }

    if let Some(parent) = lib_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    crate::download::download_file(&url, &lib_path, "").await.map_err(|e| {
        LauncherError::ModLoader(format!("Failed to download processor library {}: {}", name, e))
    })
}

/// Run the Forge installer processors (Forge 1.13+) by spawning Java processes
/// for each processor step.
///
/// All required libraries must already be downloaded to `libraries_dir` before
/// calling this function. Embedded files (universal jar, client.lzma) must also
/// have been extracted.
///
/// `data_base` is the maven directory of the loader inside `libraries_dir`
/// ("net/minecraftforge/forge" for Forge, "net/neoforged/neoforge" for
/// NeoForge 21.x); installer data files extracted from the JAR live under
/// `<data_base>/_data/...`.
///
/// Returns Ok(()) if all processors completed successfully and output JARs exist.
pub(crate) fn run_forge_processors(
    install_profile: &serde_json::Value,
    installer_path: &Path,
    java_path: &Path,
    libraries_dir: &Path,
    mc_version: &str,
    client_jar: &Path,
    root_dir: &Path,
    data_base: &str,
) -> Result<()> {
    let processors = install_profile.get("processors")
        .and_then(|p| p.as_array())
        .ok_or_else(|| LauncherError::ModLoader("No processors in install_profile.json".into()))?;

    // Build variable map
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("SIDE".to_string(), "client".to_string());
    vars.insert("MINECRAFT_JAR".to_string(), client_jar.to_string_lossy().to_string());
    vars.insert("MINECRAFT_VERSION".to_string(), mc_version.to_string());
    vars.insert("ROOT".to_string(), root_dir.to_string_lossy().to_string());
    vars.insert("INSTALLER".to_string(), installer_path.to_string_lossy().to_string());
    vars.insert("LIBRARY_DIR".to_string(), libraries_dir.to_string_lossy().to_string());

    // Add data entries to vars
    if let Some(data_map) = install_profile.get("data").and_then(|d| d.as_object()) {
        for (key, entry) in data_map {
            let client_val = entry.get("client").and_then(|c| c.as_str()).unwrap_or("");
            if client_val.starts_with('[') {
                // Maven artifact reference - resolve to local filesystem path
                vars.insert(key.clone(), resolve_artifact_ref(client_val, libraries_dir).to_string_lossy().to_string());
            } else if client_val.starts_with('/') {
                // Path inside installer JAR - use extracted location in libraries dir
                let jar_path = client_val.trim_start_matches('/');
                let fname = Path::new(jar_path).file_name().and_then(|n| n.to_str()).unwrap_or("data");
                let data_path = libraries_dir.join(data_base)
                    .join("_data").join(&key.to_lowercase()).join(fname);
                vars.insert(key.clone(), data_path.to_string_lossy().to_string());
            } else {
                // Plain string (e.g. SHA hash in quotes) - use as-is, strip surrounding quotes
                let clean = client_val.trim_matches('\'').trim_matches('"').to_string();
                vars.insert(key.clone(), clean);
            }
        }
    }

    tracing::info!(target: "launcher", "Running {} Forge processors", processors.len());

    for (idx, proc_val) in processors.iter().enumerate() {
        // Check sides
        let sides = proc_val.get("sides")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        if !sides.is_empty() && !sides.iter().any(|s| *s == "client") {
            tracing::debug!(target: "launcher", "Skipping processor {} (not client side)", idx);
            continue;
        }

        let jar_artifact = proc_val.get("jar").and_then(|j| j.as_str()).unwrap_or("");
        if jar_artifact.is_empty() {
            tracing::warn!(target: "launcher", "Processor {} has no jar, skipping", idx);
            continue;
        }

        // Resolve the main processor jar path
        let jar_path = resolve_artifact_ref(&format!("[{}]", jar_artifact), libraries_dir);
        if !jar_path.exists() {
            tracing::warn!(target: "launcher", "Processor JAR not found: {} (resolved to {}), skipping", jar_artifact, jar_path.display());
            continue;
        }

        let main_class = match get_jar_main_class(&jar_path) {
            Some(cls) => cls,
            None => {
                tracing::warn!(target: "launcher", "No Main-Class in {} manifest, skipping", jar_path.display());
                continue;
            }
        };

        // Build classpath: processor jar + all classpath dependencies
        let classpath_artifacts = proc_val.get("classpath")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        let mut classpath_parts: Vec<String> = Vec::new();
        let all_refs = std::iter::once(jar_artifact).chain(classpath_artifacts.iter().copied());
        for artifact_ref in all_refs {
            let ref_str = if artifact_ref.starts_with('[') {
                artifact_ref.to_string()
            } else {
                format!("[{}]", artifact_ref)
            };
            let local_path = resolve_artifact_ref(&ref_str, libraries_dir);
            if local_path.exists() {
                classpath_parts.push(local_path.to_string_lossy().to_string());
            } else {
                tracing::warn!(target: "launcher", "Processor dependency not found: {} (resolved to {})", artifact_ref, local_path.display());
            }
        }

        if classpath_parts.is_empty() {
            tracing::warn!(target: "launcher", "Processor {} has no valid classpath, skipping", idx);
            continue;
        }

        let classpath = classpath_parts.join(";");

        // Build args with variable substitution
        let raw_args = proc_val.get("args")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        let mut processed_args: Vec<String> = Vec::new();
        for raw_arg in &raw_args {
            let resolved = if raw_arg.starts_with('[') && raw_arg.ends_with(']') {
                resolve_artifact_ref(raw_arg, libraries_dir).to_string_lossy().to_string()
            } else {
                let mut arg = raw_arg.to_string();
                for (key, val) in &vars {
                    arg = arg.replace(&format!("{{{}}}", key), val);
                }
                arg
            };
            processed_args.push(resolved);
        }

        tracing::info!(target: "launcher", "Processor {}: {} main={}", idx, jar_path.display(), main_class);
        tracing::debug!(target: "launcher", "  classpath ({}): {}", classpath_parts.len(), classpath);
        tracing::debug!(target: "launcher", "  args: {:?}", processed_args);

        // Spawn Java process for this processor
        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = std::process::Command::new(java_path);
            c.creation_flags(0x08000000);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut cmd = std::process::Command::new(java_path);

        let output = cmd
            .arg("-cp")
            .arg(&classpath)
            .arg(&main_class)
            .args(&processed_args)
            .output()
            .map_err(|e| LauncherError::ModLoader(format!("Failed to run processor {}: {}", idx, e)))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            let detail = if stderr.trim().is_empty() { &stdout[..stdout.len().min(500)] } else { &stderr[..stderr.len().min(500)] };
            return Err(LauncherError::ModLoader(
                format!("Forge processor {} failed (exit: {}): {}", idx, output.status, detail)
            ));
        }

        if !stderr.trim().is_empty() {
            tracing::debug!(target: "launcher", "  stderr: {}", &stderr[..stderr.len().min(200)]);
        }
        tracing::info!(target: "launcher", "Processor {} completed", idx);
    }

    Ok(())
}

fn build_profile(
    main_class: String,
    libs: Vec<ForgeLibrary>,
    arguments: Option<ForgeArguments>,
    minecraft_arguments: Option<String>,
) -> Result<LoaderProfile> {
    let mut libraries = Vec::new();
    for lib in libs {
        let (url, path) = if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let lib_path = artifact.path.clone().unwrap_or_else(|| maven_to_path(&lib.name));
                // An explicitly empty URL means the artifact is embedded in the
                // installer JAR (modern Forge ships its runtime jar that way)
                // and must be extracted from it, not downloaded from maven.
                let lib_url = match artifact.url.as_ref() {
                    Some(u) if u.is_empty() => String::new(),
                    Some(u) => u.clone(),
                    None => format!("https://maven.minecraftforge.net/{}", lib_path),
                };
                (lib_url, lib_path)
            } else {
                let path = maven_to_path(&lib.name);
                let base = lib.url.as_ref()
                    .filter(|u| !u.is_empty())
                    .cloned()
                    .unwrap_or_else(|| "https://maven.minecraftforge.net/".to_string());
                let base = if base.ends_with('/') { base } else { format!("{}/", base) };
                (format!("{}{}", base, path), path)
            }
        } else {
            let path = maven_to_path(&lib.name);
            let base = lib.url.as_ref()
                .filter(|u| !u.is_empty())
                .cloned()
                .unwrap_or_else(|| "https://maven.minecraftforge.net/".to_string());
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

    // For MC <= 1.12.2 the Forge profile carries its game arguments in the
    // legacy `minecraftArguments` string, which contains the FULL argument
    // list (including tweakClass and the standard Mojang tokens) and must
    // REPLACE the vanilla game arguments. Newer profiles use
    // `arguments.game` which only adds extra args on top of vanilla.
    let (game_args, jvm_args, legacy_args) = if let Some(args) = arguments {
        let game = args.game.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>();
        if !game.is_empty() {
            let jvm = args.jvm.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            (game, jvm, false)
        } else if let Some(mc_args) = &minecraft_arguments {
            let all = mc_args.split_whitespace().map(String::from).collect::<Vec<_>>();
            (all, Vec::new(), true)
        } else {
            (Vec::new(), Vec::new(), false)
        }
    } else if let Some(mc_args) = &minecraft_arguments {
        let all = mc_args.split_whitespace().map(String::from).collect::<Vec<_>>();
        (all, Vec::new(), true)
    } else {
        (Vec::new(), Vec::new(), false)
    };

    let mut jvm_args = jvm_args;
    // Legacy Forge (MC <= 1.12.2) needs these flags or FML aborts with
    // signature/launch errors. The official launcher adds them for Forge.
    if legacy_args {
        for flag in [
            "-Dfml.ignoreInvalidMinecraftCertificates=true",
            "-Dfml.ignorePatchDiscrepancies=true",
        ] {
            if !jvm_args.iter().any(|a| a.starts_with(flag)) {
                jvm_args.push(flag.to_string());
            }
        }
    }

    Ok(LoaderProfile {
        main_class,
        libraries,
        jvm_args,
        game_args,
        legacy_args,
    })
}

/// Get Forge launch profile by extracting from the installer JAR.
pub async fn get_profile(mc_version: &str, forge_version: &str) -> Result<LoaderProfile> {
    let installer_path = download_installer(mc_version, forge_version).await?;

    let jar_bytes = std::fs::read(&installer_path)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to read Forge installer JAR: {}", e)))?;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid Forge installer JAR: {}", e)))?;

    // Try version.json first (processor-based installer, MC 1.13+)
    if let Some(version_str) = read_entry_from_jar(&mut archive, "version.json") {
        #[derive(Deserialize)]
        struct MinecraftVersionJson {
            #[serde(rename = "mainClass", default)]
            main_class: Option<String>,
            #[serde(default)]
            libraries: Vec<ForgeLibrary>,
            #[serde(default)]
            arguments: Option<ForgeArguments>,
            #[serde(rename = "minecraftArguments", default)]
            minecraft_arguments: Option<String>,
        }
        if let Ok(ver) = serde_json::from_str::<MinecraftVersionJson>(&version_str) {
            if let Some(main_class) = ver.main_class {
                if !ver.libraries.is_empty() {
                    return build_profile(main_class, ver.libraries, ver.arguments, ver.minecraft_arguments);
                }
            }
        }
    }

    // Fall back to install_profile.json (legacy standalone installer)
    let install_profile_str = read_entry_from_jar(&mut archive, "install_profile.json")
        .ok_or_else(|| LauncherError::ModLoader("Forge installer JAR has no version.json or install_profile.json".into()))?;

    let mut profile: ForgeInstallProfile = serde_json::from_str(&install_profile_str)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e)))?;

    // If processor-based, try version.json referenced by the json field
    if profile.main_class.is_none() || profile.libraries.is_empty() {
        if let Some(json_path) = &profile.json {
            if let Some(version_str) = read_entry_from_jar(&mut archive, json_path) {
                if let Ok(version_profile) = serde_json::from_str::<ForgeInstallProfile>(&version_str) {
                    if profile.main_class.is_none() {
                        profile.main_class = version_profile.main_class;
                    }
                    if profile.libraries.is_empty() {
                        profile.libraries = version_profile.libraries;
                    }
                    if profile.arguments.is_none() {
                        profile.arguments = version_profile.arguments;
                    }
                    if profile.minecraft_arguments.is_none() {
                        profile.minecraft_arguments = version_profile.minecraft_arguments;
                    }
                }
            }
        }
    }

    // Legacy installers (MC 1.8.x and older) keep the launch data in a
    // nested `versionInfo` object instead of the top level.
    if profile.main_class.is_none() || profile.libraries.is_empty() {
        if let Ok(root) = serde_json::from_str::<serde_json::Value>(&install_profile_str) {
            if let Some(version_info) = root.get("versionInfo") {
                if let Ok(vi) = serde_json::from_value::<ForgeInstallProfile>(version_info.clone()) {
                    if profile.main_class.is_none() {
                        profile.main_class = vi.main_class;
                    }
                    if profile.libraries.is_empty() {
                        profile.libraries = vi.libraries;
                    }
                    if profile.arguments.is_none() {
                        profile.arguments = vi.arguments;
                    }
                    if profile.minecraft_arguments.is_none() {
                        profile.minecraft_arguments = vi.minecraft_arguments;
                    }
                }
            }
        }
    }

    let main_class = profile.main_class
        .ok_or_else(|| LauncherError::ModLoader("No mainClass found in Forge installer".into()))?;

    build_profile(main_class, profile.libraries, profile.arguments, profile.minecraft_arguments)
}

/// Check if MC version needs processor JARs (MC >= 1.13).
fn mc_needs_processor_jars(mc_version: &str) -> bool {
    let parts: Vec<&str> = mc_version.split('.').collect();
    if parts.len() >= 2 {
        if let (Ok(major), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            return major > 1 || (major == 1 && minor >= 13);
        }
    }
    true
}

/// Ensure Forge processor-generated JARs exist, running the processor pipeline
/// if needed.
pub async fn ensure_processor_jars(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
) -> Result<()> {
    if !mc_needs_processor_jars(mc_version) {
        tracing::debug!(target: "launcher", "MC {} doesn't need processor JARs, skipping", mc_version);
        return Ok(());
    }

    let libraries_dir = libraries_dir.to_path_buf();
    let versions_dir = versions_dir.to_path_buf();

    // Build expected output paths
    let full_version = if loader_version.contains('-') {
        loader_version.to_string()
    } else {
        format!("{}-{}", mc_version, loader_version)
    };

    // Processor outputs are written into the timestamped version dir
    // (e.g. 1.16.5-20210115.111550), not the plain MC version one —
    // scan whatever version dirs exist under net/minecraft/client/.
    let client_scan_dir = libraries_dir.join("net").join("minecraft").join("client");
    let mut client_version_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&client_scan_dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                client_version_dirs.push(e.path());
            }
        }
    }
    let jar_exists = |mc: &str, suffix: &str| -> bool {
        client_version_dirs.iter().any(|d| {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            d.join(format!("client-{}-{}.jar", name, suffix)).exists()
                || d.join(format!("client-{}-{}.jar", mc, suffix)).exists()
        })
    };
    let slim_ok = jar_exists(mc_version, "slim");
    let extra_ok = jar_exists(mc_version, "extra");
    let srg_ok = jar_exists(mc_version, "srg");
    let forge_client_path = libraries_dir
        .join("net").join("minecraftforge").join("forge")
        .join(&full_version).join(format!("forge-{}-client.jar", &full_version));

    let client_ok = forge_client_path.exists();

    if slim_ok && extra_ok && client_ok && srg_ok {
        tracing::debug!(target: "launcher", "All Forge processor JARs present");
        return Ok(());
    }

    tracing::info!(target: "launcher", "Forge processor JARs missing (slim={}, extra={}, srg={}, client={}), running processors...",
        slim_ok, extra_ok, srg_ok, client_ok);

    // Download the installer JAR
    let installer_path = download_installer(mc_version, loader_version).await?;

    // Read install_profile.json
    let jar_bytes = std::fs::read(&installer_path)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to read installer JAR: {}", e)))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid installer JAR: {}", e)))?;

    let install_profile_str = read_entry_from_jar(&mut archive, "install_profile.json")
        .ok_or_else(|| LauncherError::ModLoader("install_profile.json not found in installer JAR".into()))?;

    let install_profile: serde_json::Value = serde_json::from_str(&install_profile_str)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e)))?;

    drop(archive); // Release the read lock on jar_bytes

    // Check if this is a processor-based installer
    let has_processors = install_profile.get("processors")
        .and_then(|p| p.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    if !has_processors {
        tracing::debug!(target: "launcher", "No processors in install_profile.json, nothing to do");
        std::fs::remove_file(&installer_path).ok();
        return Ok(());
    }

    // --- Step 1: Download all processor libraries ---
    tracing::info!(target: "launcher", "Downloading Forge processor libraries...");
    if let Some(libraries) = install_profile.get("libraries").and_then(|l| l.as_array()) {
        for lib_val in libraries {
            if let Err(e) = download_processor_lib(lib_val, &libraries_dir, "https://maven.minecraftforge.net/").await {
                tracing::warn!(target: "launcher", "Processor library download warning: {}", e);
            }
        }
    }

    // --- Step 2: Extract embedded files from the installer JAR ---
    tracing::info!(target: "launcher", "Extracting embedded files from installer...");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Failed to reopen installer JAR: {}", e)))?;

    // Extract embedded JARs (forge jar with empty URL)
    extract_embedded_forge_jars(&mut archive, &libraries_dir);

    // Extract data files (like client.lzma) from the installer JAR
    if let Some(data_map) = install_profile.get("data").and_then(|d| d.as_object()) {
        for (key, entry) in data_map {
            let client_val = entry.get("client").and_then(|c| c.as_str()).unwrap_or("");
            if client_val.starts_with('/') {
                let jar_path = client_val.trim_start_matches('/');
                let fname = Path::new(jar_path).file_name().and_then(|n| n.to_str()).unwrap_or("data");
                let dest = libraries_dir.join("net").join("minecraftforge").join("forge")
                    .join("_data").join(&key.to_lowercase()).join(fname);
                if !dest.exists() {
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    if extract_entry_from_jar(&mut archive, jar_path, &dest) {
                        tracing::debug!(target: "launcher", "Extracted data {} -> {}", jar_path, dest.display());
                    } else {
                        tracing::warn!(target: "launcher", "Failed to extract data file: {}", jar_path);
                    }
                }
            }
        }
    }

    drop(archive); // Release the JAR bytes

    // --- Step 3: Locate client JAR ---
    let vd = versions_dir.join(mc_version);
    let client_jar = if vd.join("client.jar").exists() {
        vd.join("client.jar")
    } else {
        vd.join(format!("{}.jar", mc_version))
    };
    if !client_jar.exists() {
        std::fs::remove_file(&installer_path).ok();
        return Err(LauncherError::ModLoader(format!("Client JAR not found at {:?}", client_jar)));
    }

    let root_dir = versions_dir.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| versions_dir.to_path_buf());

    // --- Step 4: Run the processor pipeline ---
    // libraries_dir is always `<data_dir>/libraries` (see config.rs),
    // so its parent is the app data dir used to locate managed runtimes.
    let data_dir = libraries_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir());
    if let Some(java_path) = find_java_for(&data_dir, mc_version) {
        tracing::info!(target: "launcher", "Running Forge processors with Java: {:?}", java_path);
        if let Err(e) = run_forge_processors(
            &install_profile,
            &installer_path,
            &java_path,
            &libraries_dir,
            mc_version,
            &client_jar,
            &root_dir,
            "net/minecraftforge/forge",
        ) {
            tracing::warn!(target: "launcher", "Forge processor run failed: {}", e);
        }
    } else {
        tracing::warn!(target: "launcher", "Java not found — cannot run Forge processors");
    }

    std::fs::remove_file(&installer_path).ok();

    // --- Step 5: Verify outputs ---
    // Outputs live in the timestamped version dir (1.16.5-20210115.111550),
    // so scan whatever version dirs exist rather than assuming the plain
    // MC version path.
    let client_scan_dir = libraries_dir.join("net").join("minecraft").join("client");
    let mut client_version_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&client_scan_dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                client_version_dirs.push(e.path());
            }
        }
    }
    let scan_jar = |suffix: &str| -> Option<PathBuf> {
        client_version_dirs.iter().find_map(|d| {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let p = d.join(format!("client-{}-{}.jar", name, suffix));
            if p.exists() { return Some(p); }
            let p2 = d.join(format!("client-{}-{}.jar", mc_version, suffix));
            if p2.exists() { return Some(p2); }
            None
        })
    };
    let slim_path = scan_jar("slim");
    let extra_path = scan_jar("extra");
    let srg_path = scan_jar("srg");
    let slim_ok = slim_path.is_some();
    let extra_ok = extra_path.is_some();
    let srg_ok = srg_path.is_some();
    let client_ok = forge_client_path.exists();

    tracing::debug!(target: "launcher", "Post-processor JAR check: slim={} extra={} srg={} client={}",
        slim_ok, extra_ok, srg_ok, client_ok);
    tracing::debug!(target: "launcher", "  slim={:?}", slim_path);
    tracing::debug!(target: "launcher", "  extra={:?}", extra_path);
    tracing::debug!(target: "launcher", "  srg={:?}", srg_path);
    tracing::debug!(target: "launcher", "  forge_client={:?}", forge_client_path);

    // Log what exists in the client dir for debugging
    if client_base_exists(&libraries_dir) {
        tracing::debug!(target: "launcher", "Contents of net/minecraft/client/:");
        log_dir_entries(&libraries_dir.join("net").join("minecraft").join("client"));
    }
    log_dir_entries(&libraries_dir.join("net").join("minecraftforge").join("forge").join(&full_version));

    if !slim_ok || !extra_ok || !client_ok {
        return Err(LauncherError::ModLoader(
            "Forge processors did not generate required output JARs".into()
        ));
    }

    tracing::info!(target: "launcher", "Forge processor JARs verified");
    Ok(())
}

fn client_base_exists(libraries_dir: &Path) -> bool {
    libraries_dir.join("net").join("minecraft").join("client").exists()
}

/// Extract the Forge runtime JAR(s) embedded in the installer into the
/// libraries dir. Modern Forge (1.13+) ships its runtime jar inside the
/// installer with an empty `url` in version.json; a stale/wrong jar from an
/// earlier maven download must not shadow the embedded copy.
fn extract_embedded_forge_jars<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    libraries_dir: &Path,
) {
    let embedded_maven_base = "maven/net/minecraftforge/forge";
    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            let entry_name = entry.name().replace('\\', "/");
            if entry_name.starts_with(embedded_maven_base) && entry_name.ends_with(".jar") {
                // Extract to libraries dir maintaining the path after "maven/"
                let relative_path = entry_name.strip_prefix("maven/").unwrap_or(&entry_name);
                let dest = libraries_dir.join(relative_path);
                let dest_len = dest.metadata().map(|m| m.len()).unwrap_or(0);
                // Re-extract (overwrite) when the existing file is missing or
                // differs in size — a stale/wrong jar from an earlier run
                // (e.g. a maven download) must not shadow the embedded one.
                if !dest.exists() || entry.size() != dest_len {
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    use std::io::Write;
                    if let Ok(mut file) = std::fs::File::create(&dest) {
                        let mut buf = Vec::new();
                        if entry.read_to_end(&mut buf).is_ok() && file.write_all(&buf).is_ok() {
                            tracing::debug!(target: "launcher", "Extracted {} -> {}", entry_name, dest.display());
                        }
                    }
                }
            }
        }
    }
}

fn log_dir_entries(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                tracing::debug!(target: "launcher", "    {}/", name);
                if let Ok(inner) = std::fs::read_dir(e.path()) {
                    for f in inner.flatten() {
                        let fname = f.file_name().to_string_lossy().to_string();
                        tracing::debug!(target: "launcher", "      {} ({} bytes)", fname, f.metadata().map(|m| m.len()).unwrap_or(0));
                    }
                }
            } else {
                tracing::debug!(target: "launcher", "    {} ({} bytes)", name, e.metadata().map(|m| m.len()).unwrap_or(0));
            }
        }
    }
}

/// Install Forge — download libraries and run installer to generate processor files.
pub async fn install(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
) -> Result<LoaderProfile> {
    tracing::info!(target: "launcher", "Installing Forge for MC {} (loader {})", mc_version, loader_version);

    let profile = get_profile(mc_version, loader_version).await?;

    for lib in &profile.libraries {
        // Empty URL = artifact embedded in the installer JAR; it is extracted
        // by ensure_processor_jars, so don't try to download it.
        if lib.url.is_empty() {
            tracing::debug!(target: "launcher", "Skipping download of {} (embedded in installer)", lib.name);
            continue;
        }
        let lib_path = libraries_dir.join(&lib.path);
        if !lib_path.exists() {
            match crate::download::download_file(&lib.url, &lib_path, "").await {
                Ok(()) => {},
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("404") && lib.name.starts_with("net.minecraftforge:forge:") {
                        // Modern Forge (MC 1.13+) publishes the runtime jar as
                        // `forge-{maven_version}-universal.jar` in the same
                        // directory, but newer install profiles reference a
                        // `-client` classifier that no longer exists on maven.
                        let version_dir = Path::new(&lib.path).parent()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_string();
                        let universal_name = if version_dir.is_empty() {
                            None
                        } else {
                            Some(format!("forge-{}-universal.jar", version_dir))
                        };
                        let universal_url = if let Some(name) = &universal_name {
                            lib.url.rsplit_once('/')
                                .map(|(base, _)| format!("{}/{}", base, name))
                                .unwrap_or_else(|| lib.url.clone())
                        } else {
                            lib.url.replace(".jar", "-universal.jar")
                        };
                        let universal_path = if let Some(name) = &universal_name {
                            PathBuf::from(
                                Path::new(&lib.path).parent()
                                    .map(|p| p.join(name))
                                    .unwrap_or_else(|| PathBuf::from(&lib.path))
                            )
                        } else {
                            PathBuf::from(lib.path.replace(".jar", "-universal.jar"))
                        };
                        let universal_full = libraries_dir.join(&universal_path);
                        if !universal_full.exists() {
                            tracing::debug!(target: "launcher", "Trying universal fallback for {}", lib.name);
                            if let Err(e2) = crate::download::download_file(&universal_url, &universal_full, "").await {
                                tracing::warn!(target: "launcher", "Failed to download Forge library {} (both paths): {}", lib.name, e2);
                                return Err(e);
                            }
                            std::fs::copy(&universal_full, &lib_path).ok();
                        }
                    } else {
                        tracing::warn!(target: "launcher", "Failed to download Forge library {}: {}", lib.name, err_str);
                        return Err(e);
                    }
                }
            }
        }
    }

    // Modern Forge (1.13+) embeds its runtime JAR(s) in the installer with an
    // empty `url` in version.json. Extract them BEFORE ensure_processor_jars,
    // which may skip work (and thus extraction) when processor JARs already
    // exist, and must not leave a stale maven download shadowing them.
    let extraction_installer = download_installer(mc_version, loader_version).await?;
    if let Ok(jar_bytes) = std::fs::read(&extraction_installer) {
        if let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(&jar_bytes)) {
            extract_embedded_forge_jars(&mut archive, libraries_dir);
        }
    }
    std::fs::remove_file(&extraction_installer).ok();

    ensure_processor_jars(mc_version, loader_version, libraries_dir, versions_dir).await?;

    tracing::info!(target: "launcher", "Forge install completed for MC {}", mc_version);
    Ok(profile)
}

use super::{LoaderLibrary, LoaderProfile, LoaderVersionPage};
use crate::error::{LauncherError, Result};
use crate::integrity::HashSpec;
use crate::versions::maven_to_path;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

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
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    sha512: Option<String>,
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
    super::prism_meta::fetch_loader_versions("net.minecraftforge", Some(mc_version), offset, limit)
        .await
}

/// Download the Forge installer JAR, return path.
async fn download_installer(mc_version: &str, forge_version: &str) -> Result<PathBuf> {
    super::validate_loader_request(mc_version, forge_version)?;
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
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            full_version, full_version
        ),
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            legacy_full_version, legacy_full_version
        ),
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            full_version, forge_version
        ),
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            forge_version, forge_version
        ),
    ];

    let installer_path = std::env::temp_dir().join(format!("forge-{}-installer.jar", full_version));

    // A cached installer is accepted only after its checksum has been
    // verified. The first candidate whose repository sidecar is available
    // wins; a 404 candidate simply falls through to the next legacy layout.
    super::artifact::download_verified_installer(
        &installer_urls,
        &installer_path,
        super::artifact::FORGE_HOSTS,
        &format!("Forge installer {full_version}"),
    )
    .await?;

    // The checksum proves the exact Maven bytes; the structural check is an
    // additional guard against an installer that is authentic but unusable.
    if !installer_jar_is_valid(&installer_path) {
        let _ = std::fs::remove_file(&installer_path);
        return Err(LauncherError::ModLoader(format!(
            "Downloaded Forge installer {full_version} is not a valid installer archive"
        )));
    }
    Ok(installer_path)
}

/// A cached installer is usable only if it opens as a ZIP and carries one of
/// the metadata files the profile/processor code expects. Size alone is not
/// enough: a truncated download or an HTML error page can easily exceed 1 KB.
pub(crate) fn installer_jar_is_valid(path: &Path) -> bool {
    let ok_meta = std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() > 1000)
        .unwrap_or(false);
    if !ok_meta {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return false;
    };
    archive.by_name("install_profile.json").is_ok() || archive.by_name("version.json").is_ok()
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

/// Pick a Java for the Forge installer processor pipeline.
///
/// Processors are version-sensitive in BOTH directions: old ForgeGradle
/// dies on a too-new JVM with cryptic "Unsupported class file major
/// version" / "did not generate" errors, while new installers need at least
/// their required major. So an EXACT major is required below 21, and 21 is
/// treated as a minimum (modern installers run on 22+). The old
/// closest-higher matching handed Java 21 to 1.16-era ForgeGradle. Falls
/// back to any system Java only when nothing suitable is installed.
pub(crate) fn find_java_for(data_dir: &Path, mc_version: &str) -> Option<PathBuf> {
    let want = required_forge_java_major(mc_version);
    // Processors need their own Java (often newer than the game launch uses)
    // and — uniquely in this codebase — may run on the closest NEWER major,
    // but only for wants of Java 21+. This requirement is the Forge-order
    // one and is never mixed with the game's JVM choice.
    let req = crate::java::JavaRequirement {
        major: want,
        arch: None,
        require_64bit: true,
        allow_newer: want >= 21,
    };
    let all = crate::launch::find_all_java_installations(data_dir);
    match crate::java::resolve_java(&req, None, None, &all) {
        crate::java::JavaChoice::Selected { install } => {
            tracing::debug!(target: "launcher", "Using Java {} (v{}) for Forge processors (want {})",
                install.major_version, install.version, want);
            Some(install.path)
        }
        _ => None,
    }
}

/// Convert a Maven artifact coordinate `group:name:version[:classifier][@ext]`
/// to a local filesystem path under libraries_dir.
pub(crate) fn artifact_to_path(artifact: &str, libraries_dir: &Path, ext: &str) -> PathBuf {
    let parts: Vec<&str> = artifact.split(':').collect();
    if parts.len() < 3 {
        return PathBuf::new();
    }
    let group_path = parts[0].replace('.', "/");
    let artifact_name = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).copied().unwrap_or("");
    // Install-profile coordinates are remote input. A component containing
    // `..`, a separator or a drive prefix must never be joined into the
    // libraries directory; return an empty path so the caller fails clearly.
    let component_ok =
        |value: &str| value.is_empty() || crate::import::check_safe_file_name(value).is_ok();
    if crate::import::check_safe_remote_path(&group_path).is_err()
        || !component_ok(artifact_name)
        || !component_ok(version)
        || !component_ok(classifier)
        || !component_ok(ext)
    {
        tracing::warn!(target: "launcher", "Unsafe artifact coordinate: {}", artifact);
        return PathBuf::new();
    }
    let filename = if classifier.is_empty() {
        format!("{}-{}.{}", artifact_name, version, ext)
    } else {
        format!("{}-{}-{}.{}", artifact_name, version, classifier, ext)
    };
    libraries_dir
        .join(&group_path)
        .join(artifact_name)
        .join(version)
        .join(filename)
}

/// Parse an artifact reference like `[group:name:version:classifier@ext]`
/// and resolve to a local path under libraries_dir.
pub(crate) fn resolve_artifact_ref(ref_str: &str, libraries_dir: &Path) -> PathBuf {
    let inner = ref_str.trim_start_matches('[').trim_end_matches(']');
    let (coord, ext) = if let Some(at_pos) = inner.rfind('@') {
        (&inner[..at_pos], &inner[at_pos + 1..])
    } else {
        (inner, "jar")
    };
    artifact_to_path(coord, libraries_dir, ext)
}

/// Read a text entry from inside a JAR (zip) archive.
pub(crate) fn read_entry_from_jar<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            let Some(entry_name) = entry.name().ok().map(|name| name.replace('\\', "/")) else {
                continue;
            };
            if entry_name == name || entry_name.ends_with(&format!("/{}", name)) {
                let mut content = String::new();
                entry.read_to_string(&mut content).ok()?;
                return Some(content);
            }
        }
    }
    None
}

/// Read only `install_profile.json` from a cached installer JAR.
///
/// Used for local fast-path checks that must not turn a healthy launch into a
/// network request. The caller still verifies the installer before relying on
/// it for install/repair.
pub(crate) fn read_install_profile_from_installer(
    installer_path: &Path,
) -> Option<serde_json::Value> {
    let file = std::fs::File::open(installer_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let text = read_entry_from_jar(&mut archive, "install_profile.json")?;
    serde_json::from_str(&text).ok()
}

/// Extract a binary entry from inside a JAR (zip) archive to a destination path.
pub(crate) fn extract_entry_from_jar<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    dest: &Path,
) -> bool {
    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            let Some(entry_name) = entry.name().ok().map(|name| name.replace('\\', "/")) else {
                continue;
            };
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
    allowed_hosts: &[&str],
) -> Result<()> {
    let name = lib_val.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if name.is_empty() {
        return Ok(());
    }

    let artifact = lib_val
        .get("downloads")
        .and_then(|downloads| downloads.get("artifact"));
    let (url, path_str, sha1, sha256, sha512, size) = if let Some(artifact) = artifact {
        let path_str = artifact
            .get("path")
            .and_then(|p| p.as_str())
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| maven_to_path(name));
        let url = artifact.get("url").and_then(|u| u.as_str()).unwrap_or("");
        // An empty artifact URL means the file is embedded in the installer.
        if url.is_empty() {
            return Ok(());
        }
        (
            url.to_string(),
            path_str,
            artifact
                .get("sha1")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_string),
            artifact
                .get("sha256")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_string),
            artifact
                .get("sha512")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_string),
            artifact.get("size").and_then(|v| v.as_u64()),
        )
    } else {
        let path_str = maven_to_path(name);
        let base = lib_val
            .get("url")
            .and_then(|u| u.as_str())
            .filter(|u| !u.is_empty())
            .unwrap_or(maven_base);
        let base = if base.ends_with('/') {
            base.to_string()
        } else {
            format!("{}/", base)
        };
        (
            format!("{}{}", base, path_str),
            path_str,
            None,
            None,
            None,
            None,
        )
    };

    if crate::import::check_safe_remote_path(&path_str).is_err() {
        return Err(LauncherError::ModLoader(format!(
            "Unsafe processor library path: {}",
            path_str
        )));
    }

    let source = super::artifact::has_metadata_checksum(
        sha1.as_deref(),
        sha256.as_deref(),
        sha512.as_deref(),
    )
    .then(|| "metadata".to_string());
    let mut lib = LoaderLibrary {
        name: name.to_string(),
        url,
        path: path_str,
        sha1,
        sha256,
        sha512,
        checksum_source: source,
        size,
    };
    super::artifact::ensure_library(&mut lib, libraries_dir, allowed_hosts, "processor")
        .await
        .map(|_| ())
        .map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to verify processor library {}: {}",
                name, e
            ))
        })
}

/// Inputs for the Forge/NeoForge installer processor pipeline.
pub(crate) struct ForgeProcessorInputs<'a> {
    pub install_profile: &'a serde_json::Value,
    pub installer_path: &'a Path,
    pub java_path: &'a Path,
    pub libraries_dir: &'a Path,
    pub mc_version: &'a str,
    pub client_jar: &'a Path,
    pub root_dir: &'a Path,
    pub data_base: &'a str,
    /// Full loader version (e.g. `1.20.1-47.4.20`); scopes the extracted
    /// installer data files under `<data_base>/_data/<data_version>/...` so
    /// different loader versions can never reuse each other's patch files.
    pub data_version: &'a str,
}

/// A processor-generated JAR and the checksum the installer metadata declares
/// for it. Not every generated JAR has a publisher checksum; those without one
/// are still required to be structurally valid.
#[derive(Debug, Clone)]
pub(crate) struct ProcessorOutput {
    pub path: PathBuf,
    pub checksum: Option<HashSpec>,
}

/// Build the variable map used by installer processors and output checks.
// The arguments mirror the installer context one-to-one; grouping them into
// another struct would duplicate `ForgeProcessorInputs` without simplifying
// either caller.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_processor_vars(
    install_profile: &serde_json::Value,
    installer_path: &Path,
    libraries_dir: &Path,
    mc_version: &str,
    client_jar: &Path,
    root_dir: &Path,
    data_base: &str,
    data_version: &str,
) -> HashMap<String, String> {
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("SIDE".to_string(), "client".to_string());
    vars.insert(
        "MINECRAFT_JAR".to_string(),
        client_jar.to_string_lossy().to_string(),
    );
    vars.insert("MINECRAFT_VERSION".to_string(), mc_version.to_string());
    vars.insert("ROOT".to_string(), root_dir.to_string_lossy().to_string());
    vars.insert(
        "INSTALLER".to_string(),
        installer_path.to_string_lossy().to_string(),
    );
    vars.insert(
        "LIBRARY_DIR".to_string(),
        libraries_dir.to_string_lossy().to_string(),
    );

    if let Some(data_map) = install_profile.get("data").and_then(|d| d.as_object()) {
        for (key, entry) in data_map {
            let client_val = entry.get("client").and_then(|c| c.as_str()).unwrap_or("");
            if client_val.starts_with('[') {
                // Maven artifact reference - resolve to local filesystem path
                vars.insert(
                    key.clone(),
                    resolve_artifact_ref(client_val, libraries_dir)
                        .to_string_lossy()
                        .to_string(),
                );
            } else if client_val.starts_with('/') {
                // Path inside installer JAR - use extracted location in libraries dir
                let jar_path = client_val.trim_start_matches('/');
                let fname = Path::new(jar_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("data");
                let data_path = data_file_path(libraries_dir, data_base, data_version, key, fname);
                vars.insert(key.clone(), data_path.to_string_lossy().to_string());
            } else {
                // Plain string (e.g. SHA hash in quotes) - use as-is, strip surrounding quotes
                let clean = client_val.trim_matches('\'').trim_matches('"').to_string();
                vars.insert(key.clone(), clean);
            }
        }
    }
    vars
}

fn resolve_processor_value(raw: &str, vars: &HashMap<String, String>) -> Result<String> {
    let mut value = raw.to_string();
    for (key, replacement) in vars {
        value = value.replace(&format!("{{{}}}", key), replacement);
    }
    if value.contains('{') || value.contains('}') {
        return Err(LauncherError::ModLoader(format!(
            "Processor metadata has an unresolved placeholder: {}",
            truncate_for_error(&value, 300)
        )));
    }
    let trimmed = value.trim();
    let without_quotes = trimmed
        .strip_prefix('\'')
        .and_then(|v| v.strip_suffix('\''))
        .or_else(|| trimmed.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
        .unwrap_or(trimmed);
    Ok(without_quotes.to_string())
}

/// Resolve the processor outputs that are expected for this installer.
///
/// The installer's `data` map names the client-side JARs and, for some of
/// them, a paired `*_SHA` value. Those SHA-1 values are publisher metadata,
/// not a hash computed after generation. `MC_UNPACKED` is server-only and is
/// deliberately excluded.
pub(crate) fn processor_output_plan(
    install_profile: &serde_json::Value,
    vars: &HashMap<String, String>,
    libraries_dir: &Path,
) -> Result<Vec<ProcessorOutput>> {
    let mut outputs: Vec<ProcessorOutput> = Vec::new();
    let Some(data) = install_profile.get("data").and_then(|d| d.as_object()) else {
        return Ok(outputs);
    };

    let mut push = |path: PathBuf, checksum: Option<HashSpec>| -> Result<()> {
        if !path.starts_with(libraries_dir) {
            return Err(LauncherError::ModLoader(format!(
                "Processor output escapes the libraries directory: {}",
                path.display()
            )));
        }
        crate::import::ensure_no_reparse_points(libraries_dir, &path)?;
        if let Some(existing) = outputs.iter_mut().find(|output| output.path == path) {
            if existing.checksum.is_none() {
                existing.checksum = checksum;
            }
            return Ok(());
        }
        outputs.push(ProcessorOutput { path, checksum });
        Ok(())
    };

    for (key, entry) in data {
        if key == "MC_UNPACKED" || entry.get("client").and_then(|v| v.as_str()).is_none() {
            continue;
        }
        let Some(path) = vars.get(key) else {
            continue;
        };
        if path.is_empty() {
            let artifact_ref = entry
                .get("client")
                .and_then(|v| v.as_str())
                .is_some_and(|value| value.starts_with('['));
            if artifact_ref {
                return Err(LauncherError::ModLoader(format!(
                    "Processor output {key} has an unsafe or malformed artifact coordinate"
                )));
            }
            continue;
        }
        if !path.to_ascii_lowercase().ends_with(".jar") {
            continue;
        }
        let checksum = match vars.get(&format!("{key}_SHA")) {
            Some(hash) if !hash.is_empty() => Some(HashSpec::sha1(hash)?),
            _ => None,
        };
        push(PathBuf::from(path), checksum)?;
    }

    // Newer installer profiles may declare outputs directly. Use them too so
    // a renamed output is not silently ignored.
    if let Some(processors) = install_profile.get("processors").and_then(|p| p.as_array()) {
        for processor in processors {
            let Some(outputs_map) = processor.get("outputs").and_then(|o| o.as_object()) else {
                continue;
            };
            for (raw_path, raw_checksum) in outputs_map {
                let Some(raw_checksum) = raw_checksum.as_str() else {
                    continue;
                };
                let path = PathBuf::from(resolve_processor_value(raw_path, vars)?);
                let checksum = resolve_processor_value(raw_checksum, vars)?;
                push(path, Some(HashSpec::sha1(&checksum)?))?;
            }
        }
    }

    Ok(outputs)
}

pub(crate) fn verify_processor_outputs(outputs: &[ProcessorOutput]) -> Result<()> {
    for output in outputs {
        if !is_usable_jar(&output.path) {
            return Err(LauncherError::ModLoader(format!(
                "Processor output is missing or not a valid JAR: {}",
                output.path.display()
            )));
        }
        if let Some(checksum) = &output.checksum {
            if !checksum.verify_file(&output.path)? {
                return Err(LauncherError::ModLoader(format!(
                    "Processor output failed its installer metadata checksum: {}",
                    output.path.display()
                )));
            }
        }
    }
    Ok(())
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
/// `<data_base>/_data/<data_version>/...` (version-scoped, see
/// `data_file_path`).
///
/// Returns Ok(()) if all processors completed successfully and output JARs exist.
pub(crate) fn run_forge_processors(inputs: ForgeProcessorInputs<'_>) -> Result<()> {
    let ForgeProcessorInputs {
        install_profile,
        installer_path,
        java_path,
        libraries_dir,
        mc_version,
        client_jar,
        root_dir,
        data_base,
        data_version,
    } = inputs;
    let processors = install_profile
        .get("processors")
        .and_then(|p| p.as_array())
        .ok_or_else(|| LauncherError::ModLoader("No processors in install_profile.json".into()))?;

    let vars = build_processor_vars(
        install_profile,
        installer_path,
        libraries_dir,
        mc_version,
        client_jar,
        root_dir,
        data_base,
        data_version,
    );
    // Resolve the output list before running anything, so a malformed or
    // escaping output coordinate fails before Java can write to it.
    let expected_outputs = processor_output_plan(install_profile, &vars, libraries_dir)?;

    tracing::info!(target: "launcher", "Running {} Forge processors", processors.len());

    for (idx, proc_val) in processors.iter().enumerate() {
        // Check sides
        let sides = proc_val
            .get("sides")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        if !sides.is_empty() && !sides.contains(&"client") {
            tracing::debug!(target: "launcher", "Skipping processor {} (not client side)", idx);
            continue;
        }

        let jar_artifact = proc_val.get("jar").and_then(|j| j.as_str()).unwrap_or("");
        if jar_artifact.is_empty() {
            // Skipping a step silently produces an incomplete pipeline whose
            // missing output is only discovered (if at all) much later.
            return Err(LauncherError::ModLoader(format!(
                "Processor {idx} has no 'jar' entry in install_profile.json"
            )));
        }

        // Resolve the main processor jar path
        let jar_path = resolve_artifact_ref(&format!("[{}]", jar_artifact), libraries_dir);
        if !jar_path.is_file() {
            return Err(LauncherError::ModLoader(format!(
                "Processor {idx} JAR is missing: {jar_artifact} (resolved to {})",
                jar_path.display()
            )));
        }

        let main_class = get_jar_main_class(&jar_path).ok_or_else(|| {
            LauncherError::ModLoader(format!(
                "Processor {idx} JAR has no Main-Class manifest entry: {}",
                jar_path.display()
            ))
        })?;

        // Build classpath: processor jar + all classpath dependencies
        let classpath_artifacts = proc_val
            .get("classpath")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        let mut classpath_parts: Vec<String> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let all_refs = std::iter::once(jar_artifact).chain(classpath_artifacts.iter().copied());
        for artifact_ref in all_refs {
            let ref_str = if artifact_ref.starts_with('[') {
                artifact_ref.to_string()
            } else {
                format!("[{}]", artifact_ref)
            };
            let local_path = resolve_artifact_ref(&ref_str, libraries_dir);
            if local_path.is_file() {
                classpath_parts.push(local_path.to_string_lossy().to_string());
            } else {
                missing.push(format!("{} -> {}", artifact_ref, local_path.display()));
            }
        }

        if !missing.is_empty() {
            return Err(LauncherError::ModLoader(format!(
                "Processor {idx} classpath is incomplete ({} missing): {}",
                missing.len(),
                missing.join("; ")
            )));
        }
        if classpath_parts.is_empty() {
            return Err(LauncherError::ModLoader(format!(
                "Processor {idx} has an empty classpath in install_profile.json"
            )));
        }

        let classpath = classpath_parts.join(";");

        // Build args with variable substitution
        let raw_args = proc_val
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        let mut processed_args: Vec<String> = Vec::new();
        for raw_arg in &raw_args {
            let resolved = if raw_arg.starts_with('[') && raw_arg.ends_with(']') {
                let path = resolve_artifact_ref(raw_arg, libraries_dir);
                if path.as_os_str().is_empty() {
                    return Err(LauncherError::ModLoader(format!(
                        "Processor {idx} has an unsafe or malformed artifact argument: {raw_arg}"
                    )));
                }
                path.to_string_lossy().to_string()
            } else {
                let mut arg = raw_arg.to_string();
                for (key, val) in &vars {
                    arg = arg.replace(&format!("{{{}}}", key), val);
                }
                if arg.contains('{') || arg.contains('}') {
                    // An unresolved placeholder would be passed to Java
                    // literally and the processor would write to a path
                    // containing braces instead of the requested output.
                    return Err(LauncherError::ModLoader(format!(
                        "Processor {idx} argument has an unresolved placeholder: {}",
                        truncate_for_error(&arg, 300)
                    )));
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

        let output = crate::java::run_command_with_timeout(
            cmd.arg("-cp")
                .arg(&classpath)
                .arg(&main_class)
                .args(&processed_args),
            std::time::Duration::from_secs(600),
        )
        .map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to run processor {} ({}): {}",
                idx,
                if e.kind() == std::io::ErrorKind::TimedOut {
                    "timed out after 10 minutes"
                } else {
                    "spawn error"
                },
                e
            ))
        })?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            let detail = if stderr.trim().is_empty() {
                truncate_for_error(&stdout, 500)
            } else {
                truncate_for_error(&stderr, 500)
            };
            return Err(LauncherError::ModLoader(format!(
                "Forge processor {} failed (exit: {}): {}",
                idx, output.status, detail
            )));
        }

        if !stderr.trim().is_empty() {
            tracing::debug!(target: "launcher", "  stderr: {}", truncate_for_error(&stderr, 200));
        }
        tracing::info!(target: "launcher", "Processor {} completed", idx);
    }

    // A successful process exit is not enough: the installer metadata names
    // the expected client JAR checksums. Verify them before reporting the
    // generated pipeline as usable.
    if let Err(e) = verify_processor_outputs(&expected_outputs) {
        for output in &expected_outputs {
            let _ = std::fs::remove_file(&output.path);
        }
        return Err(e);
    }
    tracing::info!(
        target: "launcher",
        "Processor outputs verified ({} declared checksums, {} structural-only)",
        expected_outputs
            .iter()
            .filter(|output| output.checksum.is_some())
            .count(),
        expected_outputs
            .iter()
            .filter(|output| output.checksum.is_none())
            .count()
    );

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
        let (url, path, sha1, sha256, sha512, checksum_source, size) =
            if let Some(downloads) = &lib.downloads {
                if let Some(artifact) = &downloads.artifact {
                    let lib_path = artifact
                        .path
                        .clone()
                        .unwrap_or_else(|| maven_to_path(&lib.name));
                    // An explicitly empty URL means the artifact is embedded in the
                    // installer JAR (modern Forge ships its runtime jar that way)
                    // and must be extracted from it, not downloaded from maven.
                    let lib_url = match artifact.url.as_ref() {
                        Some(u) if u.is_empty() => String::new(),
                        Some(u) => u.clone(),
                        None => format!("https://maven.minecraftforge.net/{}", lib_path),
                    };
                    let sha1 = artifact.sha1.clone().filter(|s| !s.is_empty());
                    let sha256 = artifact.sha256.clone().filter(|s| !s.is_empty());
                    let sha512 = artifact.sha512.clone().filter(|s| !s.is_empty());
                    let source = super::artifact::has_metadata_checksum(
                        sha1.as_deref(),
                        sha256.as_deref(),
                        sha512.as_deref(),
                    )
                    .then(|| "metadata".to_string());
                    (
                        lib_url,
                        lib_path,
                        sha1,
                        sha256,
                        sha512,
                        source,
                        artifact.size,
                    )
                } else {
                    let path = maven_to_path(&lib.name);
                    let base = lib
                        .url
                        .as_ref()
                        .filter(|u| !u.is_empty())
                        .cloned()
                        .unwrap_or_else(|| super::artifact::FORGE_MAVEN.to_string());
                    let base = if base.ends_with('/') {
                        base
                    } else {
                        format!("{}/", base)
                    };
                    (
                        format!("{}{}", base, path),
                        path,
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                }
            } else {
                let path = maven_to_path(&lib.name);
                let base = lib
                    .url
                    .as_ref()
                    .filter(|u| !u.is_empty())
                    .cloned()
                    .unwrap_or_else(|| super::artifact::FORGE_MAVEN.to_string());
                let base = if base.ends_with('/') {
                    base
                } else {
                    format!("{}/", base)
                };
                (
                    format!("{}{}", base, path),
                    path,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            };

        libraries.push(LoaderLibrary {
            name: lib.name.clone(),
            url,
            path,
            sha1,
            sha256,
            sha512,
            checksum_source,
            size,
        });
    }

    // Loader metadata is remote input too: a path containing `..`, an
    // absolute path or a drive prefix must never reach a filesystem join.
    for lib in &libraries {
        if crate::import::check_safe_remote_path(&lib.path).is_err() {
            return Err(LauncherError::ModLoader(format!(
                "Unsafe library path in Forge profile: {}",
                lib.path
            )));
        }
    }

    // For MC <= 1.12.2 the Forge profile carries its game arguments in the
    // legacy `minecraftArguments` string, which contains the FULL argument
    // list (including tweakClass and the standard Mojang tokens) and must
    // REPLACE the vanilla game arguments. Newer profiles use
    // `arguments.game` which only adds extra args on top of vanilla.
    let (game_args, jvm_args, legacy_args) = if let Some(args) = arguments {
        let game = args
            .game
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>();
        if !game.is_empty() {
            let jvm = args
                .jvm
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            (game, jvm, false)
        } else if let Some(mc_args) = &minecraft_arguments {
            let all = mc_args
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>();
            (all, Vec::new(), true)
        } else {
            (Vec::new(), Vec::new(), false)
        }
    } else if let Some(mc_args) = &minecraft_arguments {
        let all = mc_args
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>();
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

    let jar_bytes = std::fs::read(&installer_path).map_err(|e| {
        LauncherError::ModLoader(format!("Failed to read Forge installer JAR: {}", e))
    })?;

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
                    return build_profile(
                        main_class,
                        ver.libraries,
                        ver.arguments,
                        ver.minecraft_arguments,
                    );
                }
            }
        }
    }

    // Fall back to install_profile.json (legacy standalone installer)
    let install_profile_str = read_entry_from_jar(&mut archive, "install_profile.json")
        .ok_or_else(|| {
            LauncherError::ModLoader(
                "Forge installer JAR has no version.json or install_profile.json".into(),
            )
        })?;

    let mut profile: ForgeInstallProfile =
        serde_json::from_str(&install_profile_str).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e))
        })?;

    // If processor-based, try version.json referenced by the json field
    if profile.main_class.is_none() || profile.libraries.is_empty() {
        if let Some(json_path) = &profile.json {
            if let Some(version_str) = read_entry_from_jar(&mut archive, json_path) {
                if let Ok(version_profile) =
                    serde_json::from_str::<ForgeInstallProfile>(&version_str)
                {
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
                if let Ok(vi) = serde_json::from_value::<ForgeInstallProfile>(version_info.clone())
                {
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

    let main_class = profile
        .main_class
        .ok_or_else(|| LauncherError::ModLoader("No mainClass found in Forge installer".into()))?;

    build_profile(
        main_class,
        profile.libraries,
        profile.arguments,
        profile.minecraft_arguments,
    )
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

/// Path of an extracted installer data file inside the version-scoped
/// `_data` directory: `<libraries>/<data_base>/_data/<data_version>/<key>/<file>`.
///
/// The version component is essential: installer data files (e.g.
/// `client.lzma` patches) differ between loader versions, and a shared path
/// lets the first-installed version's file leak into every other version's
/// processor run. That was observed as a binarypatcher checksum failure
/// which silently produced an empty patched-client JAR.
pub(crate) fn data_file_path(
    libraries_dir: &Path,
    data_base: &str,
    data_version: &str,
    key: &str,
    file_name: &str,
) -> PathBuf {
    libraries_dir
        .join(data_base)
        .join("_data")
        .join(data_version)
        .join(key.to_lowercase())
        .join(file_name)
}

/// Structural sanity check for a processor-generated JAR: the file must
/// exist, be plausibly sized and open as a ZIP with at least one entry.
///
/// This rejects the "processor failed and left an empty or truncated
/// archive" failure mode (e.g. the 22-byte EOCD-only zip binarypatcher left
/// behind on a patching error), which a bare `exists()` check accepts as
/// installed. It is deliberately NOT a full integrity check — a
/// structurally valid but wrong or partial JAR still passes — so it must
/// never be treated as a checksum comparison.
pub(crate) fn is_usable_jar(path: &Path) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.is_file() || meta.len() < 1024 {
        return false;
    }
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    match zip::ZipArchive::new(file) {
        Ok(archive) => !archive.is_empty(),
        Err(_) => false,
    }
}

/// Truncate a subprocess stream for an error message without splitting a
/// UTF-8 code point. Raw byte slicing panicked when Java printed localized
/// (non-ASCII) output.
pub(crate) fn truncate_for_error(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out.trim().to_string()
}

/// List the versioned directories under `libraries/net/minecraft/client/`
/// (e.g. `1.20.1-20230612.114412`) that may hold processor-generated JARs.
fn scan_client_version_dirs(libraries_dir: &Path) -> Vec<PathBuf> {
    let base = libraries_dir.join("net").join("minecraft").join("client");
    let mut dirs = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                dirs.push(e.path());
            }
        }
    }
    dirs
}

/// Find a processor-generated client JAR by suffix (`slim`/`extra`/`srg`),
/// accepting both naming styles (version-dir name or plain MC version).
fn find_client_jar_in(dirs: &[PathBuf], mc_version: &str, suffix: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|d| {
        let name = d
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let p = d.join(format!("client-{}-{}.jar", name, suffix));
        if p.is_file() {
            return Some(p);
        }
        let p2 = d.join(format!("client-{}-{}.jar", mc_version, suffix));
        if p2.is_file() {
            return Some(p2);
        }
        None
    })
}

/// Ensure Forge processor-generated JARs exist, running the processor pipeline
/// if needed.
pub async fn ensure_processor_jars(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
) -> Result<()> {
    // This path runs on every launch and derives file names from both
    // versions before any network validation, so reject unsafe values first.
    super::validate_loader_request(mc_version, loader_version)?;
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
    // (e.g. 1.16.5-20210115.111550), not the plain MC version one -
    // scan whatever version dirs exist under net/minecraft/client/.
    let client_version_dirs = scan_client_version_dirs(&libraries_dir);
    let forge_client_path = libraries_dir
        .join("net")
        .join("minecraftforge")
        .join("forge")
        .join(&full_version)
        .join(format!("forge-{}-client.jar", full_version));

    let vd = versions_dir.join(mc_version);
    let client_jar = if vd.join("client.jar").exists() {
        vd.join("client.jar")
    } else {
        vd.join(format!("{}.jar", mc_version))
    };
    let root_dir = versions_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| versions_dir.clone());
    if !client_jar.exists() {
        return Err(LauncherError::ModLoader(format!(
            "Client JAR not found at {:?}",
            client_jar
        )));
    }

    // If the installer is still cached, its metadata names the exact
    // generated paths and, for most outputs, SHA-1 values. Prefer that over
    // the old "any structurally valid JAR with the right suffix" scan: it
    // rejects a stale or wrong-loader output before the pipeline is skipped.
    let cached_installer =
        std::env::temp_dir().join(format!("forge-{}-installer.jar", full_version));
    let metadata_outputs = read_install_profile_from_installer(&cached_installer)
        .map(|profile| {
            let vars = build_processor_vars(
                &profile,
                &cached_installer,
                &libraries_dir,
                mc_version,
                &client_jar,
                &root_dir,
                "net/minecraftforge/forge",
                &full_version,
            );
            processor_output_plan(&profile, &vars, &libraries_dir)
        })
        .transpose()?;
    if let Some(outputs) = &metadata_outputs {
        if !outputs.is_empty() && verify_processor_outputs(outputs).is_ok() {
            tracing::debug!(
                target: "launcher",
                "All Forge processor JARs match the cached installer metadata"
            );
            return Ok(());
        }
    }

    // Without cached metadata, keep the structural fallback so a cleaned
    // %TEMP% does not force a download on every launch. Existing outputs are
    // trusted only when they pass the structural check: an empty/truncated
    // JAR left behind by a failed processor must trigger a re-run instead of
    // being accepted as installed.
    let slim_ok = find_client_jar_in(&client_version_dirs, mc_version, "slim")
        .map(|p| is_usable_jar(&p))
        .unwrap_or(false);
    let extra_ok = find_client_jar_in(&client_version_dirs, mc_version, "extra")
        .map(|p| is_usable_jar(&p))
        .unwrap_or(false);
    let srg_ok = find_client_jar_in(&client_version_dirs, mc_version, "srg")
        .map(|p| is_usable_jar(&p))
        .unwrap_or(false);
    let client_ok = is_usable_jar(&forge_client_path);

    if metadata_outputs.is_none() && slim_ok && extra_ok && client_ok && srg_ok {
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
        .ok_or_else(|| {
            LauncherError::ModLoader("install_profile.json not found in installer JAR".into())
        })?;

    let install_profile: serde_json::Value =
        serde_json::from_str(&install_profile_str).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e))
        })?;

    drop(archive); // Release the read lock on jar_bytes

    // Check if this is a processor-based installer
    let has_processors = install_profile
        .get("processors")
        .and_then(|p| p.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    if !has_processors {
        tracing::debug!(target: "launcher", "No processors in install_profile.json, nothing to do");
        // Keep the validated installer cached: a later self-heal must not
        // re-download several megabytes for an unchanged loader version.
        return Ok(());
    }

    // --- Step 1: Download all processor libraries ---
    tracing::info!(target: "launcher", "Downloading Forge processor libraries...");
    if let Some(libraries) = install_profile.get("libraries").and_then(|l| l.as_array()) {
        for lib_val in libraries {
            download_processor_lib(
                lib_val,
                &libraries_dir,
                super::artifact::FORGE_MAVEN,
                super::artifact::FORGE_HOSTS,
            )
            .await?;
        }
    }

    // --- Step 2: Extract embedded files from the installer JAR ---
    tracing::info!(target: "launcher", "Extracting embedded files from installer...");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Failed to reopen installer JAR: {}", e)))?;

    // Extract embedded JARs (forge jar with empty URL)
    extract_embedded_forge_jars(&mut archive, &libraries_dir);

    // Extract data files (like client.lzma) from the installer JAR into the
    // version-scoped `_data` directory. Always (over)write: the directory is
    // keyed by the loader version, so a correct file can only come from this
    // loader, and overwriting clears leftovers of an interrupted extract.
    if let Some(data_map) = install_profile.get("data").and_then(|d| d.as_object()) {
        for (key, entry) in data_map {
            let client_val = entry.get("client").and_then(|c| c.as_str()).unwrap_or("");
            if client_val.starts_with('/') {
                let jar_path = client_val.trim_start_matches('/');
                let fname = Path::new(jar_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("data");
                // The data-map key becomes a directory component; a crafted
                // ".." / separator must not escape _data.
                if crate::import::check_safe_file_name(&key.to_lowercase()).is_err() {
                    tracing::warn!(target: "launcher", "Skipping Forge data entry with unsafe key: {}", key);
                    continue;
                }
                let dest = data_file_path(
                    &libraries_dir,
                    "net/minecraftforge/forge",
                    &full_version,
                    key,
                    fname,
                );
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

    drop(archive); // Release the JAR bytes

    // --- Step 3: Run the processor pipeline ---
    // The client JAR and root path were resolved above for the
    // metadata-based fast path and are guaranteed present here.
    // libraries_dir is always `<data_dir>/libraries` (see config.rs),
    // so its parent is the app data dir used to locate managed runtimes.
    let data_dir = libraries_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    if let Some(java_path) = find_java_for(&data_dir, mc_version) {
        tracing::info!(target: "launcher", "Running Forge processors with Java: {:?}", java_path);
        // Long-lived subprocess pipeline (30-120s for some versions) — run on
        // the blocking pool so it can't stall the async runtime that drives
        // the UI and other downloads.
        let profile = install_profile.clone();
        let installer = installer_path.clone();
        let java = java_path.clone();
        let libs = libraries_dir.clone();
        let ver = mc_version.to_string();
        let cjar = client_jar.clone();
        let root = root_dir.clone();
        let data_ver = full_version.clone();
        let pipeline = match tokio::task::spawn_blocking(move || {
            run_forge_processors(ForgeProcessorInputs {
                install_profile: &profile,
                installer_path: &installer,
                java_path: &java,
                libraries_dir: &libs,
                mc_version: &ver,
                client_jar: &cjar,
                root_dir: &root,
                data_base: "net/minecraftforge/forge",
                data_version: &data_ver,
            })
        })
        .await
        {
            Ok(inner) => inner,
            Err(e) => Err(LauncherError::ModLoader(format!(
                "Forge processor task failed: {}",
                e
            ))),
        };
        if let Err(e) = pipeline {
            // A failed processor leaves the pipeline half-applied — do not
            // continue as if the install succeeded. Drop the (possibly
            // partial) patched-client output so the next attempt re-runs the
            // whole pipeline instead of finding it "already present".
            tracing::warn!(target: "launcher", "Forge processor run failed: {}", e);
            std::fs::remove_file(&forge_client_path).ok();
            // Keep the verified installer cached: the next attempt uses its
            // metadata to reject partial outputs and re-run the pipeline
            // instead of falling back to a structural-only check.
            return Err(LauncherError::ModLoader(format!(
                "Forge processor pipeline failed: {}",
                e
            )));
        }
    } else {
        // Without Java the pipeline cannot run; reporting a generic "outputs
        // missing" error hid the real cause and made this look unfixable.
        return Err(LauncherError::ModLoader(format!(
            "No Java {} installation was found to run the Forge installers. Install a matching JDK (or let the launcher download its managed Java) and try again.",
            required_forge_java_major(mc_version)
        )));
    }

    // Keep the validated installer cached (see download_installer) so a
    // self-heal of an unrelated processor JAR does not download it again.

    // --- Step 4: Verify outputs ---
    // Outputs live in the timestamped version dir (1.16.5-20210115.111550);
    // re-scan (the pipeline may have created dirs since the skip check) and
    // apply the same structural check — a file that fails it is not a valid
    // output, so verification must fail instead of reporting success.
    let post_dirs = scan_client_version_dirs(&libraries_dir);
    let slim_path = find_client_jar_in(&post_dirs, mc_version, "slim");
    let extra_path = find_client_jar_in(&post_dirs, mc_version, "extra");
    let srg_path = find_client_jar_in(&post_dirs, mc_version, "srg");
    let slim_ok = slim_path
        .as_ref()
        .map(|p| is_usable_jar(p))
        .unwrap_or(false);
    let extra_ok = extra_path
        .as_ref()
        .map(|p| is_usable_jar(p))
        .unwrap_or(false);
    let srg_ok = srg_path.as_ref().map(|p| is_usable_jar(p)).unwrap_or(false);
    let client_ok = is_usable_jar(&forge_client_path);

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
    log_dir_entries(
        &libraries_dir
            .join("net")
            .join("minecraftforge")
            .join("forge")
            .join(&full_version),
    );

    if !slim_ok || !extra_ok || !srg_ok || !client_ok {
        return Err(LauncherError::ModLoader(
            "Forge processors did not generate required output JARs".into(),
        ));
    }

    tracing::info!(target: "launcher", "Forge processor JARs verified");
    Ok(())
}

fn client_base_exists(libraries_dir: &Path) -> bool {
    libraries_dir
        .join("net")
        .join("minecraft")
        .join("client")
        .exists()
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
            let Some(entry_name) = entry.name().ok().map(|name| name.replace('\\', "/")) else {
                continue;
            };
            if entry_name.starts_with(embedded_maven_base) && entry_name.ends_with(".jar") {
                // Refuse path traversal: only entries that stay under the
                // "maven/" root of the libraries dir may be written.
                if crate::download::is_unsafe_archive_path(
                    entry_name.strip_prefix("maven/").unwrap_or(&entry_name),
                ) {
                    tracing::warn!(target: "launcher", "Skipping unsafe Forge library entry: {}", entry_name);
                    continue;
                }
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

    let mut profile = get_profile(mc_version, loader_version).await?;
    super::artifact::ensure_libraries(
        &mut profile,
        libraries_dir,
        super::artifact::FORGE_HOSTS,
        "Forge",
    )
    .await?;

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
    // Leave the installer in the cache for ensure_processor_jars below (it
    // would otherwise re-download the same file immediately).

    ensure_processor_jars(mc_version, loader_version, libraries_dir, versions_dir).await?;

    tracing::info!(target: "launcher", "Forge install completed for MC {}", mc_version);
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_test_jar(path: &Path, seed: u8) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
        zip.write_all(b"Manifest-Version: 1.0\n").unwrap();
        zip.start_file("payload.bin", options).unwrap();
        zip.write_all(&vec![seed; 4096]).unwrap();
        zip.finish().unwrap();
    }

    fn test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_forge_processor_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn processor_output_plan_uses_installer_metadata_and_rejects_corruption() {
        let dir = test_dir("plan");
        let libs = dir.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();

        let slim = resolve_artifact_ref("[net.minecraft:client:1.20.1-TEST:slim]", &libs);
        let extra = resolve_artifact_ref("[net.minecraft:client:1.20.1-TEST:extra]", &libs);
        let srg = resolve_artifact_ref("[net.minecraft:client:1.20.1-TEST:srg]", &libs);
        let patched =
            resolve_artifact_ref("[net.minecraftforge:forge:1.20.1-47.4.29:client]", &libs);
        let unpacked = resolve_artifact_ref("[net.minecraft:client:1.20.1-TEST:unpacked]", &libs);
        write_test_jar(&slim, 1);
        write_test_jar(&extra, 2);
        write_test_jar(&srg, 3);
        write_test_jar(&patched, 4);

        let slim_sha = crate::download::hash_file_sha1(&slim).unwrap();
        let extra_sha = crate::download::hash_file_sha1(&extra).unwrap();
        let patched_sha = crate::download::hash_file_sha1(&patched).unwrap();
        let profile = serde_json::json!({
            "data": {
                "MC_SLIM": { "client": "[net.minecraft:client:1.20.1-TEST:slim]" },
                "MC_SLIM_SHA": { "client": format!("'{}'", slim_sha) },
                "MC_EXTRA": { "client": "[net.minecraft:client:1.20.1-TEST:extra]" },
                "MC_EXTRA_SHA": { "client": format!("'{}'", extra_sha) },
                "MC_SRG": { "client": "[net.minecraft:client:1.20.1-TEST:srg]" },
                "MC_UNPACKED": { "client": "[net.minecraft:client:1.20.1-TEST:unpacked]" },
                "PATCHED": { "client": "[net.minecraftforge:forge:1.20.1-47.4.29:client]" },
                "PATCHED_SHA": { "client": format!("'{}'", patched_sha) }
            }
        });
        let installer = dir.join("installer.jar");
        let client = dir.join("client.jar");
        let root = dir.join("root");
        let vars = build_processor_vars(
            &profile,
            &installer,
            &libs,
            "1.20.1",
            &client,
            &root,
            "net/minecraftforge/forge",
            "1.20.1-47.4.29",
        );
        let plan = processor_output_plan(&profile, &vars, &libs).unwrap();
        assert_eq!(plan.len(), 4, "only client-side JARs are expected");
        assert!(!plan.iter().any(|output| output.path == unpacked));
        assert!(plan
            .iter()
            .find(|output| output.path == slim)
            .unwrap()
            .checksum
            .is_some());
        assert!(plan
            .iter()
            .find(|output| output.path == srg)
            .unwrap()
            .checksum
            .is_none());
        verify_processor_outputs(&plan).unwrap();

        // A structurally valid but wrong JAR must fail the metadata hash.
        write_test_jar(&patched, 9);
        assert!(verify_processor_outputs(&plan).is_err());

        // A missing declared output is not accepted just because the others
        // exist.
        std::fs::remove_file(&srg).unwrap();
        assert!(verify_processor_outputs(&plan).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_output_plan_rejects_bad_hashes_and_unsafe_paths() {
        let dir = test_dir("bad");
        let libs = dir.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();
        let installer = dir.join("installer.jar");
        let client = dir.join("client.jar");
        let root = dir.join("root");

        let malformed = serde_json::json!({
            "data": {
                "MC_SLIM": { "client": "[net.minecraft:client:1.20.1-TEST:slim]" },
                "MC_SLIM_SHA": { "client": "'not-a-hash'" }
            }
        });
        let vars = build_processor_vars(
            &malformed,
            &installer,
            &libs,
            "1.20.1",
            &client,
            &root,
            "net/minecraftforge/forge",
            "1.20.1-47.4.29",
        );
        assert!(processor_output_plan(&malformed, &vars, &libs).is_err());

        let traversal = serde_json::json!({
            "data": {
                "MC_SLIM": { "client": "[net.minecraft:client:../../escape:slim]" }
            }
        });
        let vars = build_processor_vars(
            &traversal,
            &installer,
            &libs,
            "1.20.1",
            &client,
            &root,
            "net/minecraftforge/forge",
            "1.20.1-47.4.29",
        );
        assert!(processor_output_plan(&traversal, &vars, &libs).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_output_paths_are_version_scoped() {
        let dir = test_dir("versions");
        let libs = dir.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();
        let installer = dir.join("installer.jar");
        let client = dir.join("client.jar");
        let root = dir.join("root");

        let plan_for = |loader_version: &str, mappings_version: &str| {
            let profile = serde_json::json!({
                "data": {
                    "MC_SLIM": {
                        "client": format!("[net.minecraft:client:{mappings_version}:slim]")
                    },
                    "PATCHED": {
                        "client": format!(
                            "[net.minecraftforge:forge:{loader_version}:client]"
                        )
                    }
                }
            });
            let vars = build_processor_vars(
                &profile,
                &installer,
                &libs,
                "1.20.1",
                &client,
                &root,
                "net/minecraftforge/forge",
                loader_version,
            );
            processor_output_plan(&profile, &vars, &libs).unwrap()
        };

        let first = plan_for("1.20.1-47.4.29", "1.20.1-20230612.114412");
        let second = plan_for("1.20.1-47.4.30", "1.20.1-20230701.000000");
        assert_ne!(
            first.iter().map(|o| &o.path).collect::<Vec<_>>(),
            second.iter().map(|o| &o.path).collect::<Vec<_>>(),
            "a changed loader/mapping version must not reuse the old output path"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_outputs_map_is_resolved_and_verified() {
        let dir = test_dir("outputs_map");
        let libs = dir.join("libraries");
        let out = libs.join("example/output/1.0/output-1.0.jar");
        write_test_jar(&out, 7);
        let sha = crate::download::hash_file_sha1(&out).unwrap();
        let profile = serde_json::json!({
            "data": {},
            "processors": [{
                "outputs": {
                    "{OUT}": "{OUT_SHA}"
                }
            }]
        });
        let mut vars = HashMap::new();
        vars.insert("OUT".to_string(), out.to_string_lossy().to_string());
        vars.insert("OUT_SHA".to_string(), sha);
        let plan = processor_output_plan(&profile, &vars, &libs).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, out);
        assert!(plan[0].checksum.is_some());
        verify_processor_outputs(&plan).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provider_artifact_hashes_and_sizes_reach_the_loader_profile() {
        let lib: ForgeLibrary = serde_json::from_value(serde_json::json!({
            "name": "example:library:1.0",
            "downloads": {
                "artifact": {
                    "path": "example/library/1.0/library-1.0.jar",
                    "url": "https://maven.example.invalid/library-1.0.jar",
                    "sha1": "0123456789abcdef0123456789abcdef01234567",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "sha512": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size": 4242
                }
            }
        }))
        .expect("forge library fixture");
        let profile = build_profile("Main".into(), vec![lib], None, None).unwrap();
        let library = &profile.libraries[0];
        assert_eq!(
            library.sha1.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(library.sha256.is_some());
        assert!(library.sha512.is_some());
        assert_eq!(library.checksum_source.as_deref(), Some("metadata"));
        assert_eq!(library.checksum().unwrap().unwrap().algorithm(), "sha512");
        assert_eq!(library.size, Some(4242));
    }

    #[test]
    fn forge_java_matrix() {
        assert_eq!(required_forge_java_major("1.8.9"), 8);
        assert_eq!(required_forge_java_major("1.12.2"), 8);
        assert_eq!(required_forge_java_major("1.16.5"), 8);
        assert_eq!(required_forge_java_major("1.17.1"), 16);
        assert_eq!(required_forge_java_major("1.18.2"), 17);
        assert_eq!(required_forge_java_major("1.20.1"), 17);
        assert_eq!(required_forge_java_major("1.20.4"), 17);
        assert_eq!(required_forge_java_major("1.20.5"), 21);
        assert_eq!(required_forge_java_major("1.21.4"), 21);
        // Unknown/unparseable input degrades to the oldest supported Java.
        assert_eq!(required_forge_java_major(""), 8);
        assert_eq!(required_forge_java_major("banana"), 8);
    }

    /// Two loader versions must never share extracted installer data files —
    /// the shared-path leak corrupted a 1.20.1 patching run with 1.13.2 data.
    #[test]
    fn data_file_path_is_version_scoped() {
        let p = data_file_path(
            Path::new("C:/libs"),
            "net/minecraftforge/forge",
            "1.20.1-47.4.20",
            "BINPATCH",
            "client.lzma",
        );
        let s = p.to_string_lossy().replace('\\', "/");
        assert_eq!(
            s,
            "C:/libs/net/minecraftforge/forge/_data/1.20.1-47.4.20/binpatch/client.lzma"
        );

        let p2 = data_file_path(
            Path::new("C:/libs"),
            "net/minecraftforge/forge",
            "1.13.2-25.0.223",
            "BINPATCH",
            "client.lzma",
        );
        assert_ne!(p, p2, "different loader versions must use different files");
    }

    /// The structural check must reject the exact artifact a failed
    /// processor leaves behind (empty/truncated archives) while accepting a
    /// real JAR — and it must never be confused with a checksum check.
    #[test]
    fn usable_jar_rejects_missing_empty_and_truncated_files() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("vl_usablejar_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Missing file.
        assert!(!is_usable_jar(&dir.join("nope.jar")));

        // The failure mode seen in the wild: binarypatcher's 22-byte
        // end-of-central-directory-only archive (a structurally valid but
        // EMPTY zip).
        let empty_zip = dir.join("empty.jar");
        let mut bytes = vec![0x50, 0x4B, 0x05, 0x06];
        bytes.extend_from_slice(&[0u8; 18]);
        std::fs::write(&empty_zip, &bytes).unwrap();
        assert!(!is_usable_jar(&empty_zip));

        // A real archive with content passes (Stored so the on-disk size
        // stays above the size floor instead of compressing to a few bytes).
        let ok_jar = dir.join("ok.jar");
        let f = std::fs::File::create(&ok_jar).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("data.bin", opts).unwrap();
        zw.write_all(&vec![0u8; 4096]).unwrap();
        zw.finish().unwrap();
        assert!(is_usable_jar(&ok_jar));

        // Truncated write (interrupted download/extract) fails to open.
        let truncated = dir.join("truncated.jar");
        let full = std::fs::read(&ok_jar).unwrap();
        std::fs::write(&truncated, &full[..full.len() / 2]).unwrap();
        assert!(!is_usable_jar(&truncated));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn installer_cache_requires_zip_with_metadata() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("vl_installer_cache_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Junk that is big enough to pass a size-only check.
        let junk = dir.join("junk.jar");
        std::fs::write(&junk, vec![0x41u8; 8192]).unwrap();
        assert!(!installer_jar_is_valid(&junk));

        // A valid zip without installer metadata is still not an installer.
        let nometa = dir.join("nometa.jar");
        let f = std::fs::File::create(&nometa).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("hello.txt", opts).unwrap();
        zw.write_all(&vec![0u8; 4096]).unwrap();
        zw.finish().unwrap();
        assert!(!installer_jar_is_valid(&nometa));

        // A readable archive carrying install_profile.json is usable.
        let good = dir.join("good.jar");
        let f = std::fs::File::create(&good).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        zw.start_file("install_profile.json", opts).unwrap();
        zw.write_all(&vec![b' '; 2048]).unwrap();
        zw.start_file("version.json", opts).unwrap();
        zw.write_all(b"{}").unwrap();
        zw.finish().unwrap();
        assert!(installer_jar_is_valid(&good));

        // Truncated archive is rejected even if the file is large.
        let truncated = dir.join("truncated.jar");
        let full = std::fs::read(&good).unwrap();
        std::fs::write(&truncated, &full[..full.len() / 2]).unwrap();
        assert!(!installer_jar_is_valid(&truncated));

        std::fs::remove_dir_all(&dir).ok();
    }
}

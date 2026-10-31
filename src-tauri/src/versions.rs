use crate::error::{LauncherError, Result};
use crate::import::check_safe_remote_path;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Version manifest from Mojang
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LatestVersions {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub version_type: String,
    pub url: String,
    pub time: String,
    #[serde(rename = "releaseTime")]
    pub release_time: String,
    pub sha1: String,
    #[serde(rename = "complianceLevel")]
    pub compliance_level: u32,
}

/// Detailed version info (from individual version JSON)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VersionInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub version_type: String,
    #[serde(rename = "mainClass")]
    pub main_class: String,
    #[serde(rename = "minecraftArguments", default)]
    pub minecraft_arguments: Option<String>,
    pub arguments: Option<Arguments>,
    pub libraries: Vec<Library>,
    pub downloads: Downloads,
    #[serde(rename = "assetIndex")]
    pub asset_index: AssetIndex,
    pub assets: String,
    #[serde(rename = "javaVersion", default)]
    pub java_version: Option<JavaVersionReq>,
    #[serde(rename = "complianceLevel", default)]
    pub compliance_level: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JavaVersionReq {
    pub component: String,
    #[serde(rename = "majorVersion")]
    pub major_version: u32,
}

impl VersionInfo {
    /// Java major version required by the manifest. Versions without a
    /// `javaVersion` field (1.6.4 and older) are Java 8-era — default to 8.
    pub fn required_java_major(&self) -> u32 {
        self.java_version
            .as_ref()
            .map(|v| v.major_version)
            .unwrap_or(8)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<serde_json::Value>,
    #[serde(default)]
    pub jvm: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Downloads {
    pub client: DownloadInfo,
    #[serde(default)]
    pub server: Option<DownloadInfo>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DownloadInfo {
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AssetIndex {
    pub id: String,
    pub sha1: String,
    pub size: u64,
    #[serde(rename = "totalSize")]
    pub total_size: u64,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Library {
    pub name: String,
    pub downloads: Option<LibraryDownloads>,
    pub url: Option<String>,
    pub rules: Option<Vec<Rule>>,
    pub natives: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LibraryDownloads {
    pub artifact: Option<LibraryArtifact>,
    pub classifiers: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LibraryArtifact {
    pub path: String,
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Rule {
    pub action: String,
    pub os: Option<OsRule>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OsRule {
    pub name: Option<String>,
    pub arch: Option<String>,
}

/// Asset index mapping
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AssetIndexData {
    pub objects: std::collections::HashMap<String, AssetObject>,
    /// Legacy flag: launcher should mirror the whole index into
    /// `assets/virtual/legacy` (used by 1.7.x/1.8.x for old resource packs).
    #[serde(default)]
    pub map_to_resources: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
    /// When true (1.9+ indexes), the object is part of the virtual/legacy tree.
    #[serde(default, rename = "virtual")]
    pub is_virtual: bool,
}

/// Fetch version manifest from Mojang (with retries honoring Retry-After,
/// since Mojang periodically throttles this endpoint with HTTP 429).
pub async fn fetch_version_manifest() -> Result<VersionManifest> {
    tracing::info!(target: "launcher", "Fetching version manifest from Mojang");
    let client = crate::download::global_http_client()?;
    let manifest = crate::download::get_with_retry(
        client
            .get("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")
            .timeout(std::time::Duration::from_secs(20)),
        "Mojang",
        4,
        &[1000, 2000, 4000],
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "launcher", "Failed to fetch version manifest: {}", e);
        e
    })?
    .json::<VersionManifest>()
    .await
    .map_err(|e| {
        tracing::error!(target: "launcher", "Failed to parse version manifest: {}", e);
        e
    })?;
    Ok(manifest)
}

/// Fetch detailed version info
pub async fn fetch_version_info(url: &str) -> Result<VersionInfo> {
    tracing::info!(target: "launcher", "Fetching version info from {}", url);
    let client = crate::download::global_http_client()?;
    let info = crate::download::get_with_retry(
        client.get(url).timeout(std::time::Duration::from_secs(20)),
        "Mojang",
        4,
        &[1000, 2000, 4000],
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "launcher", "Failed to fetch version info: {}", e);
        e
    })?
    .json::<VersionInfo>()
    .await
    .map_err(|e| {
        tracing::error!(target: "launcher", "Failed to parse version info: {}", e);
        e
    })?;
    Ok(info)
}

/// Fetch asset index
pub async fn fetch_asset_index(url: &str) -> Result<AssetIndexData> {
    tracing::info!(target: "launcher", "Fetching asset index from {}", url);
    let client = crate::download::global_http_client()?;
    let index = crate::download::get_with_retry(
        client.get(url).timeout(std::time::Duration::from_secs(20)),
        "Mojang",
        4,
        &[1000, 2000, 4000],
    )
    .await
    .map_err(|e| {
        tracing::error!(target: "launcher", "Failed to fetch asset index: {}", e);
        e
    })?
    .json::<AssetIndexData>()
    .await
    .map_err(|e| {
        tracing::error!(target: "launcher", "Failed to parse asset index: {}", e);
        e
    })?;
    Ok(index)
}

/// Check if a library should be included based on OS rules
pub fn should_include_library(lib: &Library) -> bool {
    let rules = match &lib.rules {
        Some(rules) => rules,
        None => return true,
    };

    let current_os = std::env::consts::OS;
    let current_arch = std::env::consts::ARCH;

    // Per the version.json spec: libraries with rules are included only if
    // the last matching rule allows them. If no rule matches (e.g. a
    // macOS-only library on Windows), the library must be excluded.
    let mut dominated_action = "disallow";

    for rule in rules {
        let matches = match &rule.os {
            None => true,
            Some(os_rule) => {
                let os_matches = match &os_rule.name {
                    Some(name) => name == current_os,
                    None => true,
                };
                let arch_matches = match &os_rule.arch {
                    Some(arch) => {
                        arch == current_arch
                            || (arch == "x86"
                                && (current_arch == "x86" || current_arch == "x86_64"))
                    }
                    None => true,
                };
                os_matches && arch_matches
            }
        };

        if matches {
            dominated_action = &rule.action;
        }
    }

    dominated_action == "allow"
}

/// Convert Maven-style library name to path
/// e.g., "net.fabricmc:fabric-loader:0.15.0" → "net/fabricmc/fabric-loader/0.15.0/fabric-loader-0.15.0.jar"
pub fn maven_to_path(name: &str) -> String {
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() < 3 {
        return name.replace(':', "/");
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];

    if parts.len() > 3 {
        let classifier = parts[3];
        format!(
            "{}/{}/{}/{}-{}-{}.jar",
            group, artifact, version, artifact, version, classifier
        )
    } else {
        format!(
            "{}/{}/{}/{}-{}.jar",
            group, artifact, version, artifact, version
        )
    }
}

/// Join a remote-supplied relative path onto a launcher-managed base after
/// validating it. Metadata that tries to escape the base must fail the
/// install/launch instead of being silently skipped, otherwise a required
/// library can disappear while the operation still reports success.
fn checked_remote_join(base: &Path, relative: &str) -> Result<PathBuf> {
    check_safe_remote_path(relative)?;
    let joined = base.join(relative);
    // Defense in depth: `check_safe_remote_path` already rejects absolute,
    // rooted and drive-prefixed paths; keep the containment check close to
    // the filesystem access as a second line of defense.
    if !joined.starts_with(base) {
        return Err(LauncherError::Version(format!(
            "Unsafe path in remote metadata: {}",
            relative
        )));
    }
    Ok(joined)
}

/// Return the native classifier for the current OS (e.g. "natives-windows")
/// by reading the library's `natives` map, or None if not applicable.
pub(crate) fn native_classifier(lib: &Library) -> Option<String> {
    let natives = lib.natives.as_ref()?;
    let os_map = natives.as_object()?;
    let os_key = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "osx",
        _ => std::env::consts::OS,
    };
    let classifier = os_map.get(os_key)?.as_str()?;
    // Old manifests contain `${arch}` (e.g. "natives-windows-${arch}") —
    // the launcher always targets 64-bit here.
    Some(classifier.replace("${arch}", "64"))
}

/// Read-only filesystem query used while composing a classpath.
///
/// Injected into `build_classpath_entries` so launch-profile composition can
/// be tested without touching the real disk; production uses
/// [`RealClasspathProbe`].
pub trait ClasspathProbe {
    fn is_file(&self, path: &Path) -> bool;
}

/// Production probe backed by `std::fs`.
pub struct RealClasspathProbe;

impl ClasspathProbe for RealClasspathProbe {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// Build the ordered vanilla classpath entries from version metadata.
///
/// Paths come from remote metadata, so every join is validated. An unsafe
/// path is a hard error (never silently dropped), and a required library
/// that is missing on disk fails with a clear message instead of letting
/// Java die later with a cryptic `NoClassDefFoundError`.
pub fn build_classpath_entries<P: ClasspathProbe + ?Sized>(
    version_info: &VersionInfo,
    libraries_dir: &Path,
    client_jar: &Path,
    probe: &P,
) -> Result<Vec<PathBuf>> {
    let mut classpath_entries: Vec<PathBuf> = Vec::new();

    for lib in &version_info.libraries {
        if !should_include_library(lib) {
            continue;
        }

        if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let lib_path = checked_remote_join(libraries_dir, &artifact.path)?;
                if probe.is_file(&lib_path) {
                    classpath_entries.push(lib_path);
                } else {
                    return Err(LauncherError::Launch(format!(
                        "Required library is missing: {}",
                        lib_path.display()
                    )));
                }
            }
            // Add native classifier JAR (e.g. lwjgl-3.2.1-natives-windows.jar)
            if let Some(classifier) = native_classifier(lib) {
                if let Some(classifiers) = &downloads.classifiers {
                    if let Some(native_artifact) = classifiers.get(classifier.as_str()) {
                        if let Some(path) = native_artifact.get("path").and_then(|p| p.as_str()) {
                            let native_path = checked_remote_join(libraries_dir, path)?;
                            if probe.is_file(&native_path) {
                                classpath_entries.push(native_path);
                            } else {
                                return Err(LauncherError::Launch(format!(
                                    "Required native library is missing: {}",
                                    native_path.display()
                                )));
                            }
                        }
                    }
                }
            }
        } else {
            let path = maven_to_path(&lib.name);
            let lib_path = checked_remote_join(libraries_dir, &path)?;
            if probe.is_file(&lib_path) {
                classpath_entries.push(lib_path);
            } else {
                return Err(LauncherError::Launch(format!(
                    "Required library is missing: {}",
                    lib_path.display()
                )));
            }
        }
    }

    classpath_entries.push(client_jar.to_path_buf());

    Ok(classpath_entries)
}

/// Build the vanilla classpath as a platform-separated string.
///
/// Kept as the string-returning convenience used by tests and callers that do
/// not need the structured entries; `launch::build_launch_profile` uses
/// [`build_classpath_entries`] directly so it can reorder and deduplicate.
pub fn build_classpath(
    version_info: &VersionInfo,
    libraries_dir: &Path,
    client_jar: &Path,
) -> Result<String> {
    let entries =
        build_classpath_entries(version_info, libraries_dir, client_jar, &RealClasspathProbe)?;
    Ok(join_classpath(&entries))
}

/// Join classpath entries with the platform separator used by the launcher.
pub(crate) fn join_classpath(entries: &[PathBuf]) -> String {
    entries
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(";")
}

/// Evaluate a single Mojang argument rule entry (`{"rules": [...], "value": ...}`).
/// `has_custom_resolution` is the only feature flag this launcher can enable.
fn argument_rule_allows(entry: &serde_json::Value, has_custom_resolution: bool) -> bool {
    let Some(rules) = entry.get("rules").and_then(|r| r.as_array()) else {
        return true;
    };
    let mut action = "disallow";
    for rule in rules {
        let rule_action = rule
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("disallow");
        let mut matches = true;
        if let Some(os) = rule.get("os") {
            if let Some(name) = os.get("name").and_then(|n| n.as_str()) {
                if name != std::env::consts::OS {
                    matches = false;
                }
            }
            if matches {
                if let Some(arch) = os.get("arch").and_then(|a| a.as_str()) {
                    let current = std::env::consts::ARCH;
                    let arch_ok = match arch {
                        // Launcher always ships 64-bit JVMs; 32-bit rules never apply.
                        "x86" => false,
                        "x86_64" => current == "x86_64",
                        a => a == current,
                    };
                    if !arch_ok {
                        matches = false;
                    }
                }
            }
        }
        if matches {
            if let Some(features) = rule.get("features").and_then(|f| f.as_object()) {
                for (name, required) in features {
                    let enabled = match name.as_str() {
                        "has_custom_resolution" => has_custom_resolution,
                        _ => false,
                    };
                    if required.as_bool() != Some(enabled) {
                        matches = false;
                        break;
                    }
                }
            }
        }
        if matches {
            action = rule_action;
        }
    }
    action == "allow"
}

/// Expand a `value` field (string or array of strings) into the argument list.
fn expand_argument_value(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                expand_argument_value(item, out);
            }
        }
        _ => {}
    }
}

/// Evaluate Mojang `arguments` entries: plain strings pass through, objects
/// with `rules`/`value` are evaluated against the current OS/arch/features.
fn evaluate_arguments(args: &[serde_json::Value], has_custom_resolution: bool) -> Vec<String> {
    let mut out = Vec::new();
    for arg in args {
        if let Some(s) = arg.as_str() {
            out.push(s.to_string());
        } else if arg.is_object() && argument_rule_allows(arg, has_custom_resolution) {
            if let Some(value) = arg.get("value") {
                expand_argument_value(value, &mut out);
            }
        }
    }
    out
}

/// Split legacy `minecraftArguments` respecting double quotes
/// (e.g. `--foo "some value"` stays one token).
fn split_mc_arguments(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Extract game arguments from version info (handles both old and new format).
/// `has_custom_resolution` enables the manifest's `has_custom_resolution`
/// feature rules so `--width/--height` come from the manifest itself
/// (no duplicate resolution args added by the launcher).
pub fn get_game_arguments(version_info: &VersionInfo, has_custom_resolution: bool) -> Vec<String> {
    if let Some(arguments) = &version_info.arguments {
        evaluate_arguments(&arguments.game, has_custom_resolution)
    } else if let Some(mc_args) = &version_info.minecraft_arguments {
        split_mc_arguments(mc_args)
    } else {
        Vec::new()
    }
}

/// Extract JVM arguments from version info (rules evaluated against current OS).
pub fn get_jvm_arguments(version_info: &VersionInfo) -> Vec<String> {
    if let Some(arguments) = &version_info.arguments {
        evaluate_arguments(&arguments.jvm, false)
    } else {
        vec![
            "-Djava.library.path=${natives_directory}".to_string(),
            "-Dminecraft.launcher.brand=${launcher_name}".to_string(),
            "-Dminecraft.launcher.version=${launcher_version}".to_string(),
            "-cp".to_string(),
            "${classpath}".to_string(),
        ]
    }
}

/// Collect all files that need to be downloaded for a version, including native
/// classifier JARs (e.g. lwjgl natives for the current platform).
///
/// Every path is validated before joining. Invalid metadata fails the whole
/// collection rather than silently omitting a required file.
pub fn collect_downloads(
    version_info: &VersionInfo,
    libraries_dir: &Path,
    versions_dir: &Path,
) -> Result<Vec<(String, PathBuf, String, u64)>> {
    // Returns: (url, local_path, sha1, size)
    let mut files = Vec::new();

    // Client JAR — the version id becomes a directory name.
    crate::import::check_safe_file_name(&version_info.id).map_err(|_| {
        LauncherError::Version(format!(
            "Invalid version id in remote metadata: {}",
            version_info.id
        ))
    })?;
    let client_path = versions_dir.join(&version_info.id).join("client.jar");
    if !client_path.starts_with(versions_dir) {
        return Err(LauncherError::Version(format!(
            "Unsafe version id in remote metadata: {}",
            version_info.id
        )));
    }
    files.push((
        version_info.downloads.client.url.clone(),
        client_path,
        version_info.downloads.client.sha1.clone(),
        version_info.downloads.client.size,
    ));

    // Libraries
    for lib in &version_info.libraries {
        if !should_include_library(lib) {
            continue;
        }

        if let Some(downloads) = &lib.downloads {
            // Main artifact
            if let Some(artifact) = &downloads.artifact {
                let lib_path = checked_remote_join(libraries_dir, &artifact.path)?;
                if !lib_path.exists() {
                    files.push((
                        artifact.url.clone(),
                        lib_path,
                        artifact.sha1.clone(),
                        artifact.size,
                    ));
                }
            }
            // Native classifier artifact for current OS
            if let Some(classifier) = native_classifier(lib) {
                if let Some(classifiers) = &downloads.classifiers {
                    if let Some(native_artifact) = classifiers.get(classifier.as_str()) {
                        if let (Some(url), Some(path), Some(sha1), Some(size)) = (
                            native_artifact.get("url").and_then(|u| u.as_str()),
                            native_artifact.get("path").and_then(|p| p.as_str()),
                            native_artifact.get("sha1").and_then(|s| s.as_str()),
                            native_artifact.get("size").and_then(|s| s.as_u64()),
                        ) {
                            let native_path = checked_remote_join(libraries_dir, path)?;
                            if !native_path.exists() {
                                files.push((url.to_string(), native_path, sha1.to_string(), size));
                            }
                        }
                    }
                }
            }
        } else if let Some(url_base) = &lib.url {
            let path = maven_to_path(&lib.name);
            let lib_path = checked_remote_join(libraries_dir, &path)?;
            if !lib_path.exists() {
                files.push((
                    format!("{}{}", url_base, path),
                    lib_path,
                    String::new(), // no sha1 available
                    0,
                ));
            }
        }
    }

    Ok(files)
}

/// Load a saved asset index (e.g. `indexes/legacy.json`) from disk
pub fn load_asset_index(path: &Path) -> Result<AssetIndexData> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| LauncherError::Version(format!("Failed to read asset index: {}", e)))?;

    let index: AssetIndexData = serde_json::from_str(&contents)
        .map_err(|e| LauncherError::Version(format!("Failed to parse asset index: {}", e)))?;

    Ok(index)
}

/// Download any missing native classifier JARs (e.g. LWJGL natives-windows)
/// for the current OS. Called at launch to self-heal installs that predate
/// native-JAR support.
pub async fn ensure_native_libraries(
    version_info: &VersionInfo,
    libraries_dir: &Path,
) -> Result<()> {
    let mut missing: Vec<(String, PathBuf, String)> = Vec::new();

    for lib in &version_info.libraries {
        if !should_include_library(lib) {
            continue;
        }
        if let Some(classifier) = native_classifier(lib) {
            if let Some(downloads) = &lib.downloads {
                if let Some(classifiers) = &downloads.classifiers {
                    if let Some(native_artifact) = classifiers.get(classifier.as_str()) {
                        if let (Some(url), Some(path), sha1) = (
                            native_artifact.get("url").and_then(|u| u.as_str()),
                            native_artifact.get("path").and_then(|p| p.as_str()),
                            native_artifact
                                .get("sha1")
                                .and_then(|s| s.as_str())
                                .unwrap_or(""),
                        ) {
                            let native_path = checked_remote_join(libraries_dir, path)?;
                            if !native_path.exists() {
                                missing.push((url.to_string(), native_path, sha1.to_string()));
                            }
                        }
                    }
                }
            }
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    tracing::info!(target: "launcher", "Downloading {} missing native libraries...", missing.len());
    use futures::StreamExt;

    // LWJGL ships several independent classifier JARs. Downloading them
    // sequentially added their round-trips to every cold launch; four at a
    // time keeps the UI responsive without hammering the mirrors.
    let downloads = missing.into_iter().map(|(url, path, sha1)| async move {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        crate::download::download_file(&url, &path, &sha1)
            .await
            .map_err(|e| {
                LauncherError::Version(format!("Failed to download native library {}: {}", url, e))
            })
    });
    let mut downloads = futures::stream::iter(downloads).buffer_unordered(4);
    while let Some(result) = downloads.next().await {
        result?;
    }

    Ok(())
}

/// Manifest of files owned by native extraction. Without it, a file removed
/// from the current native JARs could not be distinguished from a user file.
const NATIVES_MANIFEST: &str = ".voidlauncher-natives.json";

/// Process-wide unique suffix for staging and temporary files. Concurrent
/// launches of the same version may prepare natives at the same time.
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", std::process::id(), n, nanos)
}

fn natives_staging_dir(natives_dir: &Path) -> PathBuf {
    let parent = natives_dir.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".natives-staging-{}", unique_suffix()))
}

/// Relative paths of every regular file below `root`, sorted for determinism.
fn collect_relative_files(root: &Path) -> Result<Vec<String>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        let entries = std::fs::read_dir(dir).map_err(|e| {
            LauncherError::Version(format!(
                "Failed to read natives directory {}: {}",
                dir.display(),
                e
            ))
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                LauncherError::Version(format!(
                    "Failed to read natives directory {}: {}",
                    dir.display(),
                    e
                ))
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|e| {
                LauncherError::Version(format!(
                    "Failed to inspect native file {}: {}",
                    path.display(),
                    e
                ))
            })?;
            if file_type.is_dir() {
                walk(root, &path, out)?;
            } else if file_type.is_file() {
                let relative = path.strip_prefix(root).map_err(|_| {
                    LauncherError::Version(format!(
                        "Native file {} escaped {}",
                        path.display(),
                        root.display()
                    ))
                })?;
                let relative = relative.to_string_lossy().replace('\\', "/");
                if relative != NATIVES_MANIFEST {
                    out.push(relative);
                }
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    if root.is_dir() {
        walk(root, root, &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn read_natives_manifest(natives_dir: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(natives_dir.join(NATIVES_MANIFEST)).ok()?;
    let mut files: Vec<String> = serde_json::from_str(&raw).ok()?;
    files.retain(|file| !file.is_empty() && !crate::download::is_unsafe_archive_path(file));
    Some(files)
}

fn write_natives_manifest(natives_dir: &Path, files: &[String]) -> Result<()> {
    let mut files = files.to_vec();
    files.sort();
    files.dedup();
    let payload = serde_json::to_vec_pretty(&files)?;

    let manifest = natives_dir.join(NATIVES_MANIFEST);
    let temporary = natives_dir.join(format!(".voidlauncher-natives-{}.tmp", unique_suffix()));
    std::fs::write(&temporary, payload).map_err(|e| {
        LauncherError::Version(format!(
            "Failed to write natives manifest {}: {}",
            temporary.display(),
            e
        ))
    })?;
    if manifest.exists() {
        if let Err(e) = std::fs::remove_file(&manifest) {
            let _ = std::fs::remove_file(&temporary);
            return Err(LauncherError::Version(format!(
                "Failed to replace natives manifest {}: {}",
                manifest.display(),
                e
            )));
        }
    }
    std::fs::rename(&temporary, &manifest).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        LauncherError::Version(format!(
            "Failed to write natives manifest {}: {}",
            manifest.display(),
            e
        ))
    })
}

/// Extract every required native JAR into a fresh staging directory. Any
/// failure aborts preparation before the live natives directory is touched.
fn extract_natives_into(
    version_info: &VersionInfo,
    libraries_dir: &Path,
    staging: &Path,
) -> Result<Vec<String>> {
    std::fs::create_dir_all(staging)?;
    let mut extracted = Vec::new();

    for lib in &version_info.libraries {
        if !should_include_library(lib) {
            continue;
        }
        let Some(classifier) = native_classifier(lib) else {
            continue;
        };
        let Some(downloads) = &lib.downloads else {
            continue;
        };
        let Some(classifiers) = &downloads.classifiers else {
            continue;
        };
        let Some(native_artifact) = classifiers.get(classifier.as_str()) else {
            continue;
        };
        let Some(path) = native_artifact.get("path").and_then(|p| p.as_str()) else {
            continue;
        };

        let jar_path = checked_remote_join(libraries_dir, path)?;
        if !jar_path.is_file() {
            return Err(LauncherError::Version(format!(
                "Native library '{}' is missing: {}",
                lib.name,
                jar_path.display()
            )));
        }

        let file = std::fs::File::open(&jar_path).map_err(|e| {
            LauncherError::Version(format!(
                "Failed to open native library '{}' at {}: {}",
                lib.name,
                jar_path.display(),
                e
            ))
        })?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| {
            LauncherError::Version(format!(
                "Native library '{}' at {} is not a valid archive: {}",
                lib.name,
                jar_path.display(),
                e
            ))
        })?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| {
                LauncherError::Version(format!(
                    "Failed to read entry {} of native library '{}' at {}: {}",
                    i,
                    lib.name,
                    jar_path.display(),
                    e
                ))
            })?;
            let name = entry.name().replace('\\', "/");
            if name.starts_with("META-INF/") || name.ends_with('/') || name == NATIVES_MANIFEST {
                continue;
            }
            // Refuse path traversal: an entry such as "../evil.dll" must not
            // escape the staging/natives directory.
            if crate::download::is_unsafe_archive_path(&name) {
                tracing::warn!(target: "launcher", "Skipping unsafe native entry: {}", name);
                continue;
            }
            let dest = staging.join(&name);
            if !dest.starts_with(staging) {
                continue;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    LauncherError::Version(format!(
                        "Failed to create native directory {}: {}",
                        parent.display(),
                        e
                    ))
                })?;
            }
            let mut output = std::fs::File::create(&dest).map_err(|e| {
                LauncherError::Version(format!(
                    "Failed to write native file {} from native library '{}' at {}: {}",
                    dest.display(),
                    lib.name,
                    jar_path.display(),
                    e
                ))
            })?;
            std::io::copy(&mut entry, &mut output).map_err(|e| {
                LauncherError::Version(format!(
                    "Failed to extract native file {} from native library '{}' at {}: {}",
                    dest.display(),
                    lib.name,
                    jar_path.display(),
                    e
                ))
            })?;
            extracted.push(name);
        }
    }

    Ok(extracted)
}

fn files_equal(left: &Path, right: &Path) -> bool {
    match (std::fs::metadata(left), std::fs::metadata(right)) {
        (Ok(a), Ok(b)) if a.len() == b.len() => match (std::fs::read(left), std::fs::read(right)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        },
        _ => false,
    }
}

/// Move one staged file into place. The destination is removed only after the
/// staged copy exists, so a lock/read-only failure leaves the old file intact
/// and is reported with the path that could not be replaced.
fn install_native_file(source: &Path, destination: &Path) -> Result<()> {
    let temporary = destination.with_file_name(format!(
        ".{}.voidlauncher-{}.tmp",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("native"),
        unique_suffix()
    ));
    std::fs::rename(source, &temporary).map_err(|e| {
        LauncherError::Version(format!(
            "Failed to stage native file {}: {}",
            destination.display(),
            e
        ))
    })?;

    if destination.exists() {
        if let Err(e) = std::fs::remove_file(destination) {
            let _ = std::fs::remove_file(&temporary);
            return Err(LauncherError::Version(format!(
                "Cannot replace native file {}: {}. Close any running game that uses this version and try again.",
                destination.display(),
                e
            )));
        }
    }
    if let Err(e) = std::fs::rename(&temporary, destination) {
        let _ = std::fs::remove_file(&temporary);
        return Err(LauncherError::Version(format!(
            "Failed to install native file {}: {}",
            destination.display(),
            e
        )));
    }
    Ok(())
}

fn prune_empty_native_dirs(path: &Path, natives_dir: &Path) {
    let mut current = path.parent();
    while let Some(dir) = current {
        if dir == natives_dir || !dir.starts_with(natives_dir) {
            break;
        }
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
        current = dir.parent();
    }
}

/// Replace/add desired files, then drop files that the current native JARs no
/// longer ship. Unknown files are only removed when a previous manifest (or,
/// on the first run after an upgrade, the existing directory) owns them.
fn commit_natives(staging: &Path, natives_dir: &Path, desired: &[String]) -> Result<()> {
    std::fs::create_dir_all(natives_dir)?;

    // Before touching anything, prove that every file we would replace is
    // writable. A DLL loaded by another running instance fails here instead of
    // leaving a half-updated directory.
    for relative in desired {
        let source = staging.join(relative);
        let destination = natives_dir.join(relative);
        if destination.is_file() {
            if files_equal(&source, &destination) {
                continue;
            }
            std::fs::OpenOptions::new()
                .write(true)
                .open(&destination)
                .map_err(|e| {
                    LauncherError::Version(format!(
                        "Cannot replace native file {}: {}. Close any running game that uses this version and try again.",
                        destination.display(),
                        e
                    ))
                })?;
        } else if destination.exists() {
            return Err(LauncherError::Version(format!(
                "Cannot replace native path {}: it is not a regular file",
                destination.display()
            )));
        }
    }

    for relative in desired {
        let source = staging.join(relative);
        let destination = natives_dir.join(relative);
        if destination.is_file() && files_equal(&source, &destination) {
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        install_native_file(&source, &destination)?;
        tracing::debug!(target: "launcher", "Extracted native: {}", destination.display());
    }

    let previous = match read_natives_manifest(natives_dir) {
        Some(files) => files,
        // No manifest yet (install from an older launcher): the natives
        // directory is launcher-owned, so treat its contents as ours once.
        None => collect_relative_files(natives_dir)?,
    };
    let desired_set: std::collections::HashSet<&str> =
        desired.iter().map(|file| file.as_str()).collect();
    let mut retained = Vec::new();

    for relative in previous {
        if desired_set.contains(relative.as_str())
            || relative.is_empty()
            || crate::download::is_unsafe_archive_path(&relative)
        {
            continue;
        }
        let destination = natives_dir.join(&relative);
        match std::fs::remove_file(&destination) {
            Ok(()) => {
                tracing::debug!(target: "launcher", "Removed stale native: {}", destination.display());
                prune_empty_native_dirs(&destination, natives_dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // A file still mapped by a running game cannot be deleted.
                // Keep it in the manifest so the next launch retries.
                tracing::warn!(
                    target: "launcher",
                    "Could not remove stale native file {}: {}. It will be retried on the next launch.",
                    destination.display(),
                    e
                );
                retained.push(relative);
            }
        }
    }

    let mut managed = desired.to_vec();
    managed.extend(retained);
    write_natives_manifest(natives_dir, &managed)
}

/// Extract native libraries (DLLs etc.) from the native classifier JARs into
/// the natives directory so they can be found via -Djava.library.path.
/// This mirrors the official launcher behavior and also covers old LWJGL
/// versions that do not load natives from the classpath.
///
/// Extraction happens in a staging directory first: a corrupt archive, a read
/// failure or a locked destination aborts preparation and leaves the live
/// directory untouched. After a successful extraction, files from an older
/// native set are removed, so an outdated DLL can never shadow the current one.
///
/// The classifier path is validated before it is read: a crafted metadata
/// entry must not make the launcher open a file outside `libraries_dir`.
pub fn extract_natives(
    version_info: &VersionInfo,
    libraries_dir: &Path,
    natives_dir: &Path,
) -> Result<()> {
    let staging = natives_staging_dir(natives_dir);
    let _ = std::fs::remove_dir_all(&staging);

    let result = (|| {
        let mut desired = extract_natives_into(version_info, libraries_dir, &staging)?;
        desired.sort();
        desired.dedup();
        commit_natives(&staging, natives_dir, &desired)
    })();

    let _ = std::fs::remove_dir_all(&staging);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library_with_natives(natives: &str) -> Library {
        Library {
            name: "org.lwjgl.lwjgl:lwjgl-platform:2.9.4".to_string(),
            url: None,
            rules: None,
            natives: Some(serde_json::json!({ "windows": natives })),
            downloads: None,
        }
    }

    #[test]
    fn native_classifier_substitutes_arch() {
        let lib = library_with_natives("natives-windows-${arch}");
        let classifier = native_classifier(&lib).expect("classifier expected");
        assert_eq!(classifier, "natives-windows-64");
    }

    #[test]
    fn native_classifier_plain_windows() {
        let lib = library_with_natives("natives-windows");
        assert_eq!(native_classifier(&lib).as_deref(), Some("natives-windows"));
    }

    #[test]
    fn native_classifier_missing_os() {
        let lib = Library {
            name: "org.lwjgl.lwjgl:lwjgl-platform:2.9.4".to_string(),
            url: None,
            rules: None,
            natives: Some(serde_json::json!({ "osx": "natives-osx" })),
            downloads: None,
        };
        assert!(native_classifier(&lib).is_none());
    }

    fn lib_with_rules(rules: Option<Vec<Rule>>) -> Library {
        Library {
            name: "org.lwjgl:lwjgl:3.2.1".to_string(),
            url: None,
            rules,
            natives: None,
            downloads: None,
        }
    }

    #[test]
    fn library_without_rules_is_included() {
        assert!(should_include_library(&lib_with_rules(None)));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn osx_only_library_is_excluded_on_other_os() {
        // 1.16.x ships LWJGL twice: 3.2.1 (allow:osx only) and 3.2.2
        // (allow + disallow:osx). On Windows the macOS variant must be
        // excluded, otherwise both versions land on the classpath and the
        // mismatched natives crash with EXCEPTION_ACCESS_VIOLATION.
        let rules = Some(vec![Rule {
            action: "allow".to_string(),
            os: Some(OsRule {
                name: Some("osx".to_string()),
                arch: None,
            }),
        }]);
        assert!(!should_include_library(&lib_with_rules(rules)));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn cross_platform_library_is_included_on_non_macos() {
        let rules = Some(vec![
            Rule {
                action: "allow".to_string(),
                os: None,
            },
            Rule {
                action: "disallow".to_string(),
                os: Some(OsRule {
                    name: Some("osx".to_string()),
                    arch: None,
                }),
            },
        ]);
        assert!(should_include_library(&lib_with_rules(rules)));
    }

    #[test]
    fn asset_index_parses_virtual_and_map_flags() {
        let json = r#"{
            "map_to_resources": true,
            "objects": {
                "sound/a.ogg": { "hash": "abc123", "size": 10, "virtual": true },
                "texts/b.txt": { "hash": "def456", "size": 20 }
            }
        }"#;
        let index: AssetIndexData = serde_json::from_str(json).unwrap();
        assert!(index.map_to_resources);
        assert!(index.objects["sound/a.ogg"].is_virtual);
        assert!(!index.objects["texts/b.txt"].is_virtual);
    }

    #[test]
    fn arguments_strings_pass_through() {
        let args = serde_json::json!([
            "--username",
            "${auth_player_name}",
            "--version",
            "${version_name}"
        ]);
        let out = evaluate_arguments(args.as_array().unwrap(), false);
        assert_eq!(
            out,
            vec![
                "--username",
                "${auth_player_name}",
                "--version",
                "${version_name}"
            ]
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn arguments_object_os_rules_are_evaluated() {
        // macOS-only arg must be dropped on Windows/Linux
        let args = serde_json::json!([
            "--gameDir", "${game_directory}",
            { "rules": [{ "action": "allow", "os": { "name": "osx" } }], "value": "-XstartOnFirstThread" }
        ]);
        let out = evaluate_arguments(args.as_array().unwrap(), false);
        assert_eq!(out, vec!["--gameDir", "${game_directory}"]);
    }

    #[test]
    fn arguments_object_value_array_expands() {
        let args = serde_json::json!([
            {
                "rules": [{ "action": "allow", "features": { "has_custom_resolution": true } }],
                "value": ["--width", "${resolution_width}", "--height", "${resolution_height}"]
            }
        ]);
        // Feature disabled → skipped
        let out = evaluate_arguments(args.as_array().unwrap(), false);
        assert!(out.is_empty());
        // Feature enabled → array expands
        let out = evaluate_arguments(args.as_array().unwrap(), true);
        assert_eq!(
            out,
            vec![
                "--width",
                "${resolution_width}",
                "--height",
                "${resolution_height}"
            ]
        );
    }

    #[test]
    fn legacy_arguments_split_respects_quotes() {
        let out = split_mc_arguments(r#"--username ${auth_player_name} --demo --foo "some value""#);
        assert_eq!(
            out,
            vec![
                "--username",
                "${auth_player_name}",
                "--demo",
                "--foo",
                "some value"
            ]
        );
    }

    // ── Remote-metadata path validation ─────────────────────────

    fn version_info_with_libraries(libraries: serde_json::Value) -> VersionInfo {
        serde_json::from_value(serde_json::json!({
            "id": "1.20.1",
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "libraries": libraries,
            "downloads": {
                "client": {
                    "sha1": "",
                    "size": 0,
                    "url": "https://piston-data.mojang.com/v1/objects/client.jar"
                }
            },
            "assetIndex": {
                "id": "5",
                "sha1": "",
                "size": 0,
                "totalSize": 0,
                "url": "https://piston-meta.mojang.com/v1/packages/5.json"
            },
            "assets": "5"
        }))
        .expect("minimal version info must deserialize")
    }

    #[test]
    fn collect_downloads_rejects_unsafe_client_version_id() {
        let mut vi = version_info_with_libraries(serde_json::json!([]));
        vi.id = "../outside".into();
        let err = collect_downloads(&vi, Path::new("libraries"), Path::new("versions"))
            .expect_err("unsafe version id must fail");
        assert!(err.to_string().contains("version id"), "got: {}", err);
    }

    #[test]
    fn collect_downloads_rejects_unsafe_library_path() {
        let vi = version_info_with_libraries(serde_json::json!([{
            "name": "com.example:evil:1.0",
            "downloads": {
                "artifact": {
                    "path": "../../evil.jar",
                    "sha1": "",
                    "size": 1,
                    "url": "https://libraries.minecraft.net/evil.jar"
                }
            }
        }]));
        let err = collect_downloads(&vi, Path::new("libraries"), Path::new("versions"))
            .expect_err("traversal path must fail");
        assert!(err.to_string().contains("Unsafe path"), "got: {}", err);
    }

    #[test]
    fn collect_downloads_rejects_unsafe_maven_fallback_name() {
        let vi = version_info_with_libraries(serde_json::json!([{
            "name": "com.example:..:1.0",
            "url": "https://libraries.minecraft.net/"
        }]));
        let err = collect_downloads(&vi, Path::new("libraries"), Path::new("versions"))
            .expect_err("traversal in maven name must fail");
        assert!(err.to_string().contains("Unsafe path"), "got: {}", err);
    }

    #[test]
    fn collect_downloads_keeps_safe_paths_under_libraries() {
        let vi = version_info_with_libraries(serde_json::json!([{
            "name": "com.example:good:1.0",
            "downloads": {
                "artifact": {
                    "path": "com/example/good/1.0/good-1.0.jar",
                    "sha1": "",
                    "size": 1,
                    "url": "https://libraries.minecraft.net/good.jar"
                }
            }
        }]));
        let files = collect_downloads(&vi, Path::new("libraries"), Path::new("versions"))
            .expect("safe metadata must pass");
        assert_eq!(files.len(), 2, "client jar + one library");
        assert!(files[1].1.starts_with(Path::new("libraries")));
        assert!(files[1].1.ends_with("good-1.0.jar"));
    }

    #[test]
    fn build_classpath_rejects_unsafe_library_path() {
        let vi = version_info_with_libraries(serde_json::json!([{
            "name": "com.example:evil:1.0",
            "downloads": {
                "artifact": {
                    "path": "../../../evil.jar",
                    "sha1": "",
                    "size": 1,
                    "url": "https://libraries.minecraft.net/evil.jar"
                }
            }
        }]));
        let err = build_classpath(
            &vi,
            Path::new("libraries"),
            Path::new("versions/1.20.1/client.jar"),
        )
        .expect_err("traversal path must fail");
        assert!(err.to_string().contains("Unsafe path"), "got: {}", err);
    }

    #[test]
    fn build_classpath_fails_when_required_library_is_missing() {
        let vi = version_info_with_libraries(serde_json::json!([{
            "name": "com.example:missing:1.0",
            "downloads": {
                "artifact": {
                    "path": "com/example/missing/1.0/missing-1.0.jar",
                    "sha1": "",
                    "size": 1,
                    "url": "https://libraries.minecraft.net/missing.jar"
                }
            }
        }]));
        let err = build_classpath(
            &vi,
            Path::new("libraries"),
            Path::new("versions/1.20.1/client.jar"),
        )
        .expect_err("missing required library must fail");
        assert!(err.to_string().contains("missing"), "got: {}", err);
    }
}

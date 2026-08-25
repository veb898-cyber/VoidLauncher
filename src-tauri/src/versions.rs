use serde::{Deserialize, Serialize};
use crate::error::{Result, LauncherError};
use std::path::PathBuf;

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

/// Fetch version manifest from Mojang
pub async fn fetch_version_manifest() -> Result<VersionManifest> {
    tracing::info!(target: "launcher", "Fetching version manifest from Mojang");
    let client = crate::download::global_http_client();
    let manifest = crate::download::send_with_fallback(
        client.get("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"),
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
    let client = crate::download::global_http_client();
    let info = crate::download::send_with_fallback(client.get(url))
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
    let client = crate::download::global_http_client();
    let index = crate::download::send_with_fallback(client.get(url))
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
                    Some(arch) => arch == current_arch || (arch == "x86" && (current_arch == "x86" || current_arch == "x86_64")),
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

/// Return the native classifier for the current OS (e.g. "natives-windows")
/// by reading the library's `natives` map, or None if not applicable.
fn native_classifier(lib: &Library) -> Option<String> {
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

/// Build classpath from version info libraries, including native classifier JARs.
pub fn build_classpath(version_info: &VersionInfo, libraries_dir: &PathBuf, client_jar: &PathBuf) -> String {
    let mut classpath_entries: Vec<String> = Vec::new();

    for lib in &version_info.libraries {
        if !should_include_library(lib) {
            continue;
        }

        if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let lib_path = libraries_dir.join(&artifact.path);
                if lib_path.exists() {
                    classpath_entries.push(lib_path.to_string_lossy().to_string());
                }
            }
            // Add native classifier JAR (e.g. lwjgl-3.2.1-natives-windows.jar)
            if let Some(classifier) = native_classifier(lib) {
                if let Some(classifiers) = &downloads.classifiers {
                    if let Some(native_artifact) = classifiers.get(classifier.as_str()) {
                        if let Some(path) = native_artifact.get("path").and_then(|p| p.as_str()) {
                            let native_path = libraries_dir.join(path);
                            if native_path.exists() {
                                classpath_entries.push(native_path.to_string_lossy().to_string());
                            }
                        }
                    }
                }
            }
        } else {
            let path = maven_to_path(&lib.name);
            let lib_path = libraries_dir.join(&path);
            if lib_path.exists() {
                classpath_entries.push(lib_path.to_string_lossy().to_string());
            }
        }
    }

    classpath_entries.push(client_jar.to_string_lossy().to_string());

    classpath_entries.join(";")
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
fn expand_argument_value(value: &serde_json::Value, has_custom_resolution: bool, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                expand_argument_value(item, has_custom_resolution, out);
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
                expand_argument_value(value, has_custom_resolution, &mut out);
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
pub fn collect_downloads(
    version_info: &VersionInfo,
    libraries_dir: &PathBuf,
    versions_dir: &PathBuf,
) -> Vec<(String, PathBuf, String, u64)> {
    // Returns: (url, local_path, sha1, size)
    let mut files = Vec::new();

    // Client JAR
    let client_path = versions_dir
        .join(&version_info.id)
        .join("client.jar");
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
                let lib_path = libraries_dir.join(&artifact.path);
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
                            let native_path = libraries_dir.join(path);
                            if !native_path.exists() {
                                files.push((
                                    url.to_string(),
                                    native_path,
                                    sha1.to_string(),
                                    size,
                                ));
                            }
                        }
                    }
                }
            }
        } else if let Some(url_base) = &lib.url {
            let path = maven_to_path(&lib.name);
            let lib_path = libraries_dir.join(&path);
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

    files
}

/// Load version info from file
pub fn version_info_from_file(path: &PathBuf) -> Result<VersionInfo> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| LauncherError::Version(format!("Failed to read version file: {}", e)))?;
    
    let info: VersionInfo = serde_json::from_str(&contents)
        .map_err(|e| LauncherError::Version(format!("Failed to parse version JSON: {}", e)))?;
    
    Ok(info)
}

/// Load a saved asset index (e.g. `indexes/legacy.json`) from disk
pub fn load_asset_index(path: &PathBuf) -> Result<AssetIndexData> {
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
    libraries_dir: &PathBuf,
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
                            native_artifact.get("sha1").and_then(|s| s.as_str()).unwrap_or(""),
                        ) {
                            let native_path = libraries_dir.join(path);
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
    for (url, path, sha1) in &missing {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        crate::download::download_file(url, path, sha1).await.map_err(|e| {
            LauncherError::Version(format!("Failed to download native library {}: {}", url, e))
        })?;
    }

    Ok(())
}

/// Extract native libraries (DLLs etc.) from the native classifier JARs into
/// the natives directory so they can be found via -Djava.library.path.
/// This mirrors the official launcher behavior and also covers old LWJGL
/// versions that do not load natives from the classpath.
pub fn extract_natives(version_info: &VersionInfo, libraries_dir: &PathBuf, natives_dir: &PathBuf) {
    use std::io::Read;

    std::fs::create_dir_all(natives_dir).ok();

    for lib in &version_info.libraries {
        if !should_include_library(lib) {
            continue;
        }
        if let Some(classifier) = native_classifier(lib) {
            if let Some(downloads) = &lib.downloads {
                if let Some(classifiers) = &downloads.classifiers {
                    if let Some(native_artifact) = classifiers.get(classifier.as_str()) {
                        if let Some(path) = native_artifact.get("path").and_then(|p| p.as_str()) {
                            let jar_path = libraries_dir.join(path);
                            if !jar_path.exists() {
                                continue;
                            }
                            let Ok(bytes) = std::fs::read(&jar_path) else { continue };
                            let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes))
                            else { continue };
                            for i in 0..archive.len() {
                                let Ok(mut entry) = archive.by_index(i) else { continue };
                                let name = entry.name().replace('\\', "/");
                                if name.starts_with("META-INF/") || name.ends_with('/') {
                                    continue;
                                }
                                let dest = natives_dir.join(&name);
                                if dest.exists() {
                                    continue;
                                }
                                if let Some(parent) = dest.parent() {
                                    std::fs::create_dir_all(parent).ok();
                                }
                                if let Ok(mut file) = std::fs::File::create(&dest) {
                                    use std::io::Write;
                                    let mut buf = Vec::new();
                                    if entry.read_to_end(&mut buf).is_ok() {
                                        let _ = file.write_all(&buf);
                                        tracing::debug!(target: "launcher", "Extracted native: {}", dest.display());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
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
            os: Some(OsRule { name: Some("osx".to_string()), arch: None }),
        }]);
        assert!(!should_include_library(&lib_with_rules(rules)));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn cross_platform_library_is_included_on_non_macos() {
        let rules = Some(vec![
            Rule { action: "allow".to_string(), os: None },
            Rule {
                action: "disallow".to_string(),
                os: Some(OsRule { name: Some("osx".to_string()), arch: None }),
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
            "--username", "${auth_player_name}", "--version", "${version_name}"
        ]);
        let out = evaluate_arguments(args.as_array().unwrap(), false);
        assert_eq!(out, vec!["--username", "${auth_player_name}", "--version", "${version_name}"]);
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
        assert_eq!(out, vec!["--width", "${resolution_width}", "--height", "${resolution_height}"]);
    }

    #[test]
    fn legacy_arguments_split_respects_quotes() {
        let out = split_mc_arguments(r#"--username ${auth_player_name} --demo --foo "some value""#);
        assert_eq!(out, vec![
            "--username", "${auth_player_name}", "--demo", "--foo", "some value"
        ]);
    }
}

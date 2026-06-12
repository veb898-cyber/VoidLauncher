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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

/// Fetch version manifest from Mojang
pub async fn fetch_version_manifest() -> Result<VersionManifest> {
    tracing::info!(target: "launcher", "Fetching version manifest from Mojang");
    let client = crate::download::global_http_client();
    let manifest = client
        .get("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")
        .send()
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
    let info = client.get(url).send().await
        .map_err(|e| {
            tracing::error!(target: "launcher", "Failed to fetch version info: {}", e);
            e
        })?
        .json::<VersionInfo>().await
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
    let index = client.get(url).send().await
        .map_err(|e| {
            tracing::error!(target: "launcher", "Failed to fetch asset index: {}", e);
            e
        })?
        .json::<AssetIndexData>().await
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

    let mut dominated_action = "allow";

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

/// Build classpath from version info libraries
pub fn build_classpath(version_info: &VersionInfo, libraries_dir: &PathBuf, client_jar: &PathBuf) -> String {
    let mut classpath_entries: Vec<String> = Vec::new();

    for lib in &version_info.libraries {
        if !should_include_library(lib) {
            continue;
        }

        let lib_path = if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                libraries_dir.join(&artifact.path)
            } else {
                continue;
            }
        } else {
            let path = maven_to_path(&lib.name);
            libraries_dir.join(path)
        };

        if lib_path.exists() {
            classpath_entries.push(lib_path.to_string_lossy().to_string());
        }
    }

    classpath_entries.push(client_jar.to_string_lossy().to_string());

    classpath_entries.join(";")
}

/// Extract game arguments from version info (handles both old and new format)
pub fn get_game_arguments(version_info: &VersionInfo) -> Vec<String> {
    if let Some(arguments) = &version_info.arguments {
        arguments
            .game
            .iter()
            .filter_map(|arg| arg.as_str().map(|s| s.to_string()))
            .collect()
    } else if let Some(mc_args) = &version_info.minecraft_arguments {
        mc_args.split_whitespace().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    }
}

/// Extract JVM arguments from version info
pub fn get_jvm_arguments(version_info: &VersionInfo) -> Vec<String> {
    if let Some(arguments) = &version_info.arguments {
        arguments
            .jvm
            .iter()
            .filter_map(|arg| arg.as_str().map(|s| s.to_string()))
            .collect()
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

/// Collect all files that need to be downloaded for a version
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
        .join(format!("{}.jar", version_info.id));
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

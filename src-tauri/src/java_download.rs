use crate::error::{LauncherError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Managed Java runtime info
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ManagedJavaRuntime {
    pub major_version: u32,
    pub path: PathBuf,
    pub version: String,
    pub vendor: String,
    pub is_64bit: bool,
}

/// Available Java version from Adoptium API
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AvailableJavaVersion {
    pub major_version: u32,
    pub label: String,
}

/// Adoptium API response
#[derive(Debug, Deserialize)]
struct AdoptiumVersionData {
    #[serde(rename = "release_name")]
    version: String,
    binaries: Vec<AdoptiumBinary>,
}

#[derive(Debug, Deserialize)]
struct AdoptiumBinary {
    architecture: String,
    #[serde(rename = "os")]
    os_name: String,
    #[serde(rename = "image_type")]
    image_type: String,
    package: Option<AdoptiumPackage>,
    installer: Option<AdoptiumPackage>,
}

#[derive(Debug, Deserialize)]
struct AdoptiumPackage {
    link: String,
    name: String,
}

const ADOPTIUM_API: &str = "https://api.adoptium.net/v3";
const MANAGED_JAVA_DIR: &str = "java";

/// List Java versions available for download from Adoptium
pub async fn list_available_java_versions() -> Result<Vec<AvailableJavaVersion>> {
    let supported = [8u32, 11, 17, 21, 25];
    let client = crate::download::global_http_client();
    let mut versions = Vec::new();

    for major in &supported {
        let url = format!(
            "{}/assets/feature_releases/{}/ga?architecture=x64&image_type=jdk&os=windows&vendor=eclipse&page_size=1",
            ADOPTIUM_API, major
        );

        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };

        if !resp.status().is_success() {
            continue;
        }

        let data: Vec<AdoptiumVersionData> = match resp.json().await {
            Ok(d) => d,
            Err(_) => continue,
        };

        if let Some(entry) = data.into_iter().next() {
            let version_str = entry.version
                .trim_start_matches("jdk-")
                .trim_start_matches("jre-")
                .to_string();
            let label = format!("Java {} ({})", major, version_str);
            tracing::debug!(target: "launcher", "Found available Java version: {}", label);
            versions.push(AvailableJavaVersion {
                major_version: *major,
                label,
            });
        }
    }

    Ok(versions)
}

/// Download and install a Java runtime by major version
pub async fn download_java_runtime(
    major_version: u32,
    data_dir: &PathBuf,
) -> Result<ManagedJavaRuntime> {
    let java_dir = data_dir.join(MANAGED_JAVA_DIR);
    let runtime_dir = java_dir.join(format!("jdk-{}", major_version));
    let extract_marker = runtime_dir.join(".extracted");

    tracing::info!(target: "launcher", "Starting download of Java {} runtime", major_version);

    // If already extracted and valid, return it
    if extract_marker.exists() {
        if let Some(java_exe) = find_java_in_dir(&runtime_dir) {
            if let Some(install) = crate::java::probe_java_by_path(&java_exe) {
                return Ok(ManagedJavaRuntime {
                    major_version,
                    path: java_exe,
                    version: install.version,
                    vendor: install.vendor,
                    is_64bit: install.is_64bit,
                });
            }
        }
        // Probing failed, re-download
        let _ = std::fs::remove_dir_all(&runtime_dir);
    }

    std::fs::create_dir_all(&runtime_dir)?;

    let client = crate::download::global_http_client();

    // Find the download URL for this version
    let url = format!(
        "{}/assets/feature_releases/{}/ga?architecture=x64&image_type=jdk&os=windows&vendor=eclipse&page_size=1",
        ADOPTIUM_API, major_version
    );

    let resp = client.get(&url).send().await.map_err(|e| {
        tracing::error!(target: "launcher", "Adoptium API request failed: {}", e);
        LauncherError::Download(format!("Adoptium API error: {}", e))
    })?;

    let data: Vec<AdoptiumVersionData> = resp.json().await.map_err(|e| {
        tracing::error!(target: "launcher", "Failed to parse Adoptium response: {}", e);
        LauncherError::Download(format!("Adoptium parse error: {}", e))
    })?;

    let version_entry = data.into_iter().next().ok_or_else(|| {
        tracing::error!(target: "launcher", "No Java {} release found", major_version);
        LauncherError::Download(format!("No Java {} release found", major_version))
    })?;

    // Find the Windows x64 JDK package (always prefer zip for extraction)
    let pkg = version_entry
        .binaries
        .iter()
        .find(|b| b.os_name == "windows" && b.architecture == "x64" && b.image_type == "jdk")
        .and_then(|b| b.package.as_ref().or(b.installer.as_ref()))
        .ok_or_else(|| {
            LauncherError::Download(format!(
                "No Windows x64 JDK package for Java {}",
                major_version
            ))
        })?;

    let archive_path = java_dir.join(&pkg.name);

    // Download the archive
    let response = client
        .get(&pkg.link)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(target: "launcher", "Failed to download Java {}: {}", major_version, e);
            LauncherError::Download(format!("Failed to download Java: {}", e))
        })?;

    let status = response.status();
    let content_type = response.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    tracing::info!(target: "launcher", "Java {} download response: status={}, content_type={}", major_version, status, content_type);

    if !status.is_success() {
        let msg = format!("Java {} download failed with HTTP {}", major_version, status);
        tracing::error!(target: "launcher", "{}", msg);
        return Err(LauncherError::Download(msg));
    }

    let bytes = response.bytes().await.map_err(|e| {
        tracing::error!(target: "launcher", "Failed to read Java download bytes: {}", e);
        LauncherError::Download(format!("Failed to read Java download: {}", e))
    })?;

    std::fs::write(&archive_path, &bytes)?;

    // Extract
    extract_archive(&archive_path, &runtime_dir)?;

    // Clean up archive
    let _ = std::fs::remove_file(&archive_path);

    // Write extraction marker
    std::fs::write(&extract_marker, b"1")?;

    // Find java.exe and probe it
    let java_exe = find_java_in_dir(&runtime_dir).ok_or_else(|| {
        tracing::error!(target: "launcher", "java.exe not found after extraction of Java {}", major_version);
        LauncherError::Download("Failed to find java.exe after extraction".to_string())
    })?;

    let install = crate::java::probe_java_by_path(&java_exe).ok_or_else(|| {
        tracing::error!(target: "launcher", "Failed to verify extracted Java {} runtime", major_version);
        LauncherError::Download("Failed to verify extracted Java runtime".to_string())
    })?;

    tracing::info!(target: "launcher", "Successfully installed Java {} ({})", major_version, install.version);
    Ok(ManagedJavaRuntime {
        major_version,
        path: java_exe,
        version: install.version,
        vendor: install.vendor,
        is_64bit: install.is_64bit,
    })
}

/// List already-downloaded Java runtimes
pub fn list_managed_java(data_dir: &PathBuf) -> Vec<ManagedJavaRuntime> {
    let java_dir = data_dir.join(MANAGED_JAVA_DIR);
    if !java_dir.exists() {
        return Vec::new();
    }

    let mut runtimes = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&java_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let marker = path.join(".extracted");
            if !marker.exists() {
                continue;
            }
            if let Some(java_exe) = find_java_in_dir(&path) {
                if let Some(install) = crate::java::probe_java_by_path(&java_exe) {
                    runtimes.push(ManagedJavaRuntime {
                        major_version: install.major_version,
                        path: java_exe,
                        version: install.version,
                        vendor: install.vendor,
                        is_64bit: install.is_64bit,
                    });
                }
            }
        }
    }
    runtimes
}

/// Remove a managed Java runtime
pub fn remove_managed_java(major_version: u32, data_dir: &PathBuf) -> Result<()> {
    let dir = data_dir
        .join(MANAGED_JAVA_DIR)
        .join(format!("jdk-{}", major_version));
    if dir.exists() {
        tracing::info!(target: "launcher", "Removing managed Java {} runtime", major_version);
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

fn find_java_in_dir(dir: &PathBuf) -> Option<PathBuf> {
    let direct = dir.join("bin").join("java.exe");
    if direct.exists() {
        return Some(direct);
    }
    // Check first subdirectory (e.g. jdk-21.0.1+12/bin/java.exe)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if sub.is_dir() {
                let java = sub.join("bin").join("java.exe");
                if java.exists() {
                    return Some(java);
                }
            }
        }
    }
    None
}

fn extract_archive(archive_path: &PathBuf, dest_dir: &PathBuf) -> Result<()> {
    let file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| LauncherError::Download(format!("Invalid archive: {}", e)))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Download(format!("Archive entry error: {}", e)))?;

        let full_name = entry.name().to_string();

        // Strip top-level directory from the path
        let relative = match full_name.split_once('/') {
            Some((_, rest)) if !rest.is_empty() => rest.to_string(),
            _ => continue,
        };

        let out_path = dest_dir.join(&relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut outfile = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut outfile)?;
        }
    }

    Ok(())
}

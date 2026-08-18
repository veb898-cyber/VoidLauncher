use crate::error::{LauncherError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Uncomment to always return hardcoded versions (skip API)
// const FORCE_STATIC_VERSIONS: bool = true;

#[derive(Debug, Clone, Serialize)]
pub struct JavaDownloadProgress {
    pub major_version: u32,
    pub percent: f64,
    pub stage: String,
    pub message: String,
}

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

/// HTTP client without auto-decompression — prevents "error decoding response body"
/// when the server sends brotli/deflate despite us not requesting it.
/// Rebuilt automatically when the proxy setting changes.
fn download_client() -> reqwest::Client {
    static CLIENT: OnceLock<Mutex<Option<(Option<String>, reqwest::Client)>>> = OnceLock::new();
    let proxy = crate::download::configured_proxy();
    let mut slot = CLIENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap();
    if slot.as_ref().map(|(p, _)| p != &proxy).unwrap_or(true) {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(1800))
            .connect_timeout(Duration::from_secs(30))
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .redirect(crate::download::redirect_policy())
            .user_agent(concat!("VoidLauncher/", env!("CARGO_PKG_VERSION")));
        if let Some(p) = &proxy {
            if let Ok(pp) = reqwest::Proxy::all(p.clone()) {
                builder = builder.proxy(pp);
            }
        }
        *slot = Some((
            proxy,
            builder
                .build()
                .expect("Failed to create Java download client (check TLS libraries)"),
        ));
    }
    slot.as_ref().unwrap().1.clone()
}

/// Adoptium /info/available_releases response (subset of fields)
#[derive(Debug, Deserialize)]
struct AdoptiumReleasesInfo {
    available_releases: Vec<u32>,
}

/// In-memory cache so reopening settings does not re-hit the API (like Prism Launcher)
const JAVA_LIST_CACHE_TTL: Duration = Duration::from_secs(600);

static JAVA_LIST_CACHE: OnceLock<Mutex<Option<(Instant, Vec<AvailableJavaVersion>)>>> = OnceLock::new();

fn java_list_cache() -> &'static Mutex<Option<(Instant, Vec<AvailableJavaVersion>)>> {
    JAVA_LIST_CACHE.get_or_init(|| Mutex::new(None))
}

/// Java versions actually used by Minecraft Java Edition:
/// 8  - up to 1.16.5
/// 16 - 1.17 .. 1.17.1
/// 17 - 1.18 .. 1.20.4
/// 21 - 1.20.5 .. 26.1
/// 25 - 26.2 and newer (per minecraft.wiki system requirements)
const MC_SUPPORTED_JAVA: [u32; 5] = [8, 16, 17, 21, 25];

/// List Java versions available for download from Adoptium.
/// Uses ONLY the lightweight /info/available_releases endpoint. The
/// /v3/assets/... endpoint gets throttled ~30-40s on some networks (RKN/DPI),
/// so it is deliberately NOT used for listing; exact build labels are
/// resolved later by the download flow itself.
pub async fn list_available_java_versions() -> Result<Vec<AvailableJavaVersion>> {
    if let Some((fetched_at, versions)) = java_list_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        if fetched_at.elapsed() < JAVA_LIST_CACHE_TTL {
            tracing::debug!(target: "launcher", "Returning cached Java version list ({} entries)", versions.len());
            return Ok(versions.clone());
        }
    }

    let client = download_client();

    let supported: Vec<u32> = {
        let url = format!("{}/info/available_releases", ADOPTIUM_API);
        let result = tokio::time::timeout(Duration::from_secs(15), client.get(&url).send()).await;
        let releases = match result {
            Ok(Ok(resp)) if resp.status().is_success() => match resp.json::<AdoptiumReleasesInfo>().await {
                Ok(info) => Some(info.available_releases),
                Err(e) => {
                    tracing::warn!(target: "launcher", "Failed to parse Adoptium releases info: {}", e);
                    None
                }
            },
            Ok(Ok(resp)) => {
                tracing::warn!(target: "launcher", "Adoptium releases info returned HTTP {}", resp.status());
                None
            }
            Ok(Err(e)) => {
                tracing::warn!(target: "launcher", "Adoptium releases info error: {}", e);
                None
            }
            Err(_) => {
                tracing::warn!(target: "launcher", "Timeout fetching Adoptium releases info (15s)");
                None
            }
        };
        match releases {
            Some(list) => {
                let matching: Vec<u32> = MC_SUPPORTED_JAVA
                    .iter()
                    .copied()
                    .filter(|major| list.contains(major))
                    .collect();
                tracing::info!(target: "launcher", "Adoptium releases matching Minecraft's Java needs: {:?}", matching);
                if matching.is_empty() {
                    tracing::warn!(target: "launcher", "No matching releases from Adoptium, falling back to static list");
                    MC_SUPPORTED_JAVA.to_vec()
                } else {
                    matching
                }
            }
            None => MC_SUPPORTED_JAVA.to_vec(),
        }
    };

    let versions: Vec<AvailableJavaVersion> = supported
        .into_iter()
        .map(|major| AvailableJavaVersion {
            major_version: major,
            label: format!("Java {}", major),
        })
        .collect();

    *java_list_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((Instant::now(), versions.clone()));
    Ok(versions)
}

/// Download and install a Java runtime by major version
pub async fn download_java_runtime(
    major_version: u32,
    data_dir: &PathBuf,
    app: &AppHandle,
) -> Result<ManagedJavaRuntime> {
    let java_dir = data_dir.join(MANAGED_JAVA_DIR);
    let runtime_dir = java_dir.join(format!("jdk-{}", major_version));
    let extract_marker = runtime_dir.join(".extracted");

    let emit_progress = |percent: f64, stage: &str, message: &str| {
        let _ = app.emit("java_download_progress", JavaDownloadProgress {
            major_version,
            percent,
            stage: stage.to_string(),
            message: message.to_string(),
        });
    };

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
        let _ = std::fs::remove_dir_all(&runtime_dir);
    }

    std::fs::create_dir_all(&runtime_dir)?;

    let client = download_client();

    // Phase 1: Resolve download URL. The /assets endpoint is sometimes
    // throttled ~30-40s on certain networks, so retry a few times.
    emit_progress(5.0, "resolving", "Querying Adoptium API...");

    let url = format!(
        "{}/assets/feature_releases/{}/ga?architecture=x64&image_type=jdk&os=windows&vendor=eclipse&page_size=1",
        ADOPTIUM_API, major_version
    );

    let mut resolved = None;
    for attempt in 1..=3 {
        match tokio::time::timeout(Duration::from_secs(90), client.get(&url).send()).await {
            Ok(Ok(resp)) if resp.status().is_success() => {
                resolved = Some(resp);
                break;
            }
            Ok(Ok(resp)) => tracing::warn!(target: "launcher", "Adoptium API returned HTTP {} (attempt {}/3)", resp.status(), attempt),
            Ok(Err(e)) => tracing::warn!(target: "launcher", "Adoptium API request failed (attempt {}/3): {}", attempt, e),
            Err(_) => tracing::warn!(target: "launcher", "Adoptium API request timed out (attempt {}/3)", attempt),
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    let resp = resolved.ok_or_else(|| {
        tracing::error!(target: "launcher", "Adoptium API unreachable for Java {}", major_version);
        LauncherError::Download(format!("Adoptium API error for Java {}", major_version))
    })?;

    let data: Vec<AdoptiumVersionData> = resp.json().await.map_err(|e| {
        tracing::error!(target: "launcher", "Failed to parse Adoptium response: {}", e);
        LauncherError::Download(format!("Adoptium parse error: {}", e))
    })?;

    let version_entry = data.into_iter().next().ok_or_else(|| {
        tracing::error!(target: "launcher", "No Java {} release found", major_version);
        LauncherError::Download(format!("No Java {} release found", major_version))
    })?;

    let pkg = version_entry
        .binaries
        .iter()
        .find(|b| b.os_name == "windows" && b.architecture == "x64" && b.image_type == "jdk")
        .and_then(|b| b.package.as_ref().or(b.installer.as_ref()))
        .ok_or_else(|| {
            LauncherError::Download(format!("No Windows x64 JDK package for Java {}", major_version))
        })?;

    let archive_path = java_dir.join(&pkg.name);
    let pkg_name = pkg.name.clone();
    let pkg_link = pkg.link.clone();

    // Phase 2: Download
    emit_progress(10.0, "downloading", &format!("Downloading {}...", pkg_name));

    let response = client.get(&pkg_link).send().await.map_err(|e| {
        tracing::error!(target: "launcher", "Failed to download Java {}: {}", major_version, e);
        LauncherError::Download(format!("Failed to download Java: {}", e))
    })?;

    let status = response.status();
    let total_size = response.content_length().unwrap_or(0);

    tracing::info!(target: "launcher", "Java {} download response: status={}, total_size={}", major_version, status, total_size);

    if !status.is_success() {
        let msg = format!("Java {} download failed with HTTP {}", major_version, status);
        tracing::error!(target: "launcher", "{}", msg);
        return Err(LauncherError::Download(msg));
    }

    // Stream download to disk
    {
        use futures::StreamExt;
        let mut file = std::fs::File::create(&archive_path)
            .map_err(|e| LauncherError::Download(format!("Failed to create archive file: {}", e)))?;
        let mut stream = response.bytes_stream();
        let mut downloaded: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                tracing::error!(target: "launcher", "Java download stream error: {}", e);
                LauncherError::Download(format!("Download stream error: {}", e))
            })?;
            std::io::Write::write_all(&mut file, &chunk)?;
            downloaded += chunk.len() as u64;
            if total_size > 0 {
                let pct = 10.0 + (downloaded as f64 / total_size as f64) * 70.0;
                let mb_done = downloaded as f64 / (1024.0 * 1024.0);
                let mb_total = total_size as f64 / (1024.0 * 1024.0);
                emit_progress(pct, "downloading", &format!("{:.1}/{:.1} MB", mb_done, mb_total));
            }
        }
    }

    // Phase 3: Extract (blocking I/O in a dedicated thread)
    emit_progress(82.0, "extracting", "Extracting archive...");

    // Validate the file is actually a ZIP (not an MSI)
    {
        let mut header = [0u8; 4];
        use std::io::Read;
        let mut f = std::fs::File::open(&archive_path).map_err(|e| {
            LauncherError::Download(format!("Cannot open downloaded file: {}", e))
        })?;
        f.read_exact(&mut header).ok();
        if header != [0x50, 0x4b, 0x03, 0x04] {
            // Not a ZIP file — likely an MSI installer
            let _ = std::fs::remove_file(&archive_path);
            return Err(LauncherError::Download(format!(
                "Downloaded file is not a ZIP archive. Java {} may only have an MSI installer available from Adoptium.",
                major_version
            )));
        }
    }

    let archive_clone = archive_path.clone();
    let runtime_clone = runtime_dir.clone();
    tokio::task::spawn_blocking(move || {
        extract_archive(&archive_clone, &runtime_clone)
    })
    .await
    .map_err(|e| LauncherError::Download(format!("Extraction task failed: {}", e)))??;

    let _ = std::fs::remove_file(&archive_path);

    // Phase 4: Verify
    emit_progress(95.0, "verifying", "Verifying Java installation...");
    std::fs::write(&extract_marker, b"1")?;

    let java_exe = find_java_in_dir(&runtime_dir).ok_or_else(|| {
        tracing::error!(target: "launcher", "java.exe not found after extraction of Java {}", major_version);
        LauncherError::Download("Failed to find java.exe after extraction".to_string())
    })?;

    let install = crate::java::probe_java_by_path(&java_exe).ok_or_else(|| {
        tracing::error!(target: "launcher", "Failed to verify extracted Java {} runtime", major_version);
        LauncherError::Download("Failed to verify extracted Java runtime".to_string())
    })?;

    emit_progress(100.0, "done", "Java installed successfully");
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

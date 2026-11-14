use crate::error::{LauncherError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
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
///
/// `arch` defaults for records from older launcher builds that serialized
/// this struct before arch detection existed.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ManagedJavaRuntime {
    pub major_version: u32,
    pub path: PathBuf,
    pub version: String,
    pub vendor: String,
    pub is_64bit: bool,
    #[serde(default)]
    pub arch: crate::java::JavaArch,
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
    // Integrity metadata the Adoptium API publishes on the ZIP/MSI object
    // itself (SHA-256 hex of the exact file referenced by `link`).
    checksum: Option<String>,
    name: String,
}

const ADOPTIUM_API: &str = "https://api.adoptium.net/v3";
const MANAGED_JAVA_DIR: &str = "java";

type ClientSlot = OnceLock<Mutex<Option<(Option<String>, reqwest::Client)>>>;
type JavaListSlot = OnceLock<Mutex<Option<(Instant, Vec<AvailableJavaVersion>)>>>;

/// One async lock per Java major. Two instances needing Java 21 must never
/// share the archive/runtime paths concurrently; Java 17 and Java 21 remain
/// independent and can download in parallel.
static JAVA_DOWNLOAD_LOCKS: OnceLock<Mutex<HashMap<u32, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

fn java_download_lock(major_version: u32) -> Arc<tokio::sync::Mutex<()>> {
    let locks = JAVA_DOWNLOAD_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .entry(major_version)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}
/// HTTP client without auto-decompression вЂ” prevents "error decoding response body"
/// when the server sends brotli/deflate despite us not requesting it.
/// Rebuilt automatically when the proxy setting changes.
fn download_client() -> Result<reqwest::Client> {
    static CLIENT: ClientSlot = OnceLock::new();
    let proxy = crate::download::proxy_client_url()?;
    let mut slot = CLIENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.as_ref().map(|(p, _)| p != &proxy).unwrap_or(true) {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(1800))
            .connect_timeout(Duration::from_secs(30))
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .redirect(crate::download::redirect_policy())
            .user_agent(concat!("VoidLauncher/", env!("CARGO_PKG_VERSION")));
        match &proxy {
            Some(p) => {
                // A malformed proxy must fail loudly: silently dropping it
                // would turn a proxied request into a direct one.
                builder = builder.proxy(reqwest::Proxy::all(p.clone())?);
            }
            None => {
                // Truly direct: ignore HTTP_PROXY-style env variables so the
                // send_with_fallback retry cannot loop through the same proxy.
                builder = builder.no_proxy();
            }
        }
        let client = builder.build()?;
        *slot = Some((proxy, client));
    }
    Ok(slot.as_ref().unwrap().1.clone())
}

/// Adoptium /info/available_releases response (subset of fields)
#[derive(Debug, Deserialize)]
struct AdoptiumReleasesInfo {
    available_releases: Vec<u32>,
}

/// In-memory cache so reopening settings does not re-hit the API (like Prism Launcher)
const JAVA_LIST_CACHE_TTL: Duration = Duration::from_secs(600);

static JAVA_LIST_CACHE: JavaListSlot = OnceLock::new();

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

    // Resolve proxy scheme before the first Adoptium API call (see download_java_runtime).
    crate::download::ensure_proxy_resolved().await;

    let client = download_client()?;

    let supported: Vec<u32> = {
        let url = format!("{}/info/available_releases", ADOPTIUM_API);
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            crate::download::send_with_fallback(client.get(&url)),
        )
        .await;
        let releases = match result {
            Ok(Ok(resp)) if resp.status().is_success() => {
                match resp.json::<AdoptiumReleasesInfo>().await {
                    Ok(info) => Some(info.available_releases),
                    Err(e) => {
                        tracing::warn!(target: "launcher", "Failed to parse Adoptium releases info: {}", e);
                        None
                    }
                }
            }
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
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some((Instant::now(), versions.clone()));
    Ok(versions)
}

/// Download and install a Java runtime by major version.
///
/// Emits a terminal `"error"` progress event when it fails вЂ” the UI listens
/// for the event (launch-time setup modal, Settings spinner) and without it a
/// failed download left the modal/spinner claiming progress forever.
pub async fn download_java_runtime(
    major_version: u32,
    data_dir: &Path,
    app: &AppHandle,
) -> Result<ManagedJavaRuntime> {
    // Serialize only the same major. The guard is held across the whole
    // resolve/download/extract/publish sequence, so the second caller waits
    // and then reuses the published runtime instead of corrupting it.
    let lock = java_download_lock(major_version);
    let _guard = lock.lock().await;
    let result = download_java_runtime_inner(major_version, data_dir, app).await;
    if let Err(e) = &result {
        let _ = app.emit(
            "java_download_progress",
            JavaDownloadProgress {
                major_version,
                percent: 0.0,
                stage: "error".to_string(),
                message: e.to_string(),
            },
        );
    }
    result
}

/// Remove a temporary archive on every return path (success included).
struct RemoveFileOnDrop(PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Remove an unpublished extraction directory unless ownership was handed to
/// the final runtime path.
struct RemoveDirOnDrop {
    path: PathBuf,
    keep: bool,
}

impl Drop for RemoveDirOnDrop {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Remove leftovers from a crashed run of this same Java major. Must be called
/// with the per-major lock held, so any matching path is guaranteed dead.
fn cleanup_stale_staging(java_dir: &Path, major_version: u32) {
    let Ok(entries) = std::fs::read_dir(java_dir) else {
        return;
    };
    let dir_prefix = format!(".jdk-{}-", major_version);
    let file_prefix = format!(".java-{}-", major_version);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let is_staging_dir =
            name.starts_with(&dir_prefix) && name.ends_with(".tmp") && path.is_dir();
        let is_archive = name.starts_with(&file_prefix) && name.ends_with(".zip") && path.is_file();
        if is_staging_dir {
            let _ = std::fs::remove_dir_all(&path);
        } else if is_archive {
            let _ = std::fs::remove_file(&path);
        }
    }
}

async fn download_java_runtime_inner(
    major_version: u32,
    data_dir: &Path,
    app: &AppHandle,
) -> Result<ManagedJavaRuntime> {
    let java_dir = data_dir.join(MANAGED_JAVA_DIR);
    let runtime_dir = java_dir.join(format!("jdk-{}", major_version));
    let extract_marker = runtime_dir.join(".extracted");

    let emit_progress = |percent: f64, stage: &str, message: &str| {
        let _ = app.emit(
            "java_download_progress",
            JavaDownloadProgress {
                major_version,
                percent,
                stage: stage.to_string(),
                message: message.to_string(),
            },
        );
    };

    tracing::info!(target: "launcher", "Starting download of Java {} runtime", major_version);

    // If a usable runtime is already on disk, return it. Probe even when the
    // marker is missing: a crash between publish and marker write must not
    // force a full re-download.
    if runtime_dir.exists() {
        if let Some(java_exe) = find_java_in_dir(&runtime_dir) {
            if let Some(install) = crate::java::probe_java_by_path(&java_exe) {
                if !extract_marker.exists() {
                    let _ = std::fs::write(&extract_marker, b"1");
                }
                emit_progress(100.0, "done", "Java is ready");
                return Ok(ManagedJavaRuntime {
                    major_version,
                    path: java_exe,
                    version: install.version,
                    vendor: install.vendor,
                    is_64bit: install.is_64bit,
                    arch: install.arch,
                });
            }
        }
        // The marker/runtime is invalid. Do not delete it yet: the new
        // extraction is staged and verified first.
    }

    std::fs::create_dir_all(&java_dir)?;
    // A crashed earlier attempt may have left same-major staging dirs or
    // archives behind. The per-major lock means nothing can still be using
    // them. Other Java majors are untouched.
    cleanup_stale_staging(&java_dir, major_version);
    let staging_dir = java_dir.join(format!(
        ".jdk-{}-{}.tmp",
        major_version,
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&staging_dir)?;
    let mut staging_guard = RemoveDirOnDrop {
        path: staging_dir.clone(),
        keep: false,
    };

    // Ensure the proxy scheme (HTTP vs SOCKS5) is resolved before the first
    // Adoptium request. Without this, a system-detected SOCKS proxy would be
    // tried as HTTP вЂ” the connection opens (port is listening) but the proxy
    // never responds, causing a multi-minute hang before the timeout fires.
    crate::download::ensure_proxy_resolved().await;

    let client = download_client()?;

    // Phase 1: Resolve download URL. The /assets endpoint is sometimes
    // throttled ~30-40s on certain networks, so retry a few times.
    emit_progress(5.0, "resolving", "Querying Adoptium API...");

    let url = format!(
        "{}/assets/feature_releases/{}/ga?architecture=x64&image_type=jdk&os=windows&vendor=eclipse&page_size=1",
        ADOPTIUM_API, major_version
    );

    let mut resolved = None;
    for attempt in 1..=3 {
        match tokio::time::timeout(
            Duration::from_secs(90),
            crate::download::send_with_fallback(client.get(&url)),
        )
        .await
        {
            Ok(Ok(resp)) if resp.status().is_success() => {
                resolved = Some(resp);
                break;
            }
            Ok(Ok(resp)) => {
                let code = resp.status();
                // Honor Retry-After when Adoptium throttles us (HTTP 429), so
                // we wait out the rate-limit window instead of hammering it.
                let mut delay = Duration::from_secs(3);
                if let Some(ra) = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs)
                {
                    delay = ra.min(Duration::from_secs(60));
                }
                tracing::warn!(
                    target: "launcher",
                    "Adoptium API returned HTTP {}, waiting {}s",
                    code, delay.as_secs()
                );
                if attempt < 3 {
                    tokio::time::sleep(delay).await;
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(target: "launcher", "Adoptium API request failed (attempt {}/3): {}", attempt, e);
                if attempt < 3 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
            Err(_) => {
                tracing::warn!(target: "launcher", "Adoptium API request timed out (attempt {}/3)", attempt);
                if attempt < 3 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
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

    let binary = version_entry
        .binaries
        .iter()
        .find(|b| b.os_name == "windows" && b.architecture == "x64" && b.image_type == "jdk")
        .ok_or_else(|| {
            LauncherError::Download(format!(
                "No Windows x64 JDK package for Java {}",
                major_version
            ))
        })?;

    let pkg = binary
        .package
        .as_ref()
        .or(binary.installer.as_ref())
        .ok_or_else(|| {
            LauncherError::Download(format!(
                "No Windows x64 JDK package for Java {}",
                major_version
            ))
        })?;

    // Integrity metadata: the digest Adoptium publishes for the exact archive
    // referenced by `pkg.link` (SHA-256 hex). Verified against the downloaded
    // bytes before extraction.
    let expected_checksum = pkg.checksum.clone();

    // The archive name becomes a local file name, and the link is fetched вЂ”
    // both come from remote metadata, so neither is trusted blindly.
    crate::import::check_safe_file_name(&pkg.name)
        .map_err(|_| LauncherError::Download("Adoptium returned an invalid package name".into()))?;
    if !crate::is_allowed_download_host(&pkg.link) {
        return Err(LauncherError::Download(
            "Adoptium package link is not in the download allowlist".into(),
        ));
    }

    let archive_path = java_dir.join(format!(
        ".java-{}-{}.zip",
        major_version,
        uuid::Uuid::new_v4().simple()
    ));
    let _archive_guard = RemoveFileOnDrop(archive_path.clone());
    let pkg_name = pkg.name.clone();
    let pkg_link = pkg.link.clone();

    // Phase 2: Download
    emit_progress(10.0, "downloading", &format!("Downloading {}...", pkg_name));

    let response = crate::download::send_with_fallback(client.get(&pkg_link))
        .await
        .map_err(|e| {
            tracing::error!(target: "launcher", "Failed to download Java {}: {}", major_version, e);
            LauncherError::Download(format!("Failed to download Java: {}", e))
        })?;

    let status = response.status();
    let total_size = response.content_length().unwrap_or(0);

    tracing::info!(target: "launcher", "Java {} download response: status={}, total_size={}", major_version, status, total_size);

    if !status.is_success() {
        let msg = format!(
            "Java {} download failed with HTTP {}",
            major_version, status
        );
        tracing::error!(target: "launcher", "{}", msg);
        return Err(LauncherError::Download(msg));
    }

    // Stream download to disk
    {
        use futures::StreamExt;
        let mut file = std::fs::File::create(&archive_path).map_err(|e| {
            LauncherError::Download(format!("Failed to create archive file: {}", e))
        })?;
        let mut stream = response.bytes_stream();
        let mut downloaded: u64 = 0;
        // No-data timeout: if the server goes silent for 30s (no chunk at
        // all), abort instead of stalling the Java install indefinitely.
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(30), stream.next()).await {
                Ok(Some(chunk)) => {
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
                        emit_progress(
                            pct,
                            "downloading",
                            &format!("{:.1}/{:.1} MB", mb_done, mb_total),
                        );
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    tracing::error!(target: "launcher", "Java download stalled (no data for 30s)");
                    return Err(LauncherError::Download(
                        "Download stalled: no data received for 30 seconds. Check the network and retry.".into(),
                    ));
                }
            }
        }
    }

    // Phase 3: Validate the file is actually a ZIP archive, then verify its
    // integrity BEFORE extraction. A mismatched checksum means the bytes do
    // not match what Adoptium published вЂ” do not unpack untrusted content.
    {
        let mut header = [0u8; 4];
        use std::io::Read;
        let mut f = std::fs::File::open(&archive_path)
            .map_err(|e| LauncherError::Download(format!("Cannot open downloaded file: {}", e)))?;
        f.read_exact(&mut header).ok();
        if header != [0x50, 0x4b, 0x03, 0x04] {
            // Not a ZIP file вЂ” likely an MSI installer
            let _ = std::fs::remove_file(&archive_path);
            return Err(LauncherError::Download(format!(
                "Downloaded file is not a ZIP archive. Java {} may only have an MSI installer available from Adoptium.",
                major_version
            )));
        }
    }

    verify_archive_checksum(&archive_path, expected_checksum.as_deref()).inspect_err(|_| {
        let _ = std::fs::remove_file(&archive_path);
    })?;

    emit_progress(85.0, "extracting", "Extracting archive...");

    let archive_clone = archive_path.clone();
    let staging_clone = staging_dir.clone();
    tokio::task::spawn_blocking(move || extract_archive(&archive_clone, &staging_clone))
        .await
        .map_err(|e| LauncherError::Download(format!("Extraction task failed: {}", e)))??;

    // Phase 4: verify the staged runtime before publishing it.
    emit_progress(95.0, "verifying", "Verifying Java installation...");
    let staged_java = find_java_in_dir(&staging_dir).ok_or_else(|| {
        tracing::error!(target: "launcher", "java.exe not found after extraction of Java {}", major_version);
        LauncherError::Download("Failed to find java.exe after extraction".to_string())
    })?;
    let install = crate::java::probe_java_by_path(&staged_java).ok_or_else(|| {
        tracing::error!(target: "launcher", "Failed to verify extracted Java {} runtime", major_version);
        LauncherError::Download("Failed to verify extracted Java runtime".to_string())
    })?;

    // Publish: the old runtime was already proven unusable (or absent), and
    // the new one is fully extracted and probed. Only now remove/replace the
    // public directory, then write the completion marker.
    if runtime_dir.exists() {
        std::fs::remove_dir_all(&runtime_dir).map_err(|e| {
            LauncherError::Download(format!(
                "Could not replace the existing Java {} runtime (is a game using it?): {}",
                major_version, e
            ))
        })?;
    }
    std::fs::rename(&staging_dir, &runtime_dir).map_err(|e| {
        LauncherError::Download(format!(
            "Could not publish the Java {} runtime: {}",
            major_version, e
        ))
    })?;
    staging_guard.keep = true;
    std::fs::write(&extract_marker, b"1")?;

    let java_exe = find_java_in_dir(&runtime_dir).ok_or_else(|| {
        LauncherError::Download(
            "Java runtime was published but java.exe is no longer visible".to_string(),
        )
    })?;
    emit_progress(100.0, "done", "Java installed successfully");
    tracing::info!(target: "launcher", "Successfully installed Java {} ({})", major_version, install.version);
    Ok(ManagedJavaRuntime {
        major_version,
        path: java_exe,
        version: install.version,
        vendor: install.vendor,
        is_64bit: install.is_64bit,
        arch: install.arch,
    })
}

/// List already-downloaded Java runtimes
pub fn list_managed_java(data_dir: &Path) -> Vec<ManagedJavaRuntime> {
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
                        arch: install.arch,
                    });
                }
            }
        }
    }
    runtimes
}

/// Remove a managed Java runtime
pub fn remove_managed_java(major_version: u32, data_dir: &Path) -> Result<()> {
    let dir = data_dir
        .join(MANAGED_JAVA_DIR)
        .join(format!("jdk-{}", major_version));
    if dir.exists() {
        tracing::info!(target: "launcher", "Removing managed Java {} runtime", major_version);
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

fn find_java_in_dir(dir: &Path) -> Option<PathBuf> {
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

fn extract_archive(archive_path: &Path, dest_dir: &Path) -> Result<()> {
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

        // Refuse path traversal: entries like "jdk-21/../evil.exe" must not
        // escape dest_dir (defense against a malicious/truncated archive).
        if crate::download::is_unsafe_archive_path(&relative) {
            return Err(LauncherError::Download(format!(
                "Archive entry has an unsafe path: {}",
                full_name
            )));
        }

        let out_path = dest_dir.join(&relative);
        if !out_path.starts_with(dest_dir) {
            return Err(LauncherError::Download(format!(
                "Archive entry escapes destination directory: {}",
                full_name
            )));
        }

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

/// SHA-256 hex digest of a file (streamed, constant memory).
fn sha256_hex(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|e| LauncherError::Download(format!("Cannot open archive for checksum: {}", e)))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| {
            LauncherError::Download(format!("Error reading archive for checksum: {}", e))
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Verify a downloaded archive against the checksum the Adoptium API returned.
///
/// Adoptium publishes SHA-256 hex digests on the package object. A missing
/// checksum or a value in an unexpected format is tolerated (TLS already
/// protects the transport; failing hard would break installs if Adoptium
/// changes their hash scheme). A present SHA-256 that does not match the file
/// is a hard error and the caller must not extract the archive.
fn verify_archive_checksum(path: &std::path::Path, expected: Option<&str>) -> Result<()> {
    let Some(expected) = expected.map(str::trim).filter(|s| !s.is_empty()) else {
        tracing::warn!(
            target: "launcher",
            "Adoptium returned no checksum for Java archive; skipping integrity verification"
        );
        return Ok(());
    };
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        tracing::warn!(
            target: "launcher",
            "Unexpected checksum format from Adoptium; skipping integrity verification"
        );
        return Ok(());
    }

    let actual = sha256_hex(path)?;
    if !actual.eq_ignore_ascii_case(expected) {
        tracing::error!(
            target: "launcher",
            "Checksum mismatch for Java archive: expected {}, got {}",
            expected,
            actual
        );
        return Err(LauncherError::Download(format!(
            "Java archive checksum mismatch (expected {}, got {})",
            expected, actual
        )));
    }

    tracing::info!(target: "launcher", "Java archive checksum verified (SHA-256)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique(path: &str) -> std::path::PathBuf {
        let name = format!(
            "vl_java_checksum_{}_{}_{}",
            std::process::id(),
            path,
            uuid::Uuid::new_v4().simple()
        );
        std::env::temp_dir().join(name)
    }

    #[test]
    fn checksum_accepts_matching_sha256() {
        let file = unique("match");
        std::fs::write(&file, b"java archive payload 12345").unwrap();
        let expected = sha256_hex(&file).unwrap();
        let result = verify_archive_checksum(&file, Some(&expected));
        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn checksum_rejects_mismatch() {
        let file = unique("mismatch");
        std::fs::write(&file, b"tampered bytes").unwrap();
        let expected = sha256_hex(&file).unwrap();
        // Corrupt the file: digest no longer matches the previously computed one.
        std::fs::write(&file, b"tampered bytes!").unwrap();
        let result = verify_archive_checksum(&file, Some(&expected));
        assert!(result.is_err(), "expected Err, got {:?}", result);
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.to_lowercase().contains("checksum mismatch"));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn checksum_skips_when_missing_or_unrecognized_format() {
        let file = unique("skip");
        let content = [0u8; 4096];
        std::fs::write(&file, content).unwrap();

        assert!(verify_archive_checksum(&file, None).is_ok());
        assert!(verify_archive_checksum(&file, Some("")).is_ok());
        assert!(verify_archive_checksum(&file, Some("   ")).is_ok());
        // Not a 64-char hex digest (e.g. sha512 or an unexpected scheme) вЂ” tolerate.
        assert!(verify_archive_checksum(&file, Some("deadbeef")).is_ok());
        assert!(
            verify_archive_checksum(&file, Some("0123456789abcdef".repeat(8).as_str())).is_ok()
        );

        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn parses_real_adoptium_schema_with_checksum() {
        // Shape taken from a live /v3/assets/feature_releases/*/ga response:
        // the digest lives on the package (and installer) object, and the OS
        // field is serialized as "os". Locking the real schema into a test so
        // a future API change cannot silently disable the integrity check.
        let json = r#"[
          {
            "binaries": [
              {
                "architecture": "x64",
                "image_type": "jdk",
                "os": "windows",
                "package": {
                  "link": "https://github.com/adoptium/.../jdk.zip",
                  "name": "OpenJDK21U-jdk_x64_windows_hotspot.zip",
                  "checksum": "f9d6e191ab098c0d416e7d588a24420a8621cd2f4720dab2459b8b7b2d2d8b4e"
                },
                "installer": {
                  "link": "https://github.com/adoptium/.../jdk.msi",
                  "name": "OpenJDK21U-jdk_x64_windows_hotspot.msi",
                  "checksum": "454cfd334b9ca91c96dd8c2de97fcef6b9f1f98be9172ff076711f1c6b44e4e0"
                }
              }
            ]
          }
        ]"#;
        let entries: Vec<AdoptiumVersionData> = serde_json::from_str(json).unwrap();
        let bin = &entries[0].binaries[0];
        assert_eq!(bin.os_name, "windows");
        let pkg = bin.package.as_ref().unwrap();
        assert_eq!(
            pkg.checksum.as_deref(),
            Some("f9d6e191ab098c0d416e7d588a24420a8621cd2f4720dab2459b8b7b2d2d8b4e")
        );
        let msi = bin.installer.as_ref().unwrap();
        assert_eq!(
            msi.checksum.as_deref(),
            Some("454cfd334b9ca91c96dd8c2de97fcef6b9f1f98be9172ff076711f1c6b44e4e0")
        );
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        let file = unique("vector");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_hex(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&file);
    }

    #[tokio::test]
    async fn same_java_major_is_serialized_but_other_majors_are_not_blocked() {
        let lock21 = java_download_lock(21);
        let guard21 = lock21.lock().await;

        let same_major = tokio::spawn(async move {
            let lock = java_download_lock(21);
            let _guard = lock.lock().await;
            21u32
        });
        tokio::task::yield_now().await;
        assert!(
            !same_major.is_finished(),
            "a second Java 21 download must wait for the first"
        );

        let other_major = tokio::time::timeout(Duration::from_secs(1), async {
            let lock = java_download_lock(17);
            let _guard = lock.lock().await;
            17u32
        })
        .await
        .expect("a different Java major must not be serialized behind 21");
        assert_eq!(other_major, 17);

        drop(guard21);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), same_major)
                .await
                .expect("the same-major waiter must resume after release")
                .unwrap(),
            21
        );
    }

    #[test]
    fn stale_same_major_staging_is_removed_without_touching_other_majors() {
        let dir = unique("stale_staging");
        std::fs::create_dir_all(&dir).unwrap();
        let stale_dir = dir.join(".jdk-21-deadbeef.tmp");
        std::fs::create_dir_all(&stale_dir).unwrap();
        let stale_zip = dir.join(".java-21-deadbeef.zip");
        std::fs::write(&stale_zip, b"partial").unwrap();
        let other_dir = dir.join(".jdk-17-live.tmp");
        std::fs::create_dir_all(&other_dir).unwrap();
        let final_dir = dir.join("jdk-21");
        std::fs::create_dir_all(&final_dir).unwrap();

        cleanup_stale_staging(&dir, 21);
        assert!(!stale_dir.exists(), "same-major staging must be removed");
        assert!(!stale_zip.exists(), "same-major archive must be removed");
        assert!(other_dir.exists(), "another major's download must survive");
        assert!(final_dir.exists(), "the published runtime is never touched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn temporary_archive_and_staging_directory_are_removed_on_drop() {
        let dir = unique("guards");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join(".download.tmp");
        std::fs::write(&archive, b"partial").unwrap();
        {
            let _guard = RemoveFileOnDrop(archive.clone());
        }
        assert!(!archive.exists());

        let staging = dir.join(".jdk.tmp");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("file"), b"partial").unwrap();
        {
            let _guard = RemoveDirOnDrop {
                path: staging.clone(),
                keep: false,
            };
        }
        assert!(!staging.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

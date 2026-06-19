use crate::error::{LauncherError, Result};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::sleep;

const MAX_RETRIES: u32 = 3;
const REQUEST_TIMEOUT_SECS: u64 = 30;
const CONCURRENT_LIMIT: usize = 32;

/// Shared allowlist of trusted download mirrors.
pub const ALLOWED_DOWNLOAD_HOSTS: &[&str] = &[
    "cdn.modrinth.com",
    "assets.modrinth.com",
    "api.modrinth.com",
    "github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "edge.forgecdn.net",
    "mediafilez.forgecdn.net",
    "piston-data.mojang.com",
    "piston-meta.mojang.com",
    "launchermeta.mojang.com",
    "libraries.minecraft.net",
    "resources.download.minecraft.net",
    "maven.fabricmc.net",
    "maven.quiltmc.org",
    "files.minecraftforge.net",
    "maven.minecraftforge.net",
    "maven.neoforged.net",
    "api.adoptium.net",
];

/// Check whether `host` is in the allowlist (exact or subdomain match).
pub fn is_host_allowed(host: &str) -> bool {
    ALLOWED_DOWNLOAD_HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{}", h)))
}

/// Global HTTP client with connection pooling
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(32)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(15))
            .user_agent("VoidLauncher/0.1.3")
            .build()
            .expect("Failed to create HTTP client (check TLS libraries)")
    })
}

/// Download a single file with SHA1 verification, resume, timeout, and retry.
/// Enforces HTTPS + an allowlist of trusted hosts to prevent SSRF / file://
/// attacks when the URL originates from a third-party API.
pub async fn download_file(url: &str, path: &PathBuf, expected_sha1: &str) -> Result<()> {
    let url_lower = url.to_ascii_lowercase();
    if !url_lower.starts_with("https://") {
        return Err(crate::error::LauncherError::Download(
            "Download URL must use HTTPS".to_string(),
        ));
    }
    let after_scheme = &url[url.find("://").map(|i| i + 3).unwrap_or(8)..];
    let host_end = after_scheme
        .find(|c: char| c == '/' || c == ':' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let host = after_scheme[..host_end].to_ascii_lowercase();
    if !is_host_allowed(&host) {
        return Err(crate::error::LauncherError::Download(format!(
            "Download host '{}' is not in the allowlist",
            host
        )));
    }
    tracing::info!(target: "launcher", url = %url, "Downloading {}", path.display());
    // Check if file exists with correct hash
    if path.exists() && !expected_sha1.is_empty() {
        if verify_sha1(path, expected_sha1)? {
            tracing::debug!(target: "launcher", "SHA1 verified: {}", path.display());
            return Ok(());
        }
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Check for partial file (for resume)
    let existing_len = if path.exists() {
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    let client = http_client();
    let mut last_err = None;

    for attempt in 1..=MAX_RETRIES {
        match attempt_download(client, url, path, expected_sha1, existing_len).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt < MAX_RETRIES {
                    let delay = Duration::from_secs(2u64.pow(attempt));
                    sleep(delay).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        LauncherError::Download(format!("Failed to download {}", url))
    }))
}

async fn attempt_download(
    client: &reqwest::Client,
    url: &str,
    path: &PathBuf,
    expected_sha1: &str,
    existing_len: u64,
) -> Result<()> {
    use std::io::Write;
    use futures::StreamExt;

    let mut req = client.get(url);

    // Resume if we have a partial file
    if existing_len > 0 {
        req = req.header("Range", format!("bytes={}-", existing_len));
    }

    let response = req
        .send()
        .await
        .map_err(|e| LauncherError::Download(format!("Failed to download {}: {}", url, e)))?;

    let status = response.status();
    if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        tracing::warn!(target: "launcher", status = %status, url = %url, "Download returned non-success status");
        return Err(LauncherError::Download(format!(
            "HTTP {} for {}",
            status, url
        )));
    }

    if status == reqwest::StatusCode::PARTIAL_CONTENT {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| LauncherError::Download(format!("Failed to open {}: {}", path.display(), e)))?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| LauncherError::Download(format!("Stream error: {}", e)))?;
            file.write_all(&chunk)
                .map_err(|e| LauncherError::Download(format!("Failed to write {}: {}", path.display(), e)))?;
        }
    } else {
        let mut file = std::fs::File::create(path)
            .map_err(|e| LauncherError::Download(format!("Failed to create {}: {}", path.display(), e)))?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| LauncherError::Download(format!("Stream error: {}", e)))?;
            file.write_all(&chunk)
                .map_err(|e| LauncherError::Download(format!("Failed to write {}: {}", path.display(), e)))?;
        }
    }

    if !expected_sha1.is_empty() {
        if !verify_sha1(path, expected_sha1)? {
            let bytes = std::fs::read(path)?;
            let mut hasher = Sha1::new();
            hasher.update(&bytes);
            let got = hex::encode(hasher.finalize());
            tracing::warn!(target: "launcher", expected = %expected_sha1, got = %got, "SHA1 mismatch for {}", path.display());
            std::fs::remove_file(path)?;
            return Err(LauncherError::Download(format!(
                "SHA1 mismatch for {}",
                path.display()
            )));
        }
    }

    Ok(())
}

/// Download up to CONCURRENT_LIMIT files in parallel with progress callback
pub async fn download_files(
    files: Vec<(String, PathBuf, String, u64)>,
    on_progress: impl Fn(usize, usize, &str) + Send + Sync,
) -> Result<()> {
    let total = files.len();
    let mut completed = 0;
    let mut errors = Vec::new();

    for chunk in files.chunks(CONCURRENT_LIMIT) {
        let mut handles = Vec::with_capacity(chunk.len());

        for (url, path, sha1, _size) in chunk {
            let url = url.clone();
            let path = path.clone();
            let sha1 = sha1.clone();

            handles.push(tokio::spawn(async move {
                download_file(&url, &path, &sha1).await
            }));
        }

        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {
                    completed += 1;
                    on_progress(completed, total, "Downloading...");
                }
                Ok(Err(e)) => {
                    errors.push(e);
                    completed += 1;
                    on_progress(completed, total, "Downloading...");
                }
                Err(e) => {
                    errors.push(LauncherError::Download(format!("Task failed: {}", e)));
                    completed += 1;
                    on_progress(completed, total, "Downloading...");
                }
            }
        }
    }

    if !errors.is_empty() {
        let count = errors.len();
        let msg = errors
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(LauncherError::Download(format!(
            "{} download(s) failed: {}",
            count, msg
        )));
    }

    Ok(())
}

/// Download assets from Mojang
pub async fn download_assets(
    asset_index: &crate::versions::AssetIndexData,
    assets_dir: &PathBuf,
    on_progress: impl Fn(usize, usize, &str) + Send + Sync,
) -> Result<()> {
    let objects_dir = assets_dir.join("objects");
    std::fs::create_dir_all(&objects_dir)?;
    tracing::info!(target: "launcher", count = asset_index.objects.len(), "Downloading game assets");

    let mut files: Vec<(String, PathBuf, String, u64)> = Vec::new();

    for (_name, obj) in &asset_index.objects {
        let hash_prefix = &obj.hash[..2];
        let path = objects_dir.join(hash_prefix).join(&obj.hash);

        if !path.exists() {
            let url = format!(
                "https://resources.download.minecraft.net/{}/{}",
                hash_prefix, obj.hash
            );
            files.push((url, path, obj.hash.clone(), obj.size));
        }
    }

    if files.is_empty() {
        tracing::info!(target: "launcher", "Game assets download complete");
        return Ok(());
    }

    download_files(files, on_progress).await?;
    tracing::info!(target: "launcher", "Game assets download complete");
    Ok(())
}

/// Verify file SHA1 hash
pub fn verify_sha1(path: &PathBuf, expected: &str) -> Result<bool> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    let result = hex::encode(hasher.finalize());
    Ok(result == expected)
}

/// Expose global client for use by other modules (versions, modloaders)
pub fn global_http_client() -> &'static reqwest::Client {
    http_client()
}

use crate::error::{LauncherError, Result};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::time::sleep;

const MAX_RETRIES: u32 = 5;
const REQUEST_TIMEOUT_SECS: u64 = 120;
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
    "media.forgecdn.net",
    "piston-data.mojang.com",
    "piston-meta.mojang.com",
    "launchermeta.mojang.com",
    "launcher.mojang.com",
    "libraries.minecraft.net",
    "resources.download.minecraft.net",
    "maven.fabricmc.net",
    "files.minecraftforge.net",
    "maven.minecraftforge.net",
    "maven.neoforged.net",
    "maven.creeperhost.net",
    "repo.maven.apache.org",
    "meta.fabricmc.net",
    "meta.prismlauncher.org",
    "api.curseforge.com",
    "api.minecraftservices.com",
    "authserver.ely.by",
    "login.microsoftonline.com",
    "login.live.com",
    "user.auth.xboxlive.com",
    "xsts.auth.xboxlive.com",
    "bmclapi2.bangbang93.com",
    "mirrors.cernet.edu.cn",
    "api.adoptium.net",
];

/// Check whether `host` is in the allowlist (exact or subdomain match).
pub fn is_host_allowed(host: &str) -> bool {
    ALLOWED_DOWNLOAD_HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{}", h)))
}

/// Redirect policy: follow a redirect only if the destination is HTTPS and
/// its host is allowlisted, otherwise stop. reqwest follows up to 10
/// redirects by default without re-validating the target, which would let
/// a 302 from a trusted host escape to an arbitrary host.
pub fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let url = attempt.url();
        let host = url.host_str().unwrap_or("");
        if url.scheme() == "https" && is_host_allowed(host) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// Global HTTP client with connection pooling
fn http_client() -> reqwest::Client {
    static CLIENT: OnceLock<Mutex<Option<(Option<String>, reqwest::Client)>>> = OnceLock::new();
    let proxy = configured_proxy();
    let mut slot = CLIENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap();
    // Rebuild the client when the proxy setting changes (including disabled).
    if slot.as_ref().map(|(p, _)| p != &proxy).unwrap_or(true) {
        *slot = Some((proxy.clone(), build_client(proxy.as_deref())));
    }
    slot.as_ref().unwrap().1.clone()
}

fn build_client(proxy: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(32)
        .pool_idle_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(15))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .redirect(redirect_policy())
        .user_agent(concat!("VoidLauncher/", env!("CARGO_PKG_VERSION")));
    if let Some(proxy) = proxy {
        if let Ok(p) = reqwest::Proxy::all(proxy.to_string()) {
            builder = builder.proxy(p);
        }
    }
    builder
        .build()
        .expect("Failed to create HTTP client (check TLS libraries)")
}

/// Proxy URL configured in the settings (None = direct connection).
static PROXY_CFG: OnceLock<Mutex<Option<String>>> = OnceLock::new();

pub(crate) fn configured_proxy() -> Option<String> {
    PROXY_CFG
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
}

/// Update the global proxy used by every HTTP request. Called when the
/// config is loaded and whenever the settings are saved.
pub fn set_global_proxy(proxy: Option<String>) {
    *PROXY_CFG
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = proxy;
}

/// Validate a completed download file: SHA1 check or JSON-content rejection.
fn validate_download(path: &std::path::Path, expected_sha1: &str) -> Result<()> {
    if !expected_sha1.is_empty() {
        if !verify_sha1(path, expected_sha1)? {
            return Err(LauncherError::Download(format!(
                "SHA1 mismatch for {}",
                path.display()
            )));
        }
    } else {
        // basic sanity: reject if looks like JSON
        let meta = std::fs::metadata(path)?;
        if meta.len() > 0 {
            use std::io::Read;
            let mut buf = [0u8; 1];
            let mut f = std::fs::File::open(path)?;
            let _ = f.read(&mut buf);
            if buf[0] == b'{' || buf[0] == b'[' {
                return Err(LauncherError::Download(format!(
                    "Downloaded content looks like JSON, not a binary file: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// Validate URL + host, then download to a .part temp file, validate,
/// and atomically rename to `path` on success. On ANY error the .part file
/// is cleaned up and `path` is never touched.
async fn download_to_part(
    url: &str,
    path: &std::path::Path,
    expected_sha1: &str,
    timeout: Option<Duration>,
) -> Result<()> {
    // Auto-upgrade http:// to https://
    let url = if url.to_ascii_lowercase().starts_with("http://") {
        tracing::debug!(target: "launcher", "Upgrading http:// to https:// for: {}", url);
        url.replacen("http://", "https://", 1)
    } else {
        url.to_string()
    };
    if !url.starts_with("https://") {
        return Err(LauncherError::Download(format!(
            "Download URL must use HTTPS: {}",
            url
        )));
    }
    let after_scheme = &url[8..];
    let host_end = after_scheme
        .find(|c: char| c == '/' || c == ':' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let host = after_scheme[..host_end].to_ascii_lowercase();
    if !is_host_allowed(&host) {
        return Err(LauncherError::Download(format!(
            "Download host '{}' is not in the allowlist",
            host
        )));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let part_path = {
        let mut p = path.as_os_str().to_os_string();
        p.push(".part");
        std::path::PathBuf::from(p)
    };

    // Always remove any stale .part from a previous attempt
    let _ = std::fs::remove_file(&part_path);

    let client = http_client();
    let mut req = client.get(&url);
    if let Some(t) = timeout {
        req = req.timeout(t);
    }
    let response = req
        .send()
        .await
        .map_err(|e| LauncherError::Download(format!("Failed to download {}: {}", url, e)))?;

    let status = response.status();
    if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(LauncherError::Download(format!("HTTP {} for {}", status, &url)));
    }

    // Reject non-binary responses (only when no SHA1 to validate)
    if expected_sha1.is_empty() {
        if let Some(ct) = response.headers().get("content-type") {
            let ct_str = ct.to_str().unwrap_or("");
            if is_rejected_content_type(ct_str) {
                return Err(LauncherError::Download(format!(
                    "Server returned unexpected content-type '{}' for {}",
                    ct_str, url
                )));
            }
        }
    }

    // Download entire body into memory first, then write to .part file.
    // Using bytes() instead of bytes_stream() avoids potential chunked-encoding
    // or HTTP framing issues in the streaming decoder.
    let write_result = {
        use std::io::Write;

        let bytes = response.bytes().await.map_err(|e| {
            LauncherError::Download(format!("Failed to read response body: {}", e))
        })?;
        let mut file = std::fs::File::create(&part_path)
            .map_err(|e| LauncherError::Download(format!("Failed to create .part: {}", e)))?;
        file.write_all(&bytes)
            .map_err(|e| LauncherError::Download(format!("Failed to write .part: {}", e)))
    };

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&part_path);
        return Err(e);
    }

    // Validate the .part file content
    if let Err(e) = validate_download(&part_path, expected_sha1) {
        let _ = std::fs::remove_file(&part_path);
        return Err(e);
    }

    // Atomically replace the destination: rename .part → final path
    std::fs::rename(&part_path, path)?;

    Ok(())
}

/// Some Mojang artifacts (e.g. `launchwrapper`) are referenced by old
/// Forge profiles on `maven.minecraftforge.net` but only exist on
/// Mojang's own mirror. Attempt the same path on the other host when
/// the primary one fails (404).
///
/// NeoForge's `maven.neoforged.net` is IP-blocked for RU users (connect
/// timeout), and `maven.creeperhost.net` is DPI-throttled, so we try the
/// BMCLAPI mirror first (path without `/releases/`), then creeperhost.
fn mirror_alternates(url: &str) -> Vec<String> {
    const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/";
    const MOJANG_LIBRARIES: &str = "https://libraries.minecraft.net/";
    const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/";
    const BMCLAPI_MAVEN: &str = "https://bmclapi2.bangbang93.com/maven/";
    const CREEPERHOST_MAVEN: &str = "https://maven.creeperhost.net/";
    if let Some(rest) = url.strip_prefix(NEOFORGE_MAVEN) {
        vec![
            format!("{}{}", BMCLAPI_MAVEN, rest),
            format!("{}{}", CREEPERHOST_MAVEN, rest),
        ]
    } else if let Some(rest) = url.strip_prefix("https://maven.neoforged.net/") {
        vec![
            format!("{}{}", BMCLAPI_MAVEN, rest),
            format!("{}{}", CREEPERHOST_MAVEN, rest),
        ]
    } else if let Some(rest) = url.strip_prefix(BMCLAPI_MAVEN) {
        vec![
            format!("{}{}", NEOFORGE_MAVEN, rest),
            format!("{}{}", CREEPERHOST_MAVEN, rest),
        ]
    } else if let Some(rest) = url.strip_prefix(CREEPERHOST_MAVEN) {
        vec![
            format!("{}{}", NEOFORGE_MAVEN, rest),
            format!("{}{}", BMCLAPI_MAVEN, rest),
        ]
    } else if let Some(rest) = url.strip_prefix(FORGE_MAVEN) {
        vec![format!("{}{}", MOJANG_LIBRARIES, rest)]
    } else if let Some(rest) = url.strip_prefix(MOJANG_LIBRARIES) {
        vec![format!("{}{}", FORGE_MAVEN, rest)]
    } else {
        Vec::new()
    }
}

/// Download a single file with SHA1 verification and retry.
/// Writes to a .part temp file, validates, then atomically renames.
pub async fn download_file(url: &str, path: &PathBuf, expected_sha1: &str) -> Result<()> {
    tracing::info!(target: "launcher", url = %url, "Downloading {}", path.display());

    // Check if file exists with correct hash
    if path.exists() && !expected_sha1.is_empty() {
        if verify_sha1(path, expected_sha1)? {
            tracing::debug!(target: "launcher", "SHA1 verified: {}", path.display());
            return Ok(());
        }
    }

    let candidates: Vec<String> = std::iter::once(url.to_string())
        .chain(mirror_alternates(url))
        .collect();
    let mut last_err = None;

    for (ci, candidate) in candidates.iter().enumerate() {
        // If mirrors exist, the primary host gets only one attempt so a
        // blocked/dead host fails over to a mirror quickly instead of
        // burning MAX_RETRIES worth of timeouts per file.
        let attempts = if candidates.len() > 1 && ci == 0 { 1 } else { MAX_RETRIES };
        for attempt in 1..=attempts {
            match download_to_part(candidate, path, expected_sha1, None).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < attempts {
                        sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    }
                }
            }
        }
        if ci + 1 < candidates.len() {
            tracing::warn!(target: "launcher", "Primary host failed for {}, trying mirror", url);
        }
    }

    Err(last_err.unwrap_or_else(|| {
        LauncherError::Download(format!("Failed to download {}", url))
    }))
}

/// Compute a reasonable download timeout from file size (~512 KB/s minimum + buffer).
pub fn timeout_for_size(size_bytes: u64) -> Duration {
    let secs = size_bytes / 512_000 + 60;
    Duration::from_secs(secs.clamp(120, 900))
}

/// Download a file with streaming, retries, and a timeout scaled to expected size.
pub async fn download_file_sized(
    url: &str,
    path: &PathBuf,
    expected_sha1: &str,
    size_bytes: u64,
) -> Result<()> {
    tracing::info!(target: "launcher", url = %url, size = size_bytes, "Downloading {} (sized)", path.display());

    let timeout = timeout_for_size(size_bytes);
    let candidates: Vec<String> = std::iter::once(url.to_string())
        .chain(mirror_alternates(url))
        .collect();
    let mut last_err = None;

    for (ci, candidate) in candidates.iter().enumerate() {
        // Mirrors get full retries; the primary host gets a single attempt
        // so it can't stall the whole install with repeated timeouts.
        let attempts = if candidates.len() > 1 && ci == 0 { 1 } else { MAX_RETRIES };
        for attempt in 1..=attempts {
            match download_to_part(candidate, path, expected_sha1, Some(timeout)).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < attempts {
                        sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    }
                }
            }
        }
        if ci + 1 < candidates.len() {
            tracing::warn!(target: "launcher", "Primary host failed for {}, trying mirror", url);
        }
    }

    Err(last_err.unwrap_or_else(|| {
        LauncherError::Download(format!("Failed to download {}", url))
    }))
}

/// Returns `true` for content-types that indicate an error page or non-binary response.
fn is_rejected_content_type(ct: &str) -> bool {
    let ct_lower = ct.to_ascii_lowercase();
    ct_lower.starts_with("text/")
        || ct_lower.starts_with("application/json")
        || ct_lower.starts_with("application/problem+json")
        || ct_lower.starts_with("application/xml")
        || ct_lower.starts_with("application/xhtml")
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

/// Mirror asset objects into `<assets>/virtual/legacy` for versions that read
/// flat paths from the assets directory instead of the objects store:
///   - "legacy"/"pre-1.6" index (MC 1.6.x and older): the whole index, the
///     game is given `--assetsDir <assets>/virtual/legacy`.
///   - index with `map_to_resources` (MC 1.7.x/1.8.x): whole index, used as a
///     fallback for legacy resource pack layouts.
///   - 1.9+ indexes: only objects flagged `virtual: true`.
/// Modern indexes without virtual objects are skipped entirely.
pub fn ensure_virtual_assets(
    index_id: &str,
    asset_index: &crate::versions::AssetIndexData,
    assets_dir: &PathBuf,
) -> Result<()> {
    let all = index_id == "legacy" || index_id == "pre-1.6" || asset_index.map_to_resources;
    let any_virtual = asset_index.objects.values().any(|o| o.is_virtual);
    if !all && !any_virtual {
        return Ok(());
    }

    let objects_dir = assets_dir.join("objects");
    let virtual_dir = assets_dir.join("virtual").join("legacy");
    std::fs::create_dir_all(&virtual_dir)?;

    let mut copied = 0usize;
    for (name, obj) in &asset_index.objects {
        if !all && !obj.is_virtual {
            continue;
        }
        let src = objects_dir.join(&obj.hash[..2]).join(&obj.hash);
        if !src.exists() {
            continue;
        }
        let dest = virtual_dir.join(name);
        if dest.exists() {
            let skip = std::fs::metadata(&dest)
                .map(|m| m.len() == obj.size)
                .unwrap_or(false);
            if skip {
                continue;
            }
            let _ = std::fs::remove_file(&dest);
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Prefer a hard link (instant, deduplicated); fall back to a copy.
        if std::fs::hard_link(&src, &dest).is_err() {
            std::fs::copy(&src, &dest)?;
        }
        copied += 1;
    }

    tracing::info!(target: "launcher", count = copied, "Prepared virtual/legacy assets (index {})", index_id);
    Ok(())
}

/// Verify file SHA1 hash
pub fn verify_sha1(path: &std::path::Path, expected: &str) -> Result<bool> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    let result = hex::encode(hasher.finalize());
    Ok(result == expected)
}

/// Verify a downloaded file is a valid ZIP/JAR archive by its magic bytes.
/// Guards against error pages (HTML/JSON) or truncated responses being saved
/// under a .jar/.zip name when no SHA1 is available to check.
pub fn verify_zip_magic(path: &std::path::Path) -> Result<()> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|e| LauncherError::Download(format!("Cannot open downloaded file: {}", e)))?;
    let mut header = [0u8; 4];
    if file.read_exact(&mut header).is_err() {
        return Err(LauncherError::Download(
            "Downloaded file is too small to be a valid JAR archive".to_string(),
        ));
    }
    // PK\x03\x04 (regular) or PK\x05\x06 (empty archive, EOCD only)
    if header != [0x50, 0x4b, 0x03, 0x04] && header != [0x50, 0x4b, 0x05, 0x06] {
        return Err(LauncherError::Download(
            "Downloaded file is not a valid JAR archive".to_string(),
        ));
    }
    Ok(())
}

/// Compute SHA1 hash of a file and return hex string
pub fn hash_file_sha1(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Expose global client for use by other modules (versions, modloaders).
/// Returns a cheap clone; the underlying client is rebuilt automatically
/// when the proxy setting changes.
pub fn global_http_client() -> reqwest::Client {
    http_client()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("voidlauncher_test_{}_{}", std::process::id(), name));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn zip_magic_accepts_regular_zip() {
        let p = temp_file("ok.zip", b"PK\x03\x04rest");
        assert!(verify_zip_magic(&p).is_ok());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn zip_magic_accepts_empty_zip() {
        let p = temp_file("empty.zip", b"PK\x05\x06rest");
        assert!(verify_zip_magic(&p).is_ok());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn zip_magic_rejects_html() {
        let p = temp_file("bad.jar", b"<!DOCTYPE html><html>404</html>");
        assert!(verify_zip_magic(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn zip_magic_rejects_json() {
        let p = temp_file("bad2.jar", b"{\"error\":\"Not Found\"}");
        assert!(verify_zip_magic(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn zip_magic_rejects_too_small() {
        let p = temp_file("tiny.jar", b"PK");
        assert!(verify_zip_magic(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn host_allowlist_exact_match() {
        assert!(is_host_allowed("github.com"));
        assert!(is_host_allowed("objects.githubusercontent.com"));
        assert!(is_host_allowed("api.modrinth.com"));
    }

    #[test]
    fn host_allowlist_subdomain_match() {
        assert!(is_host_allowed("sub.github.com"));
        assert!(is_host_allowed("media.forgecdn.net"));
        assert!(is_host_allowed("x.api.adoptium.net"));
    }

    #[test]
    fn host_allowlist_rejects_unknown_and_suffix_spoofing() {
        assert!(!is_host_allowed("attacker.example"));
        assert!(!is_host_allowed("github.com.attacker.example"));
        assert!(!is_host_allowed("notgithub.com"));
        assert!(!is_host_allowed(""));
    }
}

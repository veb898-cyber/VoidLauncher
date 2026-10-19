use crate::error::{LauncherError, Result};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::time::sleep;

const MAX_RETRIES: u32 = 5;
const REQUEST_TIMEOUT_SECS: u64 = 120;
const CONCURRENT_LIMIT: usize = 32;

/// Global pause flag for modpack installs: when set, the currently active
/// download stops at the next chunk (keeping the .part file) and install
/// commands fail with `LauncherError::Paused`. Resuming re-runs the install
/// and `download_to_part` continues from the saved offset via Range.
static PAUSE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Number of pausable operations (modpack installs/updates/repairs) that are
/// currently running. A pause request is only honoured while at least one is
/// active, so a session with no pack install running can never pause (or get
/// stuck paused) unrelated downloads like game files or mod updates.
static PAUSABLE_SCOPES: AtomicUsize = AtomicUsize::new(0);

/// RAII marker for a pausable operation. Create one at the start of a
/// pack install/update/repair command; downloads only respond to
/// `request_pause()` while at least one marker is alive.
pub struct PausableInstallScope {
    _private: (),
}

impl PausableInstallScope {
    pub fn begin() -> Self {
        // Clear any stale flag from a previous install that raced with its
        // own cleanup — a fresh install always starts unpaused.
        PAUSE_REQUESTED.store(false, Ordering::SeqCst);
        PAUSABLE_SCOPES.fetch_add(1, Ordering::SeqCst);
        Self { _private: () }
    }
}

impl Drop for PausableInstallScope {
    fn drop(&mut self) {
        if PAUSABLE_SCOPES.fetch_sub(1, Ordering::SeqCst) == 1 {
            // Last pausable operation finished — a leftover request must not
            // pause whatever the user does next.
            PAUSE_REQUESTED.store(false, Ordering::SeqCst);
        }
    }
}

/// Request that the current download stop as soon as possible. No-op when
/// no pausable install is running.
pub fn request_pause() {
    if PAUSABLE_SCOPES.load(Ordering::SeqCst) > 0 {
        PAUSE_REQUESTED.store(true, Ordering::SeqCst);
    }
}

/// Clear the pause flag (called when the user resumes an install).
pub fn clear_pause() {
    PAUSE_REQUESTED.store(false, Ordering::SeqCst);
}

fn pause_requested() -> bool {
    PAUSABLE_SCOPES.load(Ordering::SeqCst) > 0 && PAUSE_REQUESTED.load(Ordering::SeqCst)
}

/// Shared allowlist of trusted download mirrors.
pub const ALLOWED_DOWNLOAD_HOSTS: &[&str] = &[
    "cdn.modrinth.com",
    "assets.modrinth.com",
    "api.modrinth.com",
    "github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
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
    "mc-heads.net",
    "sessionserver.mojang.com",
    "textures.minecraft.net",
    "skinsystem.ely.by",
    "ely.by",
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
    "download.nodecdn.net",
    "api.atlauncher.com",
    "pkgs.tailscale.com",
];

/// Check whether `host` is in the allowlist (exact or subdomain match).
pub fn is_host_allowed(host: &str) -> bool {
    ALLOWED_DOWNLOAD_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{}", h)))
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

type CachedClient = Mutex<Option<(Option<String>, reqwest::Client)>>;

/// Global HTTP client with connection pooling. On failure (rare: TLS backend
/// trouble) returns the error instead of panicking; nothing is cached, so the
/// next call retries the build.
fn http_client() -> reqwest::Result<reqwest::Client> {
    static CLIENT: OnceLock<CachedClient> = OnceLock::new();
    let proxy = resolved_proxy_url(active_proxy_raw());
    let mut slot = CLIENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Rebuild the client when the proxy setting changes (including disabled).
    if slot.as_ref().map(|(p, _)| p != &proxy).unwrap_or(true) {
        let client = build_client(proxy.as_deref())?;
        *slot = Some((proxy.clone(), client));
    }
    Ok(slot.as_ref().unwrap().1.clone())
}

fn build_client(proxy: Option<&str>) -> reqwest::Result<reqwest::Client> {
    base_client_builder(proxy).build()
}

/// The shared request settings (timeouts, pooling, proxy handling, user
/// agent) used by both the global client and the per-request pinned clients.
fn base_client_builder(proxy: Option<&str>) -> reqwest::ClientBuilder {
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
    match proxy {
        Some(proxy) => {
            if let Ok(p) = reqwest::Proxy::all(proxy.to_string()) {
                builder = builder.proxy(p);
            }
        }
        None => {
            // A truly direct client: without `.no_proxy()` reqwest would still
            // honor HTTP_PROXY/HTTPS_PROXY environment variables, so the
            // "proxy -> direct" fallback could silently retry through the very
            // same broken proxy. The launcher's own setting is the single
            // source of truth; system-level detection is handled separately.
            builder = builder.no_proxy();
        }
    }
    builder
}

/// Build a client that pins `host` to the given pre-validated IP addresses
/// (DNS-rebinding protection: the name is never re-resolved). Redirects are
/// NOT followed automatically — the caller validates and pins each hop.
/// Uses the configured proxy.
pub(crate) fn pinned_http_client(
    host: &str,
    addrs: &[std::net::IpAddr],
) -> reqwest::Result<reqwest::Client> {
    let proxy = resolved_proxy_url(active_proxy_raw());
    finish_pinned_client(base_client_builder(proxy.as_deref()), host, addrs)
}

/// Same as [`pinned_http_client`] but bypasses the proxy — the "proxy ->
/// direct" retry path for pinned requests.
pub(crate) fn pinned_http_client_direct(
    host: &str,
    addrs: &[std::net::IpAddr],
) -> reqwest::Result<reqwest::Client> {
    finish_pinned_client(base_client_builder(None), host, addrs)
}

fn finish_pinned_client(
    mut builder: reqwest::ClientBuilder,
    host: &str,
    addrs: &[std::net::IpAddr],
) -> reqwest::Result<reqwest::Client> {
    builder = builder.redirect(reqwest::redirect::Policy::none());
    for addr in addrs {
        // The port from the URL overrides this one when explicit; https URLs
        // without a port use 443 anyway.
        builder = builder.resolve(host, std::net::SocketAddr::new(*addr, 443));
    }
    builder.build()
}

/// Proxy URL configured in the settings (None = direct connection).
static PROXY_CFG: OnceLock<Mutex<Option<String>>> = OnceLock::new();

#[cfg(windows)]
type SystemProxyCache = Mutex<Option<(std::time::Instant, Option<String>)>>;

/// Cached Windows system proxy (`ProxyEnable`/`ProxyServer` from the
/// Internet Settings registry key), refreshed at most every 60 s so users who
/// toggle their VPN app on/off get picked up without a restart.
#[cfg(windows)]
static SYSTEM_PROXY_CACHE: OnceLock<SystemProxyCache> = OnceLock::new();

type ResolvedProxyCache = Mutex<Option<(String, Option<String>)>>;

/// Resolved proxy scheme, cached per configured `host:port`.
/// `(raw, Some(url))` = working proxy; `(raw, None)` = proxy unusable.
static PROXY_RESOLVED: OnceLock<ResolvedProxyCache> = OnceLock::new();

pub(crate) fn configured_proxy() -> Option<String> {
    PROXY_CFG
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
}

/// Parse the `ProxyServer` registry value into a `host:port` string.
/// Supported formats: `host:port`, `http://host:port`,
/// `http=host:p;https=host:p[;socks=...]` (https preferred, then http, then socks).
#[cfg(windows)]
fn parse_win_proxy_server(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if !v.contains('=') {
        // Plain "host:port" (optionally with scheme prefix).
        let bare = v
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_start_matches("socks://")
            .trim_start_matches("socks5://");
        return (!bare.is_empty()).then(|| bare.to_string());
    }
    // Per-protocol map.
    let mut https = None;
    let mut http = None;
    let mut socks = None;
    for part in v.split(';') {
        let Some((proto, addr)) = part.split_once('=') else {
            continue;
        };
        let addr = addr.trim();
        if addr.is_empty() {
            continue;
        }
        match proto.trim().to_ascii_lowercase().as_str() {
            "https" => https = Some(addr.to_string()),
            "http" => http = Some(addr.to_string()),
            "socks" | "socks5" => socks = Some(addr.to_string()),
            _ => {}
        }
    }
    https.or(http).or(socks)
}

/// Read the Windows system proxy (WinINET settings used by browsers/WebView).
/// Returns the raw `host:port`, or None when disabled/unreadable/non-Windows.
#[cfg(windows)]
fn system_proxy_hint() -> Option<String> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(60);
    let slot = SYSTEM_PROXY_CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = slot.lock().unwrap();
        if let Some((at, val)) = guard.as_ref() {
            if at.elapsed() < TTL {
                return val.clone();
            }
        }
    }
    let detected = (|| {
        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let key = hkcu
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
            .ok()?;
        let enabled: u32 = key.get_value("ProxyEnable").ok()?;
        if enabled == 0 {
            return None;
        }
        let server: String = key.get_value("ProxyServer").ok()?;
        parse_win_proxy_server(&server)
    })();
    *slot.lock().unwrap() = Some((std::time::Instant::now(), detected.clone()));
    tracing::info!(target: "launcher", "System proxy detection: {}", detected.as_deref().unwrap_or("none"));
    detected
}

#[cfg(not(windows))]
fn system_proxy_hint() -> Option<String> {
    None
}

/// The proxy the launcher should actively use: the explicit in-app setting
/// when present, otherwise the Windows system proxy (VPN apps like v2rayN set
/// it system-wide; WebView already follows it, so using it for backend traffic
/// too keeps everything consistent and toggle-safe — an unreachable proxy is
/// re-probed by `ensure_proxy_resolved` and degrades to direct).
pub(crate) fn active_proxy_raw() -> Option<String> {
    configured_proxy().or_else(system_proxy_hint)
}

/// Best-known proxy URL for the configured `host:port`: the resolved scheme
/// when already tested, otherwise the HTTP guess (the most common case).
pub fn resolved_proxy_url(raw: Option<String>) -> Option<String> {
    let raw = raw?;
    if let Some((r, url)) = PROXY_RESOLVED
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
    {
        if r == raw {
            return url;
        }
    }
    Some(format!("http://{}", raw))
}

/// Test the configured proxy once (HTTP first, then SOCKS5) and cache the
/// scheme that actually connects. Called before the first network request.
/// `127.0.0.1:10808` style SOCKS ports (VPN apps) are detected this way.
pub async fn ensure_proxy_resolved() {
    let Some(raw) = active_proxy_raw() else {
        return;
    };
    {
        let slot = PROXY_RESOLVED
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap();
        if slot.as_ref().map(|(r, _)| r == &raw).unwrap_or(false) {
            return;
        }
    }
    let candidates = [format!("http://{}", raw), format!("socks5://{}", raw)];
    let mut chosen: Option<String> = None;
    for url in &candidates {
        let Ok(proxy) = reqwest::Proxy::all(url.clone()) else {
            continue;
        };
        let Ok(client) = reqwest::Client::builder()
            .proxy(proxy)
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(4))
            .build()
        else {
            continue;
        };
        let ok = tokio::time::timeout(
            Duration::from_secs(4),
            client.get("https://api.modrinth.com/").send(),
        )
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false);
        if ok {
            chosen = Some(url.clone());
            break;
        }
    }
    if chosen.is_none() {
        tracing::warn!(
            target: "launcher",
            "Proxy {}://{} unreachable — falling back to direct connection",
            candidates[0],
            raw
        );
    }
    *PROXY_RESOLVED
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = Some((raw, chosen));
}

/// Send a request through the configured proxy; if that fails at the network
/// level and a proxy is configured, retry the same request directly. VPN
/// proxies often work for some hosts but not others (e.g. CurseForge, Mojang)
/// — falling back keeps the launcher usable instead of showing a dead catalog
/// or failing an install.
pub async fn send_with_fallback(
    req: reqwest::RequestBuilder,
) -> std::result::Result<reqwest::Response, reqwest::Error> {
    let Some(proxied) = req.try_clone() else {
        return req.send().await;
    };
    match proxied.send().await {
        Ok(r) => Ok(r),
        Err(e) => {
            if active_proxy_raw().is_some() {
                // Retry on a REAL proxy-free client: a VPN proxy that works
                // for most hosts can still refuse specific ones (Mojang CDN).
                if let Some(builder) = req.try_clone() {
                    if let Ok(request) = builder.build() {
                        let direct = match build_client(None) {
                            Ok(client) => client,
                            Err(_) => return Err(e),
                        };
                        if let Ok(r) = direct.execute(request).await {
                            tracing::warn!(
                                target: "launcher",
                                "Proxy failed for request, retried directly: {}",
                                r.url()
                            );
                            return Ok(r);
                        }
                    }
                }
            }
            Err(e)
        }
    }
}

/// Perform a GET with retry on transient failures only: network errors,
/// HTTP 429 (rate limit, honoring `Retry-After` when present, capped so a
/// retry never blocks an install for too long) and 5xx. Non-transient 4xx
/// return immediately. The final error message carries a truncated body
/// preview (500 chars) so API noise doesn't flood the UI or logs.
///
/// Shared by the Modrinth and CurseForge API clients whose retry semantics
/// are identical — keep both callers on this helper so the behavior cannot
/// drift apart.
pub async fn get_with_retry(
    req: reqwest::RequestBuilder,
    provider: &str,
    attempts: usize,
    backoff_ms: &[u64],
) -> Result<reqwest::Response> {
    let mut last_err: Option<LauncherError> = None;
    for attempt in 0..attempts {
        let attempt_req = match req.try_clone() {
            Some(r) => r,
            None => return req.send().await.map_err(LauncherError::Network),
        };
        let response = match send_with_fallback(attempt_req).await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(LauncherError::Network(e));
                if attempt < attempts - 1 {
                    crate::events::emit_fetch_retry(
                        provider,
                        attempt + 2,
                        attempts,
                        &format!("Retrying {} request", provider),
                    );
                    sleep(Duration::from_millis(
                        backoff_ms[attempt.min(backoff_ms.len() - 1)],
                    ))
                    .await;
                }
                continue;
            }
        };

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let retriable = status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.as_u16() >= 500;
        if retriable && attempt < attempts - 1 {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(|secs| secs.min(10))
                .unwrap_or_else(|| (backoff_ms[attempt.min(backoff_ms.len() - 1)] / 1000).max(1));
            last_err = Some(LauncherError::Download(format!(
                "{} API error ({})",
                provider, status
            )));
            crate::events::emit_fetch_retry(
                provider,
                attempt + 2,
                attempts,
                &format!("{} returned {}, retrying", provider, status),
            );
            sleep(Duration::from_secs(retry_after)).await;
            continue;
        }

        let text = response.text().await.unwrap_or_default();
        let preview: String = text.chars().take(500).collect();
        return Err(LauncherError::Download(format!(
            "{} API error ({}): {}",
            provider, status, preview
        )));
    }
    Err(last_err.unwrap_or_else(|| LauncherError::Download(format!("{} request failed", provider))))
}

/// Update the global proxy used by every HTTP request. Called when the
/// config is loaded and whenever the settings are saved.
pub fn set_global_proxy(proxy: Option<String>) {
    *PROXY_CFG.get_or_init(|| Mutex::new(None)).lock().unwrap() = proxy;
    *PROXY_RESOLVED
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = None;
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

/// Extract the total size from a `Content-Range` header
/// (`bytes 0-99/1234` → 1234, `bytes */1234` → 1234). Unknown totals (`*`)
/// and malformed values yield `None`.
fn content_range_total(value: &str) -> Option<u64> {
    value
        .trim()
        .strip_prefix("bytes")?
        .trim_start()
        .rsplit('/')
        .next()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Extract the start offset from a `Content-Range` header
/// (`bytes 100-199/1234` → 100). `bytes */1234` yields `None`.
fn content_range_start(value: &str) -> Option<u64> {
    value
        .trim()
        .strip_prefix("bytes")?
        .trim_start()
        .split('/')
        .next()?
        .split('-')
        .next()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Validate URL + host, then download to a .part temp file, validate,
/// and atomically rename to `path` on success. On ANY error the .part file
/// is cleaned up and `path` is never touched.
///
/// Downloads are RESUMABLE: the .part file is kept between attempts and
/// subsequent requests use a `Range` header so interrupted transfers
/// continue from the last byte instead of restarting from zero (important
/// on flaky connections). Servers that ignore `Range` simply restart.
pub(crate) async fn download_to_part(
    url: &str,
    path: &std::path::Path,
    expected_sha1: &str,
    expected_size: Option<u64>,
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
        .find(['/', ':', '?', '#'])
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

    // A leftover .part larger than the expected file is a different artifact
    // (mirror switch, new version) — drop it and restart fresh.
    if let Some(size) = expected_size {
        if let Ok(meta) = std::fs::metadata(&part_path) {
            if meta.len() > size {
                let _ = std::fs::remove_file(&part_path);
            }
        }
    }

    let client = http_client()?;
    let max_attempts = MAX_RETRIES as usize + 2;
    let mut offset: u64 = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
    let mut last_err: Option<String> = None;
    let mut complete = false;
    // After the body stream breaks once (proxy cutting mid-transfer), the
    // remaining attempts go straight to the host — retrying through the same
    // proxy would just repeat the stall. Progress is also kept monotonic so
    // the bar never walks backwards when a retry restarts from zero.
    let mut direct_only = false;
    let mut last_downloaded: u64 = 0;
    let mut known_total = expected_size;

    ensure_proxy_resolved().await;

    for attempt in 1..=max_attempts {
        if pause_requested() {
            return Err(LauncherError::Paused);
        }
        if attempt > 1 {
            // Sleep in 1-second steps so a pause request lands promptly even
            // during the backoff window.
            let secs = 1u64 << attempt.min(4);
            for _ in 0..secs {
                if pause_requested() {
                    return Err(LauncherError::Paused);
                }
                sleep(Duration::from_secs(1)).await;
            }
        }

        let mut req = client.get(&url);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        if offset > 0 {
            req = req.header("Range", format!("bytes={}-", offset));
        }

        let response = match if direct_only {
            req.send().await
        } else {
            send_with_fallback(req).await
        } {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(format!("send: {}", e));
                continue;
            }
        };

        let status = response.status();
        // Server says the range we asked for is past the end of the file.
        // That only means "done" if the bytes we already have match the real
        // size; a stale .part from another artifact must not be accepted as
        // complete just because the server rejected the range.
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            let range_total = response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(content_range_total);
            let part_len = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
            let total = expected_size.or(range_total);
            let size_ok =
                offset > 0 && total.map(|t| part_len == t && offset == t).unwrap_or(false);
            if size_ok {
                known_total = total;
                complete = true;
                break;
            }
            // Drop the unusable partial file and retry from zero.
            let partial_offset = offset;
            let _ = std::fs::remove_file(&part_path);
            offset = 0;
            last_err = Some(format!(
                "server rejected range at offset {} (partial {} bytes, expected {:?})",
                partial_offset, part_len, total
            ));
            if attempt < max_attempts {
                continue;
            }
            break;
        }
        if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(LauncherError::Download(format!(
                "HTTP {} for {}",
                status, &url
            )));
        }

        // Reject non-binary responses (only when no SHA1 to validate)
        if expected_sha1.is_empty() && attempt == 1 {
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

        let total_hint: Option<u64> = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(content_range_total);
        known_total = expected_size.or(total_hint);
        let content_length = response.content_length();

        // A 206 response must start exactly where our .part ends; otherwise
        // appending would splice bytes from two different ranges together.
        if status == reqwest::StatusCode::PARTIAL_CONTENT && offset > 0 {
            let start = response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(content_range_start);
            if start != Some(offset) {
                let _ = std::fs::remove_file(&part_path);
                last_err = Some(format!(
                    "server returned range starting at {:?}, expected {}",
                    start, offset
                ));
                offset = 0;
                if attempt < max_attempts {
                    continue;
                }
                break;
            }
        }

        // If the server ignored the Range header (plain 200), restart from zero.
        let mut file = if status == reqwest::StatusCode::PARTIAL_CONTENT && offset > 0 {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&part_path)
                .await
                .map_err(|e| LauncherError::Download(format!("Failed to open .part: {}", e)))?
        } else {
            offset = 0;
            tokio::fs::File::create(&part_path)
                .await
                .map_err(|e| LauncherError::Download(format!("Failed to create .part: {}", e)))?
        };

        // Stream the body chunk by chunk — flat memory usage even for large
        // files, and interrupted transfers keep their partial bytes.
        {
            use futures::StreamExt;
            use tokio::io::AsyncWriteExt;

            let mut stream = response.bytes_stream();
            let mut received: u64 = 0;
            let mut broken = false;
            let mut last_emit = std::time::Instant::now() - Duration::from_secs(1);
            // No-data timeout: if the server goes silent for 30s (no chunk at
            // all), abort the attempt so a stalled host doesn't block the
            // whole install for minutes (reqwest's overall timeout covers the
            // whole request and can be 120-900s).
            loop {
                match tokio::time::timeout(Duration::from_secs(30), stream.next()).await {
                    Ok(Some(chunk)) => {
                        if pause_requested() {
                            last_err = Some("paused by user".to_string());
                            broken = true;
                            break;
                        }
                        match chunk {
                            Ok(c) => {
                                file.write_all(&c).await.map_err(|e| {
                                    LauncherError::Download(format!("Failed to write .part: {}", e))
                                })?;
                                received += c.len() as u64;
                                if last_emit.elapsed() >= Duration::from_millis(300) {
                                    last_emit = std::time::Instant::now();
                                    let downloaded = offset + received;
                                    // Keep the progress bar monotonic: a retry that
                                    // restarts from zero (server ignoring Range) must
                                    // not walk the bar backwards.
                                    if downloaded > last_downloaded {
                                        last_downloaded = downloaded;
                                        crate::events::emit_file_progress(
                                            &url,
                                            downloaded,
                                            // Prefer the known file size (from the pack
                                            // manifest) over the Content-Range header;
                                            // never show "37/37 MB" when 100 MB is known.
                                            expected_size.or(total_hint).unwrap_or(downloaded),
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                last_err = Some(format!("body: {}", e));
                                broken = true;
                                direct_only = true;
                                break;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        last_err = Some("no data for 30s (stalled connection)".to_string());
                        broken = true;
                        direct_only = true;
                        break;
                    }
                }
            }
            file.flush()
                .await
                .map_err(|e| LauncherError::Download(format!("Failed to flush .part: {}", e)))?;
            offset += received;
            // A clean EOF is not proof of a complete transfer when the
            // server announced a body length: a proxy can terminate the
            // stream early and reqwest reports EOF without an error. Retry
            // (Range resumes where this attempt stopped) unless the number
            // of received bytes matches Content-Length exactly.
            if !broken {
                if let Some(expected_len) = content_length {
                    if received != expected_len {
                        last_err = Some(format!(
                            "truncated body: received {} of {} bytes",
                            received, expected_len
                        ));
                        broken = true;
                    }
                }
            }
            if !broken {
                complete = true;
                break;
            }
        }
    }

    if !complete {
        if pause_requested() {
            return Err(LauncherError::Paused);
        }
        let _ = std::fs::remove_file(&part_path);
        return Err(LauncherError::Download(format!(
            "Failed to download {}: {}",
            url,
            last_err.unwrap_or_else(|| "connection interrupted".to_string())
        )));
    }

    // Size check: when the manifest or Content-Range knows the total size,
    // reject files that don't match (truncated transfers, wrong artifact,
    // error pages).
    if let Some(expected) = known_total {
        let actual = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
        if actual != expected {
            let _ = std::fs::remove_file(&part_path);
            return Err(LauncherError::Download(format!(
                "Size mismatch for {}: expected {} bytes, got {}",
                path.display(),
                expected,
                actual
            )));
        }
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
pub async fn download_file(url: &str, path: &Path, expected_sha1: &str) -> Result<()> {
    tracing::info!(target: "launcher", url = %url, "Downloading {}", path.display());

    // Check if file exists with correct hash
    if path.exists() && !expected_sha1.is_empty() && verify_sha1(path, expected_sha1)? {
        tracing::debug!(target: "launcher", "SHA1 verified: {}", path.display());
        return Ok(());
    }

    let candidates: Vec<String> = std::iter::once(url.to_string())
        .chain(mirror_alternates(url))
        .collect();
    let mut last_err = None;

    for (ci, candidate) in candidates.iter().enumerate() {
        // Mirrors serve different URLs — a .part resumed from the primary
        // host would mix two files. Reset it when switching candidates.
        if ci > 0 {
            let part_path = {
                let mut p = path.as_os_str().to_os_string();
                p.push(".part");
                std::path::PathBuf::from(p)
            };
            let _ = std::fs::remove_file(&part_path);
        }
        // If mirrors exist, the primary host gets only one attempt so a
        // blocked/dead host fails over to a mirror quickly instead of
        // burning MAX_RETRIES worth of timeouts per file.
        let attempts = if candidates.len() > 1 && ci == 0 {
            1
        } else {
            MAX_RETRIES
        };
        for attempt in 1..=attempts {
            match download_to_part(
                candidate,
                path,
                expected_sha1,
                None,
                Some(timeout_for_unknown_size()),
            )
            .await
            {
                Ok(()) => return Ok(()),
                Err(e) if matches!(e, LauncherError::Paused) => return Err(e),
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

    Err(last_err.unwrap_or_else(|| LauncherError::Download(format!("Failed to download {}", url))))
}

/// Compute a reasonable download timeout from file size (~512 KB/s minimum + buffer).
pub fn timeout_for_size(size_bytes: u64) -> Duration {
    let secs = size_bytes / 512_000 + 60;
    Duration::from_secs(secs.clamp(120, 900))
}

/// Overall timeout for downloads whose size is unknown. Such downloads are
/// guarded against stalls by a per-chunk no-data timeout (30 s), so the
/// overall cap only needs to be generous enough for a large file on a slow
/// link (e.g. a Forge installer or a library of tens of MB) — the flat 120 s
/// HTTP-client default would otherwise abort a slow-but-steady transfer.
pub fn timeout_for_unknown_size() -> Duration {
    Duration::from_secs(900)
}

/// Download a file with streaming, retries, and a timeout scaled to expected size.
pub async fn download_file_sized(
    url: &str,
    path: &Path,
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
        if ci > 0 {
            let part_path = {
                let mut p = path.as_os_str().to_os_string();
                p.push(".part");
                std::path::PathBuf::from(p)
            };
            let _ = std::fs::remove_file(&part_path);
        }
        // Mirrors get full retries; the primary host gets a single attempt
        // so it can't stall the whole install with repeated timeouts.
        let attempts = if candidates.len() > 1 && ci == 0 {
            1
        } else {
            MAX_RETRIES
        };
        for attempt in 1..=attempts {
            match download_to_part(
                candidate,
                path,
                expected_sha1,
                Some(size_bytes),
                Some(timeout),
            )
            .await
            {
                Ok(()) => return Ok(()),
                Err(e) if matches!(e, LauncherError::Paused) => return Err(e),
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

    Err(last_err.unwrap_or_else(|| LauncherError::Download(format!("Failed to download {}", url))))
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

/// Download up to CONCURRENT_LIMIT files in parallel with progress callback.
///
/// The callback receives `(files_done, files_total, bytes_done, bytes_total,
/// message)`. Byte totals come from the known file sizes; per-file bytes done
/// are read from the actual on-disk size after each file completes, so the
/// numbers stay truthful even when the index reports size = 0.
pub async fn download_files(
    files: Vec<(String, PathBuf, String, u64)>,
    on_progress: impl Fn(usize, usize, u64, u64, &str) + Send + Sync,
) -> Result<()> {
    let total = files.len();
    let total_bytes: u64 = files.iter().map(|(_, _, _, s)| *s).sum();
    let mut completed = 0usize;
    let mut completed_bytes = 0u64;
    let mut errors = Vec::new();

    // Emit once upfront so the UI can show real totals from the start
    // instead of "0 / 0 MB".
    on_progress(0, total, 0, total_bytes, "Downloading...");

    for chunk in files.chunks(CONCURRENT_LIMIT) {
        let mut handles = Vec::with_capacity(chunk.len());

        for (url, path, sha1, size) in chunk {
            let url = url.clone();
            let path = path.clone();
            let sha1 = sha1.clone();
            let size = *size;

            handles.push((
                path.clone(),
                tokio::spawn(async move {
                    if size > 0 {
                        download_file_sized(&url, &path, &sha1, size).await
                    } else {
                        download_file(&url, &path, &sha1).await
                    }
                }),
            ));
        }

        for (path, handle) in handles {
            match handle.await {
                Ok(Ok(())) => {
                    completed += 1;
                    completed_bytes += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    on_progress(
                        completed,
                        total,
                        completed_bytes,
                        total_bytes,
                        "Downloading...",
                    );
                }
                Ok(Err(e)) => {
                    errors.push(e);
                    completed += 1;
                    on_progress(
                        completed,
                        total,
                        completed_bytes,
                        total_bytes,
                        "Downloading...",
                    );
                }
                Err(e) => {
                    errors.push(LauncherError::Download(format!("Task failed: {}", e)));
                    completed += 1;
                    on_progress(
                        completed,
                        total,
                        completed_bytes,
                        total_bytes,
                        "Downloading...",
                    );
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
    assets_dir: &Path,
    on_progress: impl Fn(usize, usize, u64, u64, &str) + Send + Sync,
) -> Result<()> {
    let objects_dir = assets_dir.join("objects");
    std::fs::create_dir_all(&objects_dir)?;
    tracing::info!(target: "launcher", count = asset_index.objects.len(), "Downloading game assets");

    let mut files: Vec<(String, PathBuf, String, u64)> = Vec::new();

    for obj in asset_index.objects.values() {
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
///
/// Modern indexes without virtual objects are skipped entirely.
pub fn ensure_virtual_assets(
    index_id: &str,
    asset_index: &crate::versions::AssetIndexData,
    assets_dir: &Path,
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
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Verify file SHA1 hash without loading the whole file into memory.
pub fn verify_sha1(path: &std::path::Path, expected: &str) -> Result<bool> {
    Ok(hash_file_sha1(path)? == expected)
}

/// Reject archive entry paths that could escape their extraction directory:
/// absolute paths, ".." components and Windows drive prefixes (e.g. "C:").
/// Callers normalize backslashes to forward slashes before splitting.
pub(crate) fn is_unsafe_archive_path(relative: &str) -> bool {
    let normalized = relative.replace('\\', "/");
    if normalized.starts_with('/') {
        return true;
    }
    if normalized.split('/').any(|c| c == "..") {
        return true;
    }
    let bytes = normalized.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Expose global client for use by other modules (versions, modloaders).
/// Returns a cheap clone; the underlying client is rebuilt automatically
/// when the proxy setting changes.
pub fn global_http_client() -> reqwest::Result<reqwest::Client> {
    http_client()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("voidlauncher_test_{}_{}", std::process::id(), name));
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

    #[cfg(windows)]
    #[test]
    fn win_proxy_server_plain_formats() {
        assert_eq!(
            parse_win_proxy_server("127.0.0.1:10808"),
            Some("127.0.0.1:10808".to_string())
        );
        assert_eq!(
            parse_win_proxy_server("http://127.0.0.1:8080"),
            Some("127.0.0.1:8080".to_string())
        );
        assert_eq!(parse_win_proxy_server("  "), None);
    }

    #[cfg(windows)]
    #[test]
    fn win_proxy_server_protocol_map_prefers_https() {
        assert_eq!(
            parse_win_proxy_server("http=127.0.0.1:80;https=127.0.0.1:443"),
            Some("127.0.0.1:443".to_string())
        );
        assert_eq!(
            parse_win_proxy_server("http=10.0.0.1:8080"),
            Some("10.0.0.1:8080".to_string())
        );
        assert_eq!(
            parse_win_proxy_server("socks=127.0.0.1:1080"),
            Some("127.0.0.1:1080".to_string())
        );
        // Empty https entry falls through to http.
        assert_eq!(
            parse_win_proxy_server("https=;http=127.0.0.1:88"),
            Some("127.0.0.1:88".to_string())
        );
    }

    #[test]
    fn unsafe_archive_path_rejects_traversal_and_absolutes() {
        assert!(is_unsafe_archive_path("../evil.dll"));
        assert!(is_unsafe_archive_path("a/b/../../evil.dll"));
        assert!(is_unsafe_archive_path("/abs/path.dll"));
        assert!(is_unsafe_archive_path("C:/windows/evil.dll"));
        assert!(is_unsafe_archive_path("..\\evil.dll"));
        assert!(is_unsafe_archive_path("a\\..\\b\\evil.dll"));

        // Safe relative paths must be accepted.
        assert!(!is_unsafe_archive_path("lwjgl.dll"));
        assert!(!is_unsafe_archive_path("org/lwjgl/foo.dll"));
        assert!(!is_unsafe_archive_path("jdk-21/bin/java.exe"));
    }

    // ── Download safety: validation, content-type, timeouts, mirrors ──

    #[test]
    fn sha1_verification_accepts_match_and_rejects_mismatch() {
        let p = temp_file("sha_ok.bin", b"abc");
        assert!(verify_sha1(&p, "a9993e364706816aba3e25717850c26c9cd0d89d").unwrap());
        assert!(!verify_sha1(&p, "0000000000000000000000000000000000000000").unwrap());
        assert_eq!(
            hash_file_sha1(&p).unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn validate_download_rejects_sha1_mismatch() {
        let p = temp_file("val_mismatch.bin", b"actual content");
        let err = validate_download(&p, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap_err();
        assert!(err.to_string().contains("SHA1 mismatch"), "got: {}", err);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn validate_download_accepts_correct_sha1() {
        let p = temp_file("val_ok.bin", b"abc");
        assert!(validate_download(&p, "a9993e364706816aba3e25717850c26c9cd0d89d").is_ok());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn validate_download_without_sha1_rejects_json_error_pages() {
        let p = temp_file("val_json.bin", br#"{"error":"not found","status":404}"#);
        let err = validate_download(&p, "").unwrap_err();
        assert!(err.to_string().contains("JSON"), "got: {}", err);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn validate_download_without_sha1_accepts_binary_and_empty() {
        let p = temp_file("val_bin.bin", &[0x50, 0x4b, 0x03, 0x04, 0x14, 0x00]);
        assert!(validate_download(&p, "").is_ok());
        let empty = temp_file("val_empty.bin", b"");
        assert!(validate_download(&empty, "").is_ok());
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&empty);
    }

    #[test]
    fn content_type_rejects_only_error_pages() {
        assert!(is_rejected_content_type("text/html; charset=utf-8"));
        assert!(is_rejected_content_type("TEXT/PLAIN"));
        assert!(is_rejected_content_type("application/json"));
        assert!(is_rejected_content_type(
            "application/problem+json; charset=utf-8"
        ));
        assert!(is_rejected_content_type("application/xml"));
        assert!(is_rejected_content_type("application/xhtml+xml"));

        assert!(!is_rejected_content_type("application/java-archive"));
        assert!(!is_rejected_content_type("application/octet-stream"));
        assert!(!is_rejected_content_type("application/zip"));
        assert!(!is_rejected_content_type("image/png"));
    }

    #[test]
    fn timeout_scales_with_size_and_stays_bounded() {
        // Small/zero-size files get the 120 s floor.
        assert_eq!(timeout_for_size(0), Duration::from_secs(120));
        assert_eq!(timeout_for_size(1024), Duration::from_secs(120));
        // ~512 KB/s + 60 s buffer, clamped to a 900 s ceiling.
        assert_eq!(timeout_for_size(512_000 * 300), Duration::from_secs(360));
        assert_eq!(timeout_for_size(512_000 * 10_000), Duration::from_secs(900));
        assert_eq!(timeout_for_unknown_size(), Duration::from_secs(900));
    }

    #[test]
    fn mirror_alternates_knows_the_known_hosts() {
        let neoforge =
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";
        let alts = mirror_alternates(neoforge);
        assert_eq!(alts.len(), 2);
        assert!(alts[0].starts_with("https://bmclapi2.bangbang93.com/maven/"));
        assert!(alts[1].starts_with("https://maven.creeperhost.net/"));
        assert!(alts[0].ends_with("net/neoforged/neoforge/maven-metadata.xml"));

        let lib = "https://libraries.minecraft.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar";
        let back = mirror_alternates(lib);
        assert_eq!(back.len(), 1);
        assert!(back[0].starts_with("https://maven.minecraftforge.net/"));

        // Unknown hosts get no alternates (and no surprise network calls).
        assert!(mirror_alternates("https://api.modrinth.com/v2/project/x").is_empty());
        assert!(mirror_alternates("https://example.com/file.jar").is_empty());
    }

    #[test]
    fn content_range_parsing_handles_partial_and_unsatisfied_forms() {
        assert_eq!(content_range_total("bytes 0-99/1234"), Some(1234));
        assert_eq!(content_range_total("bytes 100-199/*"), None);
        assert_eq!(content_range_total("bytes */1234"), Some(1234));
        assert_eq!(content_range_total("garbage"), None);

        assert_eq!(content_range_start("bytes 100-199/1234"), Some(100));
        assert_eq!(content_range_start("bytes */1234"), None);
        assert_eq!(content_range_start("bytes 0-0/1"), Some(0));
        assert_eq!(content_range_start("garbage"), None);
    }

    #[test]
    fn pausable_scope_gates_pause_requests() {
        // A pause request with no active install is a no-op.
        request_pause();
        assert!(!pause_requested());

        // While a scope is alive, a request pauses and is visible.
        let scope = PausableInstallScope::begin();
        request_pause();
        assert!(pause_requested());

        // A nested scope keeps the pause alive after the inner one drops.
        let inner = PausableInstallScope::begin();
        request_pause();
        assert!(pause_requested());
        drop(inner);
        assert!(pause_requested());

        // The last scope going away clears the leftover request, so the next
        // unrelated download can never be paused by it.
        drop(scope);
        assert!(!pause_requested());

        // A fresh scope always starts unpaused.
        let fresh = PausableInstallScope::begin();
        assert!(!pause_requested());
        drop(fresh);
    }
}

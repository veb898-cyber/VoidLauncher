use crate::error::{LauncherError, Result};
use crate::integrity::HashSpec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::time::sleep;

const MAX_RETRIES: u32 = 5;
const REQUEST_TIMEOUT_SECS: u64 = 120;
const CONCURRENT_LIMIT: usize = 32;

/// Absolute cap for one artifact written to disk.
///
/// Modpacks, game libraries, assets and JVM runtimes are all far below this;
/// the cap exists so a malicious or misbehaving server cannot fill the disk by
/// streaming an unbounded body (with no `Content-Length`, the downloader used
/// to write until the connection closed or the disk was full). It is checked
/// both against the advertised length and after every received chunk.
const MAX_SINGLE_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Global pause flag for modpack installs: when set, the currently active
/// download stops at the next chunk (keeping the .part file) and install
/// commands fail with `LauncherError::Paused`. Resuming re-runs the install
/// and `download_to_part` continues from the saved offset via Range.
static PAUSE_REQUESTED: AtomicBool = AtomicBool::new(false);

// Test-only fault injection for the file-operation paths that cannot be
// triggered portably otherwise (a real disk-full error, a failing flush).
// Compiled out of every non-test build.
#[cfg(test)]
thread_local! {
    static FAIL_NEXT_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_FLUSH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_CLIENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_next_write() {
    FAIL_NEXT_WRITE.with(|flag| flag.set(true));
}

#[cfg(test)]
fn fail_next_flush() {
    FAIL_NEXT_FLUSH.with(|flag| flag.set(true));
}

#[cfg(test)]
fn fail_next_client() {
    FAIL_NEXT_CLIENT.with(|flag| flag.set(true));
}

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

/// Only the default HTTPS port may be used. A non-default port would let an
/// allowlisted name point at an unrelated service on the same host.
const HTTPS_PORT: u16 = 443;

/// Maximum redirects followed by the shared clients. reqwest does not apply
/// its default limit to a custom policy, so the policy enforces it itself.
const MAX_REDIRECTS: usize = 10;

/// Check whether `host` is in the allowlist (exact or subdomain match).
/// Comparison is case-insensitive; the suffix must start at a DNS label
/// boundary, so `notgithub.com` never matches `github.com`.
pub fn is_host_allowed(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    ALLOWED_DOWNLOAD_HOSTS.iter().any(|allowed| {
        host == *allowed
            || host
                .strip_suffix(allowed)
                .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.'))
    })
}

/// Validate the parts shared by every downloader URL. The host allowlist is
/// deliberately left to the caller: the global downloader uses the launcher
/// allowlist, while pinned module clients use their own host set.
///
/// This is the single source of truth for URL safety. It parses the real
/// `reqwest::Url` instead of splitting the string by hand, so userinfo
/// (`https://cdn.modrinth.com:443@evil.example/`) cannot spoof the host,
/// backslashes cannot change the parser's host decision, and IP literals,
/// non-HTTPS schemes and non-default ports are rejected.
fn parse_https_download_url(raw: &str) -> Result<reqwest::Url> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(LauncherError::Download("Download URL is empty".to_string()));
    }
    // The URL parser normalises backslashes in special schemes; reject them
    // before parsing so a Windows-style path can never be reinterpreted.
    if raw.chars().any(|c| c.is_control() || c == '\\') {
        return Err(LauncherError::Download(
            "Download URL contains invalid characters".to_string(),
        ));
    }
    let url = reqwest::Url::parse(raw)
        .map_err(|_| LauncherError::Download("Invalid download URL".to_string()))?;
    if url.scheme() != "https" {
        return Err(LauncherError::Download(
            "Download URL must use HTTPS".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(LauncherError::Download(
            "Download URL must not contain a username or password".to_string(),
        ));
    }
    let host = url.host_str().unwrap_or("");
    if host.is_empty() {
        return Err(LauncherError::Download(
            "Download URL has no host".to_string(),
        ));
    }
    let ip_candidate = host.trim_start_matches('[').trim_end_matches(']');
    if ip_candidate.parse::<std::net::IpAddr>().is_ok() {
        return Err(LauncherError::Download(
            "Download URL must not use an IP address".to_string(),
        ));
    }
    if url.port().is_some_and(|port| port != HTTPS_PORT) {
        return Err(LauncherError::Download(
            "Download URL uses a non-standard port".to_string(),
        ));
    }
    Ok(url)
}

/// Validate a download URL against the launcher-wide allowlist.
pub(crate) fn validate_download_url(raw: &str) -> Result<reqwest::Url> {
    let url = parse_https_download_url(raw)?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if !is_host_allowed(&host) {
        return Err(LauncherError::Download(format!(
            "Download host '{}' is not in the allowlist",
            host
        )));
    }
    Ok(url)
}

/// Validate a download URL against a module-owned host set.
pub(crate) fn validate_download_url_for_hosts(raw: &str, hosts: &[String]) -> Result<reqwest::Url> {
    let url = parse_https_download_url(raw)?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let allowed = hosts.iter().any(|allowed| {
        host == *allowed
            || host
                .strip_suffix(allowed.as_str())
                .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.'))
    });
    if !allowed {
        return Err(LauncherError::Download(format!(
            "Download host '{}' is not in this module's allowlist",
            host
        )));
    }
    Ok(url)
}

fn redirect_target_allowed(url: &reqwest::Url, previous: usize) -> bool {
    previous < MAX_REDIRECTS && validate_download_url(url.as_str()).is_ok()
}

/// Authenticated API requests carry secrets (`x-api-key`) that reqwest does
/// not strip on a redirect. They may only follow a redirect to the exact same
/// HTTPS host; even another allowlisted download host is a different trust
/// boundary.
pub(crate) fn same_host_redirect_allowed(url: &reqwest::Url, host: &str, previous: usize) -> bool {
    previous < MAX_REDIRECTS
        && url.scheme() == "https"
        && url.host_str().is_some_and(|target| {
            target.eq_ignore_ascii_case(host) && url.port_or_known_default() == Some(443)
        })
}

pub(crate) fn same_host_redirect_policy(host: &str) -> reqwest::redirect::Policy {
    let host = host.to_ascii_lowercase();
    reqwest::redirect::Policy::custom(move |attempt| {
        if same_host_redirect_allowed(attempt.url(), &host, attempt.previous().len()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// Redirect policy: follow a redirect only if the destination is HTTPS, on
/// an allowlisted host, and the chain is still below the redirect limit.
/// reqwest follows up to 10 redirects by default without re-validating the
/// target, which would let a 302 from a trusted host escape to an arbitrary
/// host.
pub fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if redirect_target_allowed(attempt.url(), attempt.previous().len()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

type CachedClient = Mutex<Option<(Option<String>, reqwest::Client)>>;
static HTTP_CLIENT: OnceLock<CachedClient> = OnceLock::new();

/// Serialize downloads that target the same path. Two instance repairs can
/// legitimately run at once, but they may share the same library or asset;
/// letting both write the same `.part` would splice two responses together.
static DOWNLOAD_PATH_LOCKS: OnceLock<
    Mutex<HashMap<PathBuf, std::sync::Weak<tokio::sync::Mutex<()>>>>,
> = OnceLock::new();

/// Comparison form for a lock key. Two callers that name the same Windows
/// file must share one mutex even when their `PathBuf`s differ in case,
/// separators or `.`/`..` components. Canonicalization is deliberately not
/// used: the destination usually does not exist yet.
fn download_lock_key(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    // Resolve an existing parent so a junction/symlink alias and its real
    // directory share one mutex. The destination file itself usually does not
    // exist yet; when the parent is also missing, the lexical key is used and
    // all callers still agree on it.
    if let (Some(parent), Some(name)) = (normalized.parent(), normalized.file_name()) {
        if let Ok(resolved) = std::fs::canonicalize(parent) {
            normalized = resolved.join(name);
        }
    }
    #[cfg(windows)]
    {
        let text = normalized.to_string_lossy().replace('/', "\\");
        let text = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            rest.to_string()
        } else {
            text
        }
        .to_ascii_lowercase();
        PathBuf::from(text)
    }
    #[cfg(not(windows))]
    {
        normalized
    }
}

fn download_path_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let key = download_lock_key(path);
    let map = DOWNLOAD_PATH_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = guard.get(&key).and_then(std::sync::Weak::upgrade) {
        return existing;
    }
    if guard.len() > 2048 {
        guard.retain(|_, weak| weak.strong_count() > 0);
    }
    let created = Arc::new(tokio::sync::Mutex::new(()));
    guard.insert(key, Arc::downgrade(&created));
    created
}

/// Global HTTP client with connection pooling. On failure (rare: TLS backend
/// trouble) returns the error instead of panicking; nothing is cached, so the
/// next call retries the build.
fn http_client() -> Result<reqwest::Client> {
    #[cfg(test)]
    if FAIL_NEXT_CLIENT.with(|flag| flag.replace(false)) {
        return Err(LauncherError::Download(
            "injected HTTP client build failure (test)".to_string(),
        ));
    }
    let proxy = proxy_client_url()?;
    let mut slot = HTTP_CLIENT
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

fn build_client(proxy: Option<&str>) -> Result<reqwest::Client> {
    Ok(base_client_builder(proxy)?.build()?)
}

/// The shared request settings (timeouts, pooling, proxy handling, user
/// agent) used by both the global client and the per-request pinned clients.
/// A malformed proxy URL is an error, not a silent direct client: in
/// proxy-only mode that would leak traffic outside the proxy.
fn base_client_builder(proxy: Option<&str>) -> Result<reqwest::ClientBuilder> {
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
            let parsed = reqwest::Proxy::all(proxy.to_string()).map_err(|e| {
                LauncherError::Download(format!("Invalid proxy address '{}': {}", proxy, e))
            })?;
            builder = builder.proxy(parsed);
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
    Ok(builder)
}

/// Build a client that pins `host` to the given pre-validated IP addresses
/// (DNS-rebinding protection: the name is never re-resolved). Redirects are
/// NOT followed automatically — the caller validates and pins each hop.
/// Uses the configured proxy.
pub(crate) fn pinned_http_client(
    host: &str,
    addrs: &[std::net::IpAddr],
) -> Result<reqwest::Client> {
    let proxy = proxy_client_url()?;
    Ok(finish_pinned_client(
        base_client_builder(proxy.as_deref())?,
        host,
        addrs,
    )?)
}

/// Same as [`pinned_http_client`] but bypasses the proxy — the "proxy ->
/// direct" retry path for pinned requests. Refuses to build a direct client
/// while proxy-only mode is enabled.
pub(crate) fn pinned_http_client_direct(
    host: &str,
    addrs: &[std::net::IpAddr],
) -> Result<reqwest::Client> {
    if proxy_enabled() && proxy_only_enabled() {
        return Err(proxy_only_error());
    }
    Ok(finish_pinned_client(
        base_client_builder(None)?,
        host,
        addrs,
    )?)
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

/// Proxy URL configured in the settings (None = no explicit proxy).
static PROXY_CFG: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Master switch: when false, every client connects directly regardless of
/// the proxy-only checkbox. Defaults to false, matching `AppConfig::default`.
static PROXY_ENABLED: AtomicBool = AtomicBool::new(false);

/// When true, use the Windows system proxy; when false, the manual address.
/// The manual address is kept in `PROXY_CFG` either way.
static PROXY_USE_SYSTEM: AtomicBool = AtomicBool::new(false);

/// When true, a failed proxied request must never be retried directly.
/// Defaults to true, including for configs written before the field existed.
static PROXY_ONLY: AtomicBool = AtomicBool::new(true);

#[cfg(test)]
static SYSTEM_PROXY_TEST_OVERRIDE: OnceLock<Mutex<Option<Option<String>>>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn set_system_proxy_override(value: Option<String>) {
    *SYSTEM_PROXY_TEST_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = Some(value);
}

#[cfg(test)]
pub(crate) fn clear_system_proxy_override() {
    *SYSTEM_PROXY_TEST_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = None;
}

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
///
/// Supported: the static `ProxyEnable` + `ProxyServer` pair (plain host:port
/// or the `http=https=...` map). PAC/WPAD (`AutoConfigURL`) and `ProxyOverride`
/// bypass rules are intentionally not applied: the launcher needs one explicit
/// proxy for every request, and honouring bypass rules could turn a strict
/// proxy-only request into a direct connection.
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

/// Drop the cached Windows proxy when settings change so a source switch
/// re-reads the registry instead of using a value up to 60 s old.
#[cfg(windows)]
fn clear_system_proxy_cache() {
    if let Some(slot) = SYSTEM_PROXY_CACHE.get() {
        *slot.lock().unwrap() = None;
    }
}

#[cfg(not(windows))]
fn clear_system_proxy_cache() {}

/// The proxy the launcher should actively use: the explicit in-app setting
/// when present, otherwise the Windows system proxy (VPN apps like v2rayN set
/// it system-wide; WebView already follows it, so using it for backend traffic
/// too keeps everything consistent and toggle-safe). The master “Use proxy”
/// switch has priority: when it is off, this returns `None` even if a system
/// proxy exists, so the child “Use proxy only” flag cannot force traffic
/// through a proxy the user disabled. Non-strict mode re-probes an unreachable
/// proxy and may degrade to direct; proxy-only mode never does.
pub(crate) fn active_proxy_raw() -> Option<String> {
    if !proxy_enabled() {
        return None;
    }
    if use_system_proxy() {
        #[cfg(test)]
        {
            if let Some(value) = SYSTEM_PROXY_TEST_OVERRIDE
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap()
                .clone()
            {
                return value;
            }
        }
        return system_proxy_hint();
    }
    configured_proxy()
}

/// Whether the proxy source is the Windows system configuration.
pub fn use_system_proxy() -> bool {
    PROXY_USE_SYSTEM.load(Ordering::SeqCst)
}

/// Whether the master “Use proxy” setting is enabled.
pub fn proxy_enabled() -> bool {
    PROXY_ENABLED.load(Ordering::SeqCst)
}

/// Whether direct connections are forbidden after a proxy failure.
pub fn proxy_only_enabled() -> bool {
    PROXY_ONLY.load(Ordering::SeqCst)
}

fn proxy_only_error() -> LauncherError {
    LauncherError::Download(
        "Proxy-only mode is enabled, but no usable proxy is configured. \
         Check the manual proxy/server settings or the Windows system proxy, \
         or disable “Use proxy only”."
            .to_string(),
    )
}

/// Proxy URL the HTTP clients must use for the current policy.
///
/// The master switch wins: when the proxy is disabled the client is direct and
/// the child flag is irrelevant. When it is enabled, strict mode always returns
/// a proxy URL or an error (never a direct client); a proxy that failed the
/// scheme probe still yields the HTTP guess, so the request stays on the proxy
/// path and fails there instead of silently becoming a direct request.
pub(crate) fn proxy_client_url() -> Result<Option<String>> {
    if !proxy_enabled() {
        return Ok(None);
    }
    let raw = active_proxy_raw();
    if proxy_only_enabled() {
        let raw = raw.ok_or_else(proxy_only_error)?;
        return Ok(Some(resolved_proxy_url_strict(raw)));
    }
    Ok(resolved_proxy_url(raw))
}

fn resolved_proxy_url_strict(raw: String) -> String {
    if let Some((cached_raw, url)) = PROXY_RESOLVED
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
    {
        if cached_raw == raw {
            if let Some(url) = url {
                return url;
            }
        }
    }
    format!("http://{}", raw)
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

/// Build the short-lived client used to test a proxy. Redirects are disabled:
/// a probe only needs to prove that the proxy accepts and answers a request,
/// and following a redirect could turn the probe into a request to a host the
/// user never configured.
fn build_proxy_probe_client(proxy_url: &str) -> Option<reqwest::Client> {
    let proxy = reqwest::Proxy::all(proxy_url.to_string()).ok()?;
    reqwest::Client::builder()
        .proxy(proxy)
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(4))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()
}

async fn probe_proxy(proxy_url: &str, target: &str) -> bool {
    let Some(client) = build_proxy_probe_client(proxy_url) else {
        return false;
    };
    tokio::time::timeout(Duration::from_secs(4), client.get(target).send())
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
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
        if probe_proxy(url, "https://api.modrinth.com/").await {
            chosen = Some(url.clone());
            break;
        }
    }
    if chosen.is_none() {
        if proxy_only_enabled() {
            tracing::warn!(
                target: "launcher",
                "Proxy {}://{} unreachable — direct fallback is disabled by proxy-only mode",
                candidates[0],
                raw
            );
        } else {
            tracing::warn!(
                target: "launcher",
                "Proxy {}://{} unreachable — falling back to direct connection",
                candidates[0],
                raw
            );
        }
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
/// or failing an install. In proxy-only mode the fallback is skipped entirely:
/// the request fails on the proxy path instead of silently going direct.
pub async fn send_with_fallback(
    req: reqwest::RequestBuilder,
) -> std::result::Result<reqwest::Response, reqwest::Error> {
    send_with_fallback_inner(req, None).await
}

/// Same as [`send_with_fallback`], but the direct retry keeps the exact-host
/// redirect policy so an API credential cannot leave its origin.
pub(crate) async fn send_with_fallback_same_host(
    req: reqwest::RequestBuilder,
    host: &str,
) -> std::result::Result<reqwest::Response, reqwest::Error> {
    send_with_fallback_inner(req, Some(host.to_string())).await
}

async fn send_with_fallback_inner(
    req: reqwest::RequestBuilder,
    exact_host: Option<String>,
) -> std::result::Result<reqwest::Response, reqwest::Error> {
    // Proxy-only mode: the request is already bound to a proxy client; never
    // clone/rebuild it against a direct client after a failure.
    if proxy_only_enabled() {
        return req.send().await;
    }
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
                        let direct = match exact_host.as_deref() {
                            Some(host) => base_client_builder(None).and_then(|builder| {
                                builder
                                    .redirect(same_host_redirect_policy(host))
                                    .build()
                                    .map_err(Into::into)
                            }),
                            None => build_client(None),
                        };
                        let direct = match direct {
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
    get_with_retry_inner(req, provider, attempts, backoff_ms, None).await
}

/// [`get_with_retry`] for authenticated API requests: both the proxied request
/// and the direct proxy-fallback retry only follow same-host redirects.
pub(crate) async fn get_with_retry_same_host(
    req: reqwest::RequestBuilder,
    provider: &str,
    attempts: usize,
    backoff_ms: &[u64],
    host: &str,
) -> Result<reqwest::Response> {
    get_with_retry_inner(req, provider, attempts, backoff_ms, Some(host)).await
}

async fn get_with_retry_inner(
    req: reqwest::RequestBuilder,
    provider: &str,
    attempts: usize,
    backoff_ms: &[u64],
    exact_host: Option<&str>,
) -> Result<reqwest::Response> {
    let mut last_err: Option<LauncherError> = None;
    for attempt in 0..attempts {
        let attempt_req = match req.try_clone() {
            Some(r) => r,
            None => return req.send().await.map_err(LauncherError::Network),
        };
        let response = match exact_host {
            Some(host) => send_with_fallback_same_host(attempt_req, host).await,
            None => send_with_fallback(attempt_req).await,
        };
        let response = match response {
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

        let server_cloudfront = response
            .headers()
            .get(reqwest::header::SERVER)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("cloudfront"));
        let text = response.text().await.unwrap_or_default();
        // CurseForge is fronted by CloudFront, which occasionally answers a
        // valid request with a transient HTML "Request blocked" 403. Treat
        // only that shape as retriable; an actual API 403 (bad key, banned
        // project) is JSON and must fail immediately.
        if status == reqwest::StatusCode::FORBIDDEN
            && attempt < attempts - 1
            && is_cloudfront_block(server_cloudfront, &text)
        {
            let preview: String = text.chars().take(200).collect();
            last_err = Some(LauncherError::Download(format!(
                "{} API error ({}): {}",
                provider, status, preview
            )));
            crate::events::emit_fetch_retry(
                provider,
                attempt + 2,
                attempts,
                &format!("{} was temporarily blocked, retrying", provider),
            );
            sleep(Duration::from_millis(
                backoff_ms[attempt.min(backoff_ms.len() - 1)].max(750),
            ))
            .await;
            continue;
        }

        let preview: String = text.chars().take(500).collect();
        return Err(LauncherError::Download(format!(
            "{} API error ({}): {}",
            provider, status, preview
        )));
    }
    Err(last_err.unwrap_or_else(|| LauncherError::Download(format!("{} request failed", provider))))
}

/// CloudFront's WAF block page is HTML, while real CurseForge API errors are
/// JSON. Matching the known block-page phrases (or an HTML body served by
/// CloudFront) avoids retrying a genuine authorization failure.
fn is_cloudfront_block(server_cloudfront: bool, body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("request blocked")
        || lower.contains("the request could not be satisfied")
        || (server_cloudfront && lower.contains("<!doctype html"))
}

/// Update the global proxy used by every HTTP request. Called when the
/// config is loaded and whenever the settings are saved.
pub fn set_global_proxy_policy(
    proxy_enabled: bool,
    use_system_proxy: bool,
    proxy: Option<String>,
    proxy_only: bool,
) {
    PROXY_ENABLED.store(proxy_enabled, Ordering::SeqCst);
    // Child/source values are remembered even while the parent is off, but
    // the manual address itself is not used until the parent is back on.
    PROXY_USE_SYSTEM.store(use_system_proxy, Ordering::SeqCst);
    PROXY_ONLY.store(proxy_only, Ordering::SeqCst);
    let proxy = if proxy_enabled { proxy } else { None };
    *PROXY_CFG.get_or_init(|| Mutex::new(None)).lock().unwrap() = proxy;
    *PROXY_RESOLVED
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = None;
    // Drop the cached client: it may have been built for the previous policy.
    *HTTP_CLIENT.get_or_init(|| Mutex::new(None)).lock().unwrap() = None;
    clear_system_proxy_cache();
}

/// The total shown to the UI for a download attempt.
///
/// Priority: manifest size → Content-Range total → Content-Length of a full
/// 200 response. A 206's Content-Length is only the remaining range and must
/// not be mistaken for the artifact size; a missing/chunked length yields an
/// unknown (`None`) progress total.
fn download_progress_total(
    status: reqwest::StatusCode,
    expected_size: Option<u64>,
    range_total: Option<u64>,
    content_length: Option<u64>,
) -> Option<u64> {
    expected_size
        .or(range_total)
        .or(if status == reqwest::StatusCode::PARTIAL_CONTENT {
            None
        } else {
            content_length
        })
}

/// Validate a completed download file through the legacy SHA-1 API.
///
/// An empty value keeps the historical "no hash available" behavior; a
/// non-empty malformed value is rejected before any download starts instead of
/// being silently treated as "no verification".
#[cfg(test)]
fn validate_download(path: &std::path::Path, expected_sha1: &str) -> Result<()> {
    let expected = HashSpec::from_sha1(expected_sha1)?;
    validate_download_with_hash(path, expected.as_ref())
}

/// Validate a completed download against a typed hash.
fn validate_download_with_hash(path: &std::path::Path, expected: Option<&HashSpec>) -> Result<()> {
    if let Some(expected) = expected {
        if !expected.verify_file(path)? {
            return Err(LauncherError::Download(format!(
                "{} mismatch for {}",
                expected.algorithm().to_ascii_uppercase(),
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

/// Build a sibling path with an additional suffix (`client.jar` → `client.jar.part`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(suffix);
    PathBuf::from(p)
}

fn part_path_for(path: &Path) -> PathBuf {
    with_suffix(path, ".part")
}

fn part_meta_path(path: &Path) -> PathBuf {
    with_suffix(path, ".part.meta")
}

/// Remove the temporary files that belong to a failed download. The final
/// destination is never touched. Returns a description of cleanup failures so
/// the caller can report them next to (never instead of) the original error.
fn discard_partial_files(part_path: &Path, meta_path: &Path) -> Option<String> {
    let mut failures = Vec::new();
    for (path, label) in [(part_path, ".part"), (meta_path, ".part identity")] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failures.push(format!("{}: {}", label, e)),
        }
    }
    if failures.is_empty() {
        None
    } else {
        Some(format!(
            "failed to remove temporary download files ({})",
            failures.join("; ")
        ))
    }
}

/// Build a terminal download error. Temporary-file cleanup is performed once,
/// by [`download_to_part_observed`], for every error return that is not a
/// pause: a single owner avoids cleanup gaps on newly added return paths.
fn terminal_download_error(message: String) -> LauncherError {
    LauncherError::Download(message)
}

/// Remove the partial data and its identity after a failed attempt, appending
/// a warning to (never replacing) the original error when cleanup itself
/// fails. The final destination is never touched.
fn finish_failed_download(
    error: LauncherError,
    part_path: &Path,
    meta_path: &Path,
) -> LauncherError {
    match discard_partial_files(part_path, meta_path) {
        Some(warning) => LauncherError::Download(format!("{}; {}", error, warning)),
        None => error,
    }
}

/// A `.part` or `.part.meta` symlink would make `File::create` truncate or
/// append to its target instead of the download directory. Resuming through a
/// link is never legitimate, so refuse it before opening anything.
fn reject_linked_partial(path: &Path) -> Result<()> {
    for candidate in [part_path_for(path), part_meta_path(path)] {
        if let Ok(metadata) = std::fs::symlink_metadata(&candidate) {
            if metadata.file_type().is_symlink() {
                return Err(LauncherError::Download(format!(
                    "Refusing to write through a linked temporary file: {}",
                    candidate.display()
                )));
            }
        }
    }
    Ok(())
}

/// Identity of the artifact a `.part` belongs to. Resuming across a restart
/// is only safe when this matches; otherwise the bytes on disk belong to a
/// different file and must never be treated as a head start (or, when no
/// SHA1 is available, as the final file).
#[cfg(test)]
fn download_identity(url: &str, expected_sha1: &str, expected_size: Option<u64>) -> String {
    format!("{}\n{}\n{}", url, expected_sha1, expected_size.unwrap_or(0))
}

/// Identity for a typed hash. SHA-1 uses the bare hex payload so resumable
/// downloads created by older launcher versions keep their identity.
fn download_identity_with_hash(
    url: &str,
    expected: Option<&HashSpec>,
    expected_size: Option<u64>,
) -> String {
    let hash = expected.map(HashSpec::identity_payload).unwrap_or_default();
    format!("{}\n{}\n{}", url, hash, expected_size.unwrap_or(0))
}

/// Drop a stale `.part` whose sidecar identity does not match the current
/// download, then return the safe resume offset.
fn prepare_partial_download(path: &Path, identity: &str) -> u64 {
    let part_path = part_path_for(path);
    if !part_path.exists() {
        return 0;
    }
    let meta_path = part_meta_path(path);
    let matches = std::fs::read_to_string(&meta_path)
        .map(|stored| stored == identity)
        .unwrap_or(false);
    if !matches {
        tracing::debug!(
            target: "launcher",
            "Discarding stale .part for {} (artifact identity changed)",
            path.display()
        );
        let _ = std::fs::remove_file(&part_path);
        let _ = std::fs::remove_file(&meta_path);
        return 0;
    }
    std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0)
}

/// 429/5xx/408 are transient and worth retrying; other 4xx (404, 403,
/// malformed request) are permanent for this URL and should fail fast so the
/// caller can move to a mirror.
/// Backoff between candidate retries. Tests compress this so a public-API
/// retry-exhaustion test does not spend half a minute sleeping; production
/// keeps the historical exponential delay.
fn retry_backoff(attempt: u32) -> Duration {
    #[cfg(test)]
    {
        let _ = attempt;
        Duration::from_millis(1)
    }
    #[cfg(not(test))]
    {
        Duration::from_secs(2u64.pow(attempt))
    }
}

fn is_transient_download_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status.as_u16() >= 500
}

/// `Retry-After` in seconds, capped so a single host cannot stall an install
/// for minutes. Falls back to exponential backoff when the header is absent
/// or an HTTP-date (not parsed here).
fn retry_after_seconds(headers: &reqwest::header::HeaderMap, fallback: u64) -> u64 {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|secs| secs.clamp(1, 30))
        .unwrap_or(fallback)
}

/// Validate an asset-index SHA1 used as a two-level directory name. A
/// malformed hash (short, non-hex, path separators) must never be joined
/// into the assets directory.
pub fn is_valid_asset_hash(hash: &str) -> bool {
    hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Validate URL + host, then download to a .part temp file, validate,
/// and atomically rename to `path` on success. On ANY error the .part file
/// is cleaned up and `path` is never touched.
///
/// Downloads are RESUMABLE: the .part file is kept between attempts and
/// subsequent requests use a `Range` header so interrupted transfers
/// continue from the last byte instead of restarting from zero (important
/// on flaky connections). Servers that ignore `Range` simply restart.
/// Per-file chunk observer used by install progress. It receives the current
/// downloaded bytes and the best known total for that single file. `None`
/// means the total is genuinely unknown (the UI must stay indeterminate).
pub type FileChunkObserver = Arc<dyn Fn(u64, Option<u64>, u64) + Send + Sync>;
type FileProgressCallback = Arc<dyn Fn(&Path, u64, Option<u64>, u64) + Send + Sync>;
type FileDoneCallback = Arc<dyn Fn(&Path, u64) + Send + Sync>;

pub(crate) async fn download_to_part(
    url: &str,
    path: &std::path::Path,
    expected_sha1: &str,
    expected_size: Option<u64>,
    timeout: Option<Duration>,
) -> Result<()> {
    download_to_part_observed(url, path, expected_sha1, expected_size, timeout, None).await
}

pub(crate) async fn download_to_part_observed(
    url: &str,
    path: &std::path::Path,
    expected_sha1: &str,
    expected_size: Option<u64>,
    timeout: Option<Duration>,
    observer: Option<&FileChunkObserver>,
) -> Result<()> {
    let expected = HashSpec::from_sha1(expected_sha1)?;
    download_to_part_observed_with_hash(
        url,
        path,
        expected.as_ref(),
        expected_size,
        timeout,
        observer,
    )
    .await
}

async fn download_to_part_observed_with_hash(
    url: &str,
    path: &std::path::Path,
    expected: Option<&HashSpec>,
    expected_size: Option<u64>,
    timeout: Option<Duration>,
    observer: Option<&FileChunkObserver>,
) -> Result<()> {
    // One writer per destination: repairs of different instances may share
    // the same library/asset path, and a fixed `.part` file cannot serve two
    // concurrent transfers.
    let _path_guard = download_path_lock(path).lock_owned().await;
    // Auto-upgrade http:// to https://, then run the same URL policy for
    // every caller. This happens before any filesystem or network side
    // effect, so a spoofed URL can never initiate a request.
    let raw = url.trim();
    let upgraded = if raw
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
    {
        tracing::debug!(target: "launcher", "Upgrading http:// to https:// for: {}", raw);
        format!("https://{}", &raw[7..])
    } else {
        raw.to_string()
    };
    let url = if cfg!(test) && raw.starts_with("http://127.0.0.1:") {
        // Test-only: a controlled loopback HTTP origin for fault injection.
        // Release builds always validate against the HTTPS allowlist below.
        raw.to_string()
    } else {
        validate_download_url(&upgraded)?.to_string()
    };
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let part_path = part_path_for(path);
    let meta_path = part_meta_path(path);
    reject_linked_partial(path)?;

    // A leftover .part larger than the expected file is a different artifact
    // (mirror switch, new version) — drop it and restart fresh.
    if let Some(size) = expected_size {
        if let Ok(meta) = std::fs::metadata(&part_path) {
            if meta.len() > size {
                let _ = std::fs::remove_file(&part_path);
                let _ = std::fs::remove_file(&meta_path);
            }
        }
    }

    // ---- Single owner of the temporary files -------------------------------
    // Every non-pause error return below goes through the match at the end of
    // this block, which removes `.part` + identity exactly once. Retries keep
    // the files; a pause or an aborted task (future dropped) keeps them too.
    let result: Result<()> = async {
    // Do not create an identity (or any other temp file) for a call that is
    // already paused: nothing can be resumed yet.
    if pause_requested() {
        return Err(LauncherError::Paused);
    }
    // A .part without a matching identity sidecar cannot be trusted: the
    // target path may be reused for another artifact/version, and resuming
    // would splice two files together.
    let identity = download_identity_with_hash(&url, expected, expected_size);
    let mut offset = prepare_partial_download(path, &identity);
    if let Err(e) = std::fs::write(&meta_path, &identity) {
        return Err(terminal_download_error(format!(
            "Failed to write .part identity for {}: {}",
            path.display(),
            e
        )));
    }

    // Probe the proxy scheme before the client is built so the first attempt
    // already uses HTTP or SOCKS5 correctly. In proxy-only mode this matters
    // even more: a wrong-scheme guess must not silently degrade to direct.
    ensure_proxy_resolved().await;
    let client = http_client()?;
    let max_attempts = MAX_RETRIES as usize + 2;
    let mut last_err: Option<String> = None;
    let mut complete = false;
    // After the body stream breaks once (proxy cutting mid-transfer), the
    // remaining attempts go straight to the host — retrying through the same
    // proxy would just repeat the stall. Progress is also kept monotonic so
    // the bar never walks backwards when a retry restarts from zero.
    let mut direct_only = false;
    let mut last_downloaded: u64 = 0;
    let mut known_total = expected_size;

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
        if is_transient_download_status(status) {
            let fallback = 1u64 << attempt.min(4);
            let wait = retry_after_seconds(response.headers(), fallback);
            last_err = Some(format!("HTTP {} for {}", status, url));
            if attempt < max_attempts {
                tracing::warn!(
                    target: "launcher",
                    "Transient HTTP {} for {}; retrying in {}s",
                    status,
                    url,
                    wait
                );
                // Sleep in 1-second steps so a pause request lands promptly.
                for _ in 0..wait {
                    if pause_requested() {
                        return Err(LauncherError::Paused);
                    }
                    sleep(Duration::from_secs(1)).await;
                }
                continue;
            }
            break;
        }
        if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
            let detail = if status.is_redirection() {
                format!(
                    "HTTP {} for {} (redirect target is not allowed; response refused)",
                    status, url
                )
            } else {
                format!("HTTP {} for {}", status, url)
            };
            return Err(terminal_download_error(detail));
        }

        // Reject non-binary responses (only when no SHA1 to validate)
        if expected.is_none() && attempt == 1 {
            if let Some(ct) = response.headers().get("content-type") {
                let ct_str = ct.to_str().unwrap_or("");
                if is_rejected_content_type(ct_str) {
                    return Err(terminal_download_error(format!(
                        "Server returned unexpected content-type '{}' for {}",
                        ct_str, url
                    )));
                }
            }
        }

        let range_total: Option<u64> = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(content_range_total);
        let content_length = response.content_length();
        // The artifact's total size, when any trustworthy source knows it:
        // manifest size, or the total in this response's Content-Range.
        let size_ceiling = expected_size.or(range_total);
        // Reject an over-limit artifact before a single body byte is written.
        // A 206 length covers only the remainder, so add the resume offset; a
        // 200 length is the whole body and the file will be truncated first.
        let advertised_total = match content_length {
            Some(len) if status == reqwest::StatusCode::PARTIAL_CONTENT && offset > 0 => {
                match offset.checked_add(len) {
                    Some(total) => Some(total),
                    None => {
                        return Err(terminal_download_error(format!(
                            "Download size overflow for {}",
                            url
                        )))
                    }
                }
            }
            Some(len) => Some(len),
            None => None,
        };
        if let Some(total) = advertised_total {
            if total > MAX_SINGLE_DOWNLOAD_BYTES {
                return Err(terminal_download_error(format!(
                    "Refusing to download {}: server advertises {} bytes, above the {} byte limit",
                    url, total, MAX_SINGLE_DOWNLOAD_BYTES
                )));
            }
            if let Some(ceiling) = size_ceiling {
                if total > ceiling {
                    return Err(terminal_download_error(format!(
                        "Server advertises {} bytes for {}, but the expected artifact size is {} bytes",
                        total, url, ceiling
                    )));
                }
            }
        }
        // The final size check keeps using only manifest/Content-Range data.
        // A 200 response's Content-Length is still the best progress hint (and
        // its truncation detector below), but a 206's Content-Length describes
        // only the remaining range and must never become the total.
        known_total = size_ceiling;
        let progress_total =
            download_progress_total(status, expected_size, range_total, content_length);

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

        // Reset the per-file prepared contribution for this attempt. On a
        // resume this is the valid .part offset; on a retry it is 0. The
        // network counter gets no delta for bytes received in an earlier
        // operation/attempt.
        if let Some(observer) = observer {
            observer(offset, progress_total, 0);
        }
        crate::events::emit_file_progress(&url, &file_name, offset, progress_total);

        // Stream the body chunk by chunk — flat memory usage even for large
        // files, and interrupted transfers keep their partial bytes.
        {
            use futures::StreamExt;
            use tokio::io::AsyncWriteExt;

            let mut stream = response.bytes_stream();
            let mut received: u64 = 0;
            let mut network_since_emit: u64 = 0;
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
                                // Streaming cap: Content-Length can be absent or
                                // lie, so the bytes actually written are checked
                                // before every write. Returns terminate the
                                // attempt; the central cleanup removes the
                                // partial file after the handle is dropped.
                                let projected = match offset
                                    .checked_add(received)
                                    .and_then(|total| total.checked_add(c.len() as u64))
                                {
                                    Some(total) => total,
                                    None => {
                                        return Err(terminal_download_error(format!(
                                            "Download size overflow for {}",
                                            url
                                        )))
                                    }
                                };
                                if projected > MAX_SINGLE_DOWNLOAD_BYTES {
                                    return Err(terminal_download_error(format!(
                                        "Refusing to download {}: received more than the {} byte limit",
                                        url, MAX_SINGLE_DOWNLOAD_BYTES
                                    )));
                                }
                                if let Some(ceiling) = known_total {
                                    if projected > ceiling {
                                        return Err(terminal_download_error(format!(
                                            "Server sent more than the expected {} bytes for {}",
                                            ceiling, url
                                        )));
                                    }
                                }
                                #[cfg(test)]
                                if FAIL_NEXT_WRITE.with(|flag| flag.replace(false)) {
                                    return Err(terminal_download_error(
                                        "injected .part write failure (test)".to_string(),
                                    ));
                                }
                                file.write_all(&c).await.map_err(|e| {
                                    LauncherError::Download(format!("Failed to write .part: {}", e))
                                })?;
                                received += c.len() as u64;
                                network_since_emit += c.len() as u64;
                                if last_emit.elapsed() >= Duration::from_millis(300) {
                                    last_emit = std::time::Instant::now();
                                    let downloaded = offset + received;
                                    if let Some(observer) = observer {
                                        if network_since_emit > 0 {
                                            observer(
                                                downloaded,
                                                progress_total,
                                                network_since_emit,
                                            );
                                            network_since_emit = 0;
                                        }
                                    }
                                    // Keep the pack/mod progress bar monotonic:
                                    // a retry that restarts from zero must not
                                    // walk that global bar backwards. The install
                                    // observer above deliberately receives the
                                    // lower prepared value and the new network
                                    // delta.
                                    if downloaded > last_downloaded {
                                        last_downloaded = downloaded;
                                        crate::events::emit_file_progress(
                                            &url,
                                            &file_name,
                                            downloaded,
                                            progress_total,
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
            #[cfg(test)]
            if FAIL_NEXT_FLUSH.with(|flag| flag.replace(false)) {
                return Err(terminal_download_error(
                    "injected .part flush failure (test)".to_string(),
                ));
            }
            file.flush()
                .await
                .map_err(|e| LauncherError::Download(format!("Failed to flush .part: {}", e)))?;
            if let Some(observer) = observer {
                if network_since_emit > 0 {
                    observer(offset + received, progress_total, network_since_emit);
                }
            }
            crate::events::emit_file_progress(&url, &file_name, offset + received, progress_total);
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
        return Err(terminal_download_error(format!(
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
            return Err(terminal_download_error(format!(
                "Size mismatch for {}: expected {} bytes, got {}",
                path.display(),
                expected,
                actual
            )));
        }
    }

    // Validate the .part file content
    if let Err(e) = validate_download_with_hash(&part_path, expected) {
        return Err(terminal_download_error(e.to_string()));
    }

    // Atomically replace the destination: rename .part → final path. The
    // destination is not touched before every size/checksum check passed.
    std::fs::rename(&part_path, path)?;

    // The destination is already published; a leftover sidecar cannot corrupt
    // it (the next download overwrites the identity), but surface the failed
    // cleanup in the log instead of swallowing it.
    if let Err(e) = std::fs::remove_file(&meta_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                target: "launcher",
                "Downloaded {}, but could not remove its identity sidecar {}: {}",
                path.display(),
                meta_path.display(),
                e
            );
        }
    }

    Ok(())
    }
    .await;

    match result {
        Ok(()) => Ok(()),
        // A pause is an intentional interruption: keep `.part` + identity so
        // resuming continues from the saved offset.
        Err(LauncherError::Paused) => Err(LauncherError::Paused),
        // Every other error is terminal for this attempt/URL. Cleanup happens
        // here, after all file handles inside the block were dropped.
        Err(error) => Err(finish_failed_download(error, &part_path, &meta_path)),
    }
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
    download_file_observed(url, path, expected_sha1, None).await
}

pub async fn download_file_observed(
    url: &str,
    path: &Path,
    expected_sha1: &str,
    observer: Option<FileChunkObserver>,
) -> Result<()> {
    let expected = HashSpec::from_sha1(expected_sha1)?;
    download_file_observed_with_hash(url, path, expected.as_ref(), observer).await
}

/// Download a single file and verify it against a typed hash.
///
/// The file is written to `.part`, verified before publication, and only then
/// atomically renamed. This is the preferred entry point for new callers.
///
/// The follow-up content-install PR migrates remote installs to this function;
/// it is kept here as the stable foundation for that migration.
#[allow(dead_code)]
pub async fn download_file_verified(url: &str, path: &Path, expected: &HashSpec) -> Result<()> {
    download_file_observed_with_hash(url, path, Some(expected), None).await
}

async fn download_file_observed_with_hash(
    url: &str,
    path: &Path,
    expected: Option<&HashSpec>,
    observer: Option<FileChunkObserver>,
) -> Result<()> {
    tracing::info!(target: "launcher", url = %url, "Downloading {}", path.display());

    // Check if file exists with correct hash
    if path.exists() {
        if let Some(expected) = expected {
            if expected.verify_file(path)? {
                tracing::debug!(
                    target: "launcher",
                    "{} verified: {}",
                    expected.algorithm().to_ascii_uppercase(),
                    path.display()
                );
                if let Some(observer) = observer.as_ref() {
                    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    observer(size, Some(size), 0);
                }
                return Ok(());
            }
        }
    }

    let candidates: Vec<String> = std::iter::once(url.to_string())
        .chain(mirror_alternates(url))
        .collect();
    let mut last_err = None;

    for (ci, candidate) in candidates.iter().enumerate() {
        // Mirrors serve different URLs — a .part resumed from the primary
        // host would mix two files. Reset it when switching candidates.
        if ci > 0 {
            let part_path = part_path_for(path);
            let _ = std::fs::remove_file(&part_path);
            let _ = std::fs::remove_file(part_meta_path(path));
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
            match download_to_part_observed_with_hash(
                candidate,
                path,
                expected,
                None,
                Some(timeout_for_unknown_size()),
                observer.as_ref(),
            )
            .await
            {
                Ok(()) => return Ok(()),
                Err(e) if matches!(e, LauncherError::Paused) => return Err(e),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < attempts {
                        sleep(retry_backoff(attempt)).await;
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
    download_file_sized_observed(url, path, expected_sha1, size_bytes, None).await
}

pub async fn download_file_sized_observed(
    url: &str,
    path: &Path,
    expected_sha1: &str,
    size_bytes: u64,
    observer: Option<FileChunkObserver>,
) -> Result<()> {
    let expected = HashSpec::from_sha1(expected_sha1)?;
    download_file_sized_observed_with_hash(url, path, expected.as_ref(), size_bytes, observer).await
}

async fn download_file_sized_observed_with_hash(
    url: &str,
    path: &Path,
    expected: Option<&HashSpec>,
    size_bytes: u64,
    observer: Option<FileChunkObserver>,
) -> Result<()> {
    tracing::info!(target: "launcher", url = %url, size = size_bytes, "Downloading {} (sized)", path.display());

    if path.exists() {
        if let Some(expected) = expected {
            if expected.verify_file(path)? {
                tracing::debug!(
                    target: "launcher",
                    "{} verified: {}",
                    expected.algorithm().to_ascii_uppercase(),
                    path.display()
                );
                if let Some(observer) = observer.as_ref() {
                    let size = std::fs::metadata(path)
                        .map(|m| m.len())
                        .unwrap_or(size_bytes);
                    observer(size, Some(size), 0);
                }
                return Ok(());
            }
        }
    }

    let timeout = timeout_for_size(size_bytes);
    let candidates: Vec<String> = std::iter::once(url.to_string())
        .chain(mirror_alternates(url))
        .collect();
    let mut last_err = None;

    for (ci, candidate) in candidates.iter().enumerate() {
        if ci > 0 {
            let part_path = part_path_for(path);
            let _ = std::fs::remove_file(&part_path);
            let _ = std::fs::remove_file(part_meta_path(path));
        }
        // Mirrors get full retries; the primary host gets a single attempt
        // so it can't stall the whole install with repeated timeouts.
        let attempts = if candidates.len() > 1 && ci == 0 {
            1
        } else {
            MAX_RETRIES
        };
        for attempt in 1..=attempts {
            match download_to_part_observed_with_hash(
                candidate,
                path,
                expected,
                Some(size_bytes),
                Some(timeout),
                observer.as_ref(),
            )
            .await
            {
                Ok(()) => return Ok(()),
                Err(e) if matches!(e, LauncherError::Paused) => return Err(e),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < attempts {
                        sleep(retry_backoff(attempt)).await;
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

/// Remove duplicate target paths from a download batch. Two concurrent
/// downloads writing the same file (duplicate metadata entries, path casing
/// on Windows) would race and could leave a validated file overwritten by an
/// unvalidated one. The first entry wins; duplicates are the same artifact.
fn dedupe_download_targets(
    files: Vec<(String, PathBuf, String, u64)>,
) -> Vec<(String, PathBuf, String, u64)> {
    let mut seen = std::collections::HashSet::new();
    files
        .into_iter()
        .filter(|(_, path, _, _)| {
            // Windows filesystems are case-insensitive; compare lowercased
            // lossy strings so `Libraries/A.jar` and `libraries/a.jar` are
            // treated as the same target.
            seen.insert(path.to_string_lossy().to_ascii_lowercase())
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadCachePolicy {
    /// Content-addressed artifacts (libraries, client, assets): verify SHA1
    /// when known, otherwise require the declared size. When neither is
    /// available, use the local integrity manifest written after the original
    /// HTTPS download.
    VerifySha1,
    /// Size-only cache proof, kept for callers where re-hashing a very large
    /// content-addressed store would be prohibitively expensive. The install
    /// pipeline uses `VerifySha1` for assets as well.
    SizeOnly,
}

const INTEGRITY_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct IntegrityManifest {
    version: u32,
    url: String,
    size: u64,
    sha256: String,
}

fn integrity_manifest_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    Some(path.with_file_name(format!(".{name}.voidlauncher-integrity.json")))
}

/// Record the identity/hash of an artifact downloaded without publisher
/// checksum or size metadata. This detects later local corruption; it is not
/// a publisher signature and does not independently prove provenance.
pub(crate) fn write_integrity_manifest(path: &Path, source_url: &str) -> Result<()> {
    let Some(manifest_path) = integrity_manifest_path(path) else {
        return Ok(());
    };
    let size = std::fs::metadata(path)
        .map_err(|error| {
            LauncherError::Download(format!("Cannot stat {}: {error}", path.display()))
        })?
        .len();
    let sha256 = hash_file_sha256(path)?;
    let manifest = IntegrityManifest {
        version: INTEGRITY_MANIFEST_VERSION,
        url: source_url.to_string(),
        size,
        sha256,
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        LauncherError::Download(format!("Cannot encode integrity manifest: {error}"))
    })?;
    crate::instances::write_file_atomic(&manifest_path, &json)?;
    Ok(())
}

pub(crate) fn verify_integrity_manifest(path: &Path, source_url: &str) -> bool {
    let Some(manifest_path) = integrity_manifest_path(path) else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<IntegrityManifest>(&raw) else {
        return false;
    };
    if manifest.version != INTEGRITY_MANIFEST_VERSION
        || manifest.url != source_url
        || manifest.sha256.len() != 64
        || !manifest.sha256.chars().all(|c| c.is_ascii_hexdigit())
    {
        return false;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() != manifest.size {
        return false;
    }
    hash_file_sha256(path)
        .map(|actual| actual.eq_ignore_ascii_case(&manifest.sha256))
        .unwrap_or(false)
}

fn cached_download_size(
    path: &Path,
    expected_sha1: &str,
    expected_size: u64,
    policy: DownloadCachePolicy,
    source_url: &str,
) -> Option<u64> {
    if !path.exists() {
        return None;
    }
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let actual = metadata.len();
    let valid = match policy {
        DownloadCachePolicy::VerifySha1 => {
            if !expected_sha1.is_empty() {
                verify_sha1(path, expected_sha1).unwrap_or(false)
            } else if expected_size > 0 {
                actual == expected_size
            } else {
                // Maven fallback: no publisher checksum or size. Only a local
                // manifest recorded after an earlier successful HTTPS download
                // can prove this file is still the bytes we received.
                verify_integrity_manifest(path, source_url)
            }
        }
        DownloadCachePolicy::SizeOnly => expected_size == 0 || actual == expected_size,
    };
    valid.then_some(actual)
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
    download_files_observed(
        files,
        DownloadCachePolicy::VerifySha1,
        on_progress,
        |_, _, _, _| {},
        |_, _| {},
    )
    .await
}

/// Same as [`download_files`], plus per-file chunk/completion observers and an
/// explicit cache policy. Install progress uses this to aggregate real bytes
/// across parallel files without changing the pack/mod update callers.
pub async fn download_files_observed(
    files: Vec<(String, PathBuf, String, u64)>,
    cache_policy: DownloadCachePolicy,
    on_progress: impl Fn(usize, usize, u64, u64, &str) + Send + Sync,
    on_file_progress: impl Fn(&Path, u64, Option<u64>, u64) + Send + Sync + 'static,
    on_file_done: impl Fn(&Path, u64) + Send + Sync + 'static,
) -> Result<()> {
    let files = dedupe_download_targets(files);
    let total = files.len();
    // A sum that silently treats unknown sizes as zero would let a large
    // unknown file look complete early. Report a byte total only when every
    // planned file has a trustworthy size; otherwise the UI falls back to
    // indeterminate/count progress.
    let all_sizes_known = files.iter().all(|(_, _, _, size)| *size > 0);
    let total_bytes: u64 = if all_sizes_known {
        files.iter().map(|(_, _, _, s)| *s).sum()
    } else {
        0
    };
    let mut completed = 0usize;
    let mut completed_bytes = 0u64;
    let mut errors = Vec::new();
    let progress_cb: FileProgressCallback = Arc::new(on_file_progress);
    let done_cb: FileDoneCallback = Arc::new(on_file_done);

    // Cached files are part of the completed plan. Invalid caches are left in
    // the pending list and will be downloaded/verified again. Hashing runs on
    // the blocking pool so a large cache does not stall the async runtime.
    let (cached, pending) = tokio::task::spawn_blocking(move || {
        let mut cached = Vec::new();
        let mut pending = Vec::new();
        for (url, path, sha1, size) in files {
            if let Some(actual) = cached_download_size(&path, &sha1, size, cache_policy, &url) {
                cached.push((url, path, actual));
            } else {
                pending.push((url, path, sha1, size));
            }
        }
        (cached, pending)
    })
    .await
    .map_err(|error| LauncherError::Download(format!("Cache validation failed: {error}")))?;

    for (_url, path, actual) in cached {
        completed += 1;
        completed_bytes = completed_bytes.saturating_add(actual);
        progress_cb(&path, actual, Some(actual), 0);
        done_cb(&path, actual);
    }

    on_progress(
        completed,
        total,
        completed_bytes,
        total_bytes,
        "Downloading...",
    );

    for chunk in pending.chunks(CONCURRENT_LIMIT) {
        let mut handles = Vec::with_capacity(chunk.len());

        for (url, path, sha1, size) in chunk {
            let url = url.clone();
            let path = path.clone();
            let sha1 = sha1.clone();
            let size = *size;
            let progress_cb = progress_cb.clone();
            let callback_path = path.clone();
            let observer: FileChunkObserver = Arc::new(move |downloaded, total, network| {
                progress_cb(&callback_path, downloaded, total, network);
            });

            handles.push((
                path.clone(),
                url.clone(),
                sha1.clone(),
                size,
                tokio::spawn(async move {
                    if size > 0 {
                        download_file_sized_observed(&url, &path, &sha1, size, Some(observer)).await
                    } else {
                        download_file_observed(&url, &path, &sha1, Some(observer)).await
                    }
                }),
            ));
        }

        for (path, url, sha1, size, handle) in handles {
            match handle.await {
                Ok(Ok(())) => {
                    // Maven fallback artifacts have neither a publisher SHA-1
                    // nor a declared size. Record the bytes we received so a
                    // later cache check can detect local corruption instead of
                    // trusting bare file existence.
                    if sha1.is_empty() && size == 0 {
                        if let Err(error) = write_integrity_manifest(&path, &url) {
                            tracing::warn!(
                                target: "launcher",
                                "Could not write integrity manifest for {}: {}",
                                path.display(),
                                error
                            );
                        }
                    }
                    let actual = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    completed += 1;
                    completed_bytes = completed_bytes.saturating_add(actual);
                    progress_cb(&path, actual, Some(actual), 0);
                    done_cb(&path, actual);
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

/// Build the complete asset download plan, including objects already present
/// on disk. The caller decides which entries are cached; install progress needs
/// the full plan so cached objects count in the cumulative total.
pub fn collect_asset_download_plan(
    asset_index: &crate::versions::AssetIndexData,
    assets_dir: &Path,
) -> Result<Vec<(String, PathBuf, String, u64)>> {
    let objects_dir = assets_dir.join("objects");
    std::fs::create_dir_all(&objects_dir)?;
    let mut files: Vec<(String, PathBuf, String, u64)> = Vec::new();

    for obj in asset_index.objects.values() {
        if !is_valid_asset_hash(&obj.hash) {
            return Err(LauncherError::Download(format!(
                "Invalid asset hash in index: {:?}",
                obj.hash
            )));
        }
        let hash_prefix = &obj.hash[..2];
        let path = objects_dir.join(hash_prefix).join(&obj.hash);
        let url = format!(
            "https://resources.download.minecraft.net/{}/{}",
            hash_prefix, obj.hash
        );
        files.push((url, path, obj.hash.clone(), obj.size));
    }
    Ok(files)
}

/// Download assets from Mojang
#[allow(dead_code)] // used by smoke tests; launch/import paths use the repair variant
pub async fn download_assets(
    asset_index: &crate::versions::AssetIndexData,
    assets_dir: &Path,
    on_progress: impl Fn(usize, usize, u64, u64, &str) + Send + Sync,
) -> Result<()> {
    tracing::info!(target: "launcher", count = asset_index.objects.len(), "Downloading game assets");

    let files = collect_asset_download_plan(asset_index, assets_dir)?
        .into_iter()
        .filter(|(url, path, _, size)| {
            cached_download_size(path, "", *size, DownloadCachePolicy::SizeOnly, url).is_none()
        })
        .collect::<Vec<_>>();

    if files.is_empty() {
        tracing::info!(target: "launcher", "Game assets download complete");
        return Ok(());
    }

    download_files(files, on_progress).await?;
    tracing::info!(target: "launcher", "Game assets download complete");
    Ok(())
}

/// Plan assets that are missing, wrong-size, or explicitly forced because a
/// repair found them corrupt. The force set matters because a corrupt object
/// may have the expected size: the cheap size-only prefilter would otherwise
/// skip it. `download_files` re-verifies the SHA-1 and replaces the object
/// atomically, so the old copy stays readable until a verified one exists.
fn collect_asset_download_plan_forced(
    asset_index: &crate::versions::AssetIndexData,
    assets_dir: &Path,
    forced: &std::collections::HashSet<PathBuf>,
) -> Result<Vec<(String, PathBuf, String, u64)>> {
    Ok(collect_asset_download_plan(asset_index, assets_dir)?
        .into_iter()
        .filter(|(url, path, _, size)| {
            forced.contains(path)
                || cached_download_size(path, "", *size, DownloadCachePolicy::SizeOnly, url)
                    .is_none()
        })
        .collect())
}

/// Repair variant of [`download_assets`]: corrupt objects are re-downloaded
/// with the normal `.part` + SHA-1 + atomic rename path instead of being
/// deleted first. A failed download therefore leaves the previous object in
/// place, and an external game never observes a file disappear.
pub async fn download_assets_repair(
    asset_index: &crate::versions::AssetIndexData,
    assets_dir: &Path,
    forced: &std::collections::HashSet<PathBuf>,
    on_progress: impl Fn(usize, usize, u64, u64, &str) + Send + Sync,
) -> Result<()> {
    tracing::info!(
        target: "launcher",
        count = asset_index.objects.len(),
        forced = forced.len(),
        "Downloading game assets for repair"
    );

    let files = collect_asset_download_plan_forced(asset_index, assets_dir, forced)?;
    if files.is_empty() {
        tracing::info!(target: "launcher", "Game assets repair complete");
        return Ok(());
    }

    download_files(files, on_progress).await?;
    tracing::info!(target: "launcher", "Game assets repair complete");
    Ok(())
}

/// Replace a virtual/legacy asset atomically. Two instances (or two repair
/// workers in one launcher) may mirror the same global index concurrently;
/// neither may expose a truncated file to the game. The temporary name is
/// unique, and the final rename replaces the previous copy in one step.
fn replace_virtual_asset(src: &Path, dest: &Path) -> Result<()> {
    let parent = dest.parent().ok_or_else(|| {
        LauncherError::Download(format!("Virtual asset has no parent: {}", dest.display()))
    })?;
    std::fs::create_dir_all(parent)?;
    let name = dest
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("asset");
    let tmp = parent.join(format!(".{}.{}.tmp", name, uuid::Uuid::new_v4().simple()));
    if std::fs::hard_link(src, &tmp).is_err() {
        if let Err(error) = std::fs::copy(src, &tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error.into());
        }
    }
    if let Err(error) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
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
        if !is_valid_asset_hash(&obj.hash) {
            return Err(LauncherError::Download(format!(
                "Invalid asset hash in index: {:?}",
                obj.hash
            )));
        }
        // Asset object names become paths under virtual/legacy. Treat them
        // like zip entries: a crafted index must not escape that directory.
        crate::import::check_safe_relative(name).map_err(|_| {
            LauncherError::Download(format!("Unsafe asset name in index: {:?}", name))
        })?;
        let src = objects_dir.join(&obj.hash[..2]).join(&obj.hash);
        if !src.exists() {
            continue;
        }
        let dest = virtual_dir.join(name);
        if !dest.starts_with(&virtual_dir) {
            return Err(LauncherError::Download(format!(
                "Unsafe asset name in index: {:?}",
                name
            )));
        }
        if dest.exists() {
            let skip = std::fs::metadata(&dest)
                .map(|m| m.len() == obj.size)
                .unwrap_or(false);
            if skip {
                continue;
            }
        }
        replace_virtual_asset(&src, &dest)?;
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

/// Compute SHA-256 hash of a file and return a lowercase hex string.
///
/// Used for pinned third-party runtime components (authlib-injector): their
/// origin publishes a SHA-256, not the SHA-1 used by Minecraft metadata.
pub fn hash_file_sha256(path: &std::path::Path) -> Result<String> {
    crate::integrity::hash_file_sha256(path)
}

/// Verify a SHA-256 hex digest without loading the whole file into memory.
pub fn verify_sha256(path: &std::path::Path, expected: &str) -> Result<bool> {
    crate::integrity::verify_sha256(path, expected)
}

/// Compute SHA1 hash of a file and return hex string
pub fn hash_file_sha1(path: &std::path::Path) -> Result<String> {
    crate::integrity::hash_file_sha1(path)
}

/// Verify file SHA1 hash without loading the whole file into memory.
pub fn verify_sha1(path: &std::path::Path, expected: &str) -> Result<bool> {
    crate::integrity::verify_sha1(path, expected)
}

/// Compute a SHA-512 hash as lowercase hexadecimal.
///
/// The download layer itself only needs SHA-1 and SHA-256 today; this helper
/// is used by integrity tests and is available to the upcoming content-install
/// migration.
#[allow(dead_code)]
pub fn hash_file_sha512(path: &std::path::Path) -> Result<String> {
    crate::integrity::hash_file_sha512(path)
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
pub fn global_http_client() -> Result<reqwest::Client> {
    http_client()
}

/// Client whose redirects are restricted to a small, module-owned host list.
///
/// Security-sensitive downloads (pinned third-party runtime components and
/// Ely.by API discovery) must not inherit the launcher-wide allowlist: a
/// redirect from the expected host to any other allowlisted domain would be
/// accepted by the global policy. The caller still verifies the final URL and
/// payload (SHA-256 / JSON schema).
pub(crate) fn http_client_for_hosts(hosts: &[&str]) -> Result<reqwest::Client> {
    let proxy = proxy_client_url()?;
    let allowed: Vec<String> = hosts.iter().map(|h| h.to_ascii_lowercase()).collect();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS
            || validate_download_url_for_hosts(attempt.url().as_str(), &allowed).is_err()
        {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    Ok(base_client_builder(proxy.as_deref())?
        .redirect(policy)
        .build()?)
}

type ExactHostClient = Mutex<Option<(String, Option<String>, reqwest::Client)>>;
static EXACT_HOST_CLIENT: OnceLock<ExactHostClient> = OnceLock::new();

/// Client for authenticated API calls whose credentials must never cross a
/// host boundary. Redirects are limited to the exact HTTPS host; proxy
/// settings are shared with the global client and rebuilt on change.
pub(crate) fn http_client_for_exact_host(host: &str) -> Result<reqwest::Client> {
    let host = host.to_ascii_lowercase();
    let proxy = proxy_client_url()?;
    let mut slot = EXACT_HOST_CLIENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((cached_host, cached_proxy, client)) = slot.as_ref() {
        if cached_host == &host && cached_proxy == &proxy {
            return Ok(client.clone());
        }
    }
    let client = base_client_builder(proxy.as_deref())?
        .redirect(same_host_redirect_policy(&host))
        .build()?;
    *slot = Some((host, proxy, client.clone()));
    Ok(client)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn lock_keys_ignore_dot_segments_and_windows_case() {
        let dotted = download_lock_key(Path::new("dir/sub/.././file.jar"));
        let plain = download_lock_key(Path::new("dir/file.jar"));
        assert_eq!(dotted, plain);

        #[cfg(windows)]
        {
            let spaced =
                download_lock_key(Path::new(r"C:\Games\Pack\libraries\..\libraries\A.jar"));
            let slashed = download_lock_key(Path::new(r"c:/games/pack/LIBRARIES/a.jar"));
            assert_eq!(spaced, slashed);
            assert_eq!(
                download_lock_key(Path::new(r"\\?\C:\Games\Pack\a.jar")),
                download_lock_key(Path::new(r"c:\games\pack\A.jar"))
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn lock_keys_resolve_directory_aliases() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_lock_alias_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.join("link");
        if std::os::windows::fs::symlink_dir(&real, &link).is_err() {
            eprintln!("skipping directory-alias test: privilege unavailable");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(
            download_lock_key(&real.join("shared.jar")),
            download_lock_key(&link.join("shared.jar"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_same_destination_is_serialized_but_different_paths_are_not() {
        let a = temp_file("lock-a.part", b"");
        let b = temp_file("lock-b.part", b"");
        let lock_a = download_path_lock(&a);
        let lock_a_again = download_path_lock(&a);
        assert!(Arc::ptr_eq(&lock_a, &lock_a_again));
        assert!(!Arc::ptr_eq(&lock_a, &download_path_lock(&b)));

        let first = lock_a.clone().lock_owned().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(40), lock_a_again.clone().lock_owned(),)
                .await
                .is_err(),
            "a second writer for the same path must wait"
        );
        drop(first);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), lock_a_again.lock_owned())
                .await
                .is_ok(),
            "the lock must be released after the first writer"
        );
    }

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
        assert!(is_host_allowed("login.live.com"));
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

    #[test]
    fn download_url_policy_accepts_allowlisted_https_urls() {
        assert!(
            validate_download_url("https://cdn.modrinth.com/data/abc/versions/1/file.jar").is_ok()
        );
        assert!(validate_download_url("https://media.forgecdn.net/files/1/2/mod.jar").is_ok());
        assert!(
            validate_download_url("https://github.com/owner/repo/releases/download/v1/a.zip")
                .is_ok()
        );
        assert!(
            validate_download_url("https://cdn.modrinth.com:443/data/abc/versions/1/file.jar")
                .is_ok(),
            "the explicit default HTTPS port is equivalent to omitting it"
        );
    }

    #[test]
    fn download_url_policy_rejects_scheme_credentials_and_host_spoofing() {
        let rejected = [
            "http://cdn.modrinth.com/data/file.jar",
            "ftp://cdn.modrinth.com/data/file.jar",
            "https://user:pass@cdn.modrinth.com/data/file.jar",
            "https://user@cdn.modrinth.com/data/file.jar",
            // The real host is evil.example; the allowlisted name is userinfo.
            "https://cdn.modrinth.com:443@evil.example/payload",
            "https://cdn.modrinth.com@evil.example/payload",
            "https://evil.example/payload?next=https://cdn.modrinth.com/",
            "https://evil.example/cdn.modrinth.com/payload",
            "https://cdn.modrinth.com.evil.example/payload",
            "https://notgithub.com/owner/repo",
            "https://github.com.attacker.example/owner/repo",
            "https://cdn.modrinth.com./data/file.jar",
            "https://cdn.modrinth.com:8443/data/file.jar",
            "https://127.0.0.1/data/file.jar",
            "https://[::1]/data/file.jar",
            "https://cdn.modrinth.com\\@evil.example/payload",
            "https://cdn.modrinth.com/data/file.jar\nHost: evil.example",
            "",
            "not a url",
            "https://",
        ];
        for url in rejected {
            assert!(
                validate_download_url(url).is_err(),
                "URL must be rejected before any request: {url:?}"
            );
        }
    }

    #[test]
    fn redirect_targets_are_revalidated_with_the_same_policy() {
        let allowed = reqwest::Url::parse("https://github.com/owner/repo/releases/download/v1/a")
            .expect("valid test URL");
        let another_allowed =
            reqwest::Url::parse("https://cdn.modrinth.com/data/a/versions/1/file.jar")
                .expect("valid test URL");
        let denied = reqwest::Url::parse("https://evil.example/payload").expect("valid test URL");
        let spoofed = reqwest::Url::parse("https://cdn.modrinth.com:443@evil.example/payload")
            .expect("the URL parser accepts userinfo");

        assert!(redirect_target_allowed(&allowed, 0));
        assert!(
            redirect_target_allowed(&another_allowed, 0),
            "a redirect to another allowlisted HTTPS host is allowed"
        );
        assert!(
            !redirect_target_allowed(&denied, 0),
            "a redirect from an allowlisted host to an arbitrary host must stop"
        );
        assert!(
            !redirect_target_allowed(&spoofed, 0),
            "redirect targets use the parsed host, not a string prefix"
        );
        assert!(
            !redirect_target_allowed(&allowed, MAX_REDIRECTS),
            "redirect loops/chains must stop at the cap"
        );
    }

    #[test]
    fn authenticated_redirects_stay_on_the_exact_https_host() {
        let url = |raw: &str| reqwest::Url::parse(raw).expect("valid test URL");
        let api = url("https://api.curseforge.com/v1/mods/1");
        assert!(same_host_redirect_allowed(&api, "api.curseforge.com", 0));
        assert!(same_host_redirect_allowed(
            &url("https://api.curseforge.com:443/v1/mods/1"),
            "api.curseforge.com",
            0
        ));
        assert!(
            same_host_redirect_allowed(
                &url("https://API.CurseForge.com/v1/mods/2"),
                "api.curseforge.com",
                0
            ),
            "host comparison is case-insensitive"
        );
        assert!(
            !same_host_redirect_allowed(
                &url("https://cdn.curseforge.com/files/pack.zip"),
                "api.curseforge.com",
                0
            ),
            "even another allowlisted download host is a different trust boundary"
        );
        assert!(!same_host_redirect_allowed(
            &url("https://evil.example/payload"),
            "api.curseforge.com",
            0
        ));
        assert!(
            !same_host_redirect_allowed(
                &url("http://api.curseforge.com/v1/mods/1"),
                "api.curseforge.com",
                0
            ),
            "a scheme downgrade must stop"
        );
        assert!(
            !same_host_redirect_allowed(
                &url("https://api.curseforge.com:8443/v1/mods/1"),
                "api.curseforge.com",
                0
            ),
            "a non-default port is not the API origin"
        );
        assert!(
            !same_host_redirect_allowed(
                &url("https://api.curseforge.com@evil.example/payload"),
                "api.curseforge.com",
                0
            ),
            "the parsed host is compared, not userinfo"
        );
        assert!(!same_host_redirect_allowed(
            &api,
            "api.curseforge.com",
            MAX_REDIRECTS
        ));
    }

    #[test]
    fn module_redirect_policy_keeps_the_host_boundary() {
        let hosts = vec!["maven.neoforged.net".to_string()];
        assert!(validate_download_url_for_hosts(
            "https://maven.neoforged.net/releases/a.jar",
            &hosts
        )
        .is_ok());
        assert!(validate_download_url_for_hosts(
            "https://maven.neoforged.net:443/releases/a.jar",
            &hosts
        )
        .is_ok());
        for url in [
            "https://github.com/owner/repo/a.jar",
            "https://maven.neoforged.net.attacker.example/a.jar",
            "https://notmaven.neoforged.net/a.jar",
            "https://maven.neoforged.net:8443/a.jar",
            "https://maven.neoforged.net:443@evil.example/a.jar",
        ] {
            assert!(
                validate_download_url_for_hosts(url, &hosts).is_err(),
                "module host policy must reject {url:?}"
            );
        }
    }

    #[tokio::test]
    async fn spoofed_url_is_rejected_before_touching_disk_or_network() {
        let path = std::env::temp_dir().join(format!(
            "voidlauncher_bad_url_{}_{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&path);
        let err = download_to_part_observed(
            "https://cdn.modrinth.com:443@evil.example/payload",
            &path,
            "",
            None,
            None,
            None,
        )
        .await
        .expect_err("the spoofed URL must be rejected");
        let text = err.to_string();
        assert!(
            text.contains("not in the allowlist") || text.contains("username or password"),
            "unexpected error: {err}"
        );
        assert!(!path.exists(), "no destination may be created");
        assert!(!part_path_for(&path).exists(), "no .part may be created");
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
    fn validate_download_with_hash_accepts_sha256_and_rejects_mismatch() {
        let p = temp_file("val_sha256.bin", b"abc");
        let good =
            HashSpec::sha256("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
                .unwrap();
        assert!(validate_download_with_hash(&p, Some(&good)).is_ok());
        let bad = HashSpec::sha256(&"0".repeat(64)).unwrap();
        let err = validate_download_with_hash(&p, Some(&bad)).unwrap_err();
        assert!(err.to_string().contains("SHA256 mismatch"), "got: {err}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn download_identity_with_hash_keeps_sha1_compatible_and_separates_algorithms() {
        let sha1 = HashSpec::sha1(&"a".repeat(40)).unwrap();
        let sha256 = HashSpec::sha256(&"b".repeat(64)).unwrap();
        let url = "https://cdn.modrinth.com/file.jar";
        assert_eq!(
            download_identity_with_hash(url, Some(&sha1), Some(10)),
            format!("{url}\n{}\n10", "a".repeat(40))
        );
        assert_eq!(
            download_identity_with_hash(url, Some(&sha256), Some(10)),
            format!("{url}\nsha256:{}\n10", "b".repeat(64))
        );
        assert_eq!(
            download_identity_with_hash(url, None, Some(10)),
            format!("{url}\n\n10")
        );
    }

    #[tokio::test]
    async fn verified_download_accepts_sha256_and_sha512() {
        let _proxy = direct_proxy_lock().await;
        let dir = mock_dir("verified_algorithms");
        let body = b"hello";

        let server = spawn_mock(vec![MockSpec::ok(body)]).await;
        let sha256_dest = dir.join("sha256.bin");
        let sha256 = HashSpec::sha256(&sha256_hex(body)).unwrap();
        download_file_verified(&server.url, &sha256_dest, &sha256)
            .await
            .expect("SHA-256 verified download");
        assert_eq!(std::fs::read(&sha256_dest).unwrap(), body);
        assert_clean(&part_path_for(&sha256_dest), &part_meta_path(&sha256_dest));

        let server = spawn_mock(vec![MockSpec::ok(body)]).await;
        let sha512_dest = dir.join("sha512.bin");
        let sha512 = HashSpec::sha512(&sha512_hex(body)).unwrap();
        download_file_verified(&server.url, &sha512_dest, &sha512)
            .await
            .expect("SHA-512 verified download");
        assert_eq!(std::fs::read(&sha512_dest).unwrap(), body);
        assert_clean(&part_path_for(&sha512_dest), &part_meta_path(&sha512_dest));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn verified_download_uses_a_valid_cache_without_network() {
        let _proxy = direct_proxy_lock().await;
        let dir = mock_dir("verified_cache");
        let dest = dir.join("cached.bin");
        std::fs::write(&dest, b"hello").unwrap();
        let server = spawn_mock(vec![MockSpec::status(500)]).await;

        let expected = HashSpec::sha256(&sha256_hex(b"hello")).unwrap();
        download_file_verified(&server.url, &dest, &expected)
            .await
            .expect("cached file must be accepted");
        assert_eq!(server.hits.load(Ordering::SeqCst), 0);
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn verified_download_mismatch_never_publishes_or_replaces_target() {
        let _proxy = direct_proxy_lock().await;
        let body = b"new-content";
        let wrong = HashSpec::sha256(&"0".repeat(64)).unwrap();

        for (tag, existing) in [
            (
                "verified_mismatch_existing",
                Some(b"old-content".as_slice()),
            ),
            ("verified_mismatch_absent", None),
        ] {
            let dir = mock_dir(tag);
            let dest = dir.join("artifact.bin");
            if let Some(existing) = existing {
                std::fs::write(&dest, existing).unwrap();
            }
            let server = spawn_mock(vec![MockSpec::ok(body)]).await;

            let err = download_to_part_observed_with_hash(
                &server.url,
                &dest,
                Some(&wrong),
                None,
                Some(Duration::from_secs(10)),
                None,
            )
            .await
            .expect_err("wrong SHA-256 must fail");

            assert!(err.to_string().contains("SHA256 mismatch"), "got: {err}");
            match existing {
                Some(existing) => assert_eq!(std::fs::read(&dest).unwrap(), existing),
                None => assert!(!dest.exists(), "a failed download must not publish"),
            }
            assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
            assert_eq!(server.hits.load(Ordering::SeqCst), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn public_verified_download_exhausts_retries_without_publishing() {
        let _proxy = direct_proxy_lock().await;
        let dir = mock_dir("verified_retry_exhaustion");
        let dest = dir.join("artifact.bin");
        std::fs::write(&dest, b"old-content").unwrap();
        let body = b"new-content";
        let wrong = HashSpec::sha256(&"0".repeat(64)).unwrap();
        let server = spawn_mock(vec![MockSpec::ok(body); MAX_RETRIES as usize]).await;

        let err = download_file_verified(&server.url, &dest, &wrong)
            .await
            .expect_err("wrong SHA-256 must fail after retries");

        assert!(err.to_string().contains("SHA256 mismatch"), "got: {err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"old-content");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        assert_eq!(
            server.hits.load(Ordering::SeqCst),
            MAX_RETRIES as usize,
            "every retry must be attempted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn verified_download_replaces_a_corrupted_cache_only_after_verification() {
        let _proxy = direct_proxy_lock().await;
        let dir = mock_dir("verified_corrupt_cache");
        let dest = dir.join("cached.bin");
        std::fs::write(&dest, b"corrupted").unwrap();
        let body = b"hello";
        let expected = HashSpec::sha256(&sha256_hex(body)).unwrap();
        let server = spawn_mock(vec![MockSpec::ok(body)]).await;

        download_file_verified(&server.url, &dest, &expected)
            .await
            .expect("corrupted cache must be replaced");
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn legacy_download_rejects_malformed_sha1_before_network() {
        let _proxy = direct_proxy_lock().await;
        let dir = mock_dir("malformed_sha1");
        let dest = dir.join("artifact.bin");
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;

        let err = download_file(&server.url, &dest, "not-a-hash")
            .await
            .expect_err("malformed SHA-1 must fail");
        assert!(err.to_string().contains("Invalid SHA1 hash"), "got: {err}");
        assert_eq!(server.hits.load(Ordering::SeqCst), 0);
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloudfront_block_only_matches_the_waf_page() {
        assert!(is_cloudfront_block(
            false,
            "<!DOCTYPE HTML><html><title>ERROR: The request could not be satisfied</title></html>"
        ));
        assert!(is_cloudfront_block(
            true,
            "<!DOCTYPE HTML><html><body>Request blocked</body></html>"
        ));
        assert!(!is_cloudfront_block(
            false,
            "{\"error\":\"Forbidden\",\"errorCode\":403}"
        ));
        assert!(!is_cloudfront_block(
            false,
            "<!DOCTYPE HTML><html><body>Not found</body></html>"
        ));
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
    fn asset_hash_validation_rejects_path_like_values() {
        let good = "0123456789abcdef0123456789abcdef01234567";
        assert!(is_valid_asset_hash(good));
        assert!(!is_valid_asset_hash(
            "../0123456789abcdef0123456789abcdef0123"
        ));
        assert!(!is_valid_asset_hash(
            "0123456789abcdef0123/456789abcdef01234567"
        ));
        assert!(!is_valid_asset_hash("0123"));
        assert!(!is_valid_asset_hash(""));
        assert!(!is_valid_asset_hash(
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
        ));
    }

    #[test]
    fn partial_download_identity_discards_stale_part() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_part_identity_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("library.jar");
        let part = part_path_for(&target);
        let meta = part_meta_path(&target);

        let identity = download_identity("https://example.com/a.jar", "abc", Some(10));
        std::fs::write(&part, b"old bytes").unwrap();
        std::fs::write(&meta, &identity).unwrap();
        assert_eq!(
            prepare_partial_download(&target, &identity),
            9,
            "matching identity must preserve the resume offset"
        );

        // A different URL/artifact must not resume from these bytes.
        let other = download_identity("https://example.com/b.jar", "def", Some(20));
        assert_eq!(prepare_partial_download(&target, &other), 0);
        assert!(!part.exists(), "stale partial must be deleted");

        // Missing sidecar is also untrusted.
        std::fs::write(&part, b"old bytes").unwrap();
        let _ = std::fs::remove_file(&meta);
        assert_eq!(prepare_partial_download(&target, &identity), 0);
        assert!(!part.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_linked_partial_file_is_refused_before_opening() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_partial_link_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("artifact.jar");
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, b"keep").unwrap();
        let link = part_path_for(&target);
        if std::os::windows::fs::symlink_file(&outside, &link).is_err() {
            eprintln!("skipping .part symlink test: privilege unavailable");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            reject_linked_partial(&target).is_err(),
            "a .part symlink must not be followed"
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        std::fs::remove_file(&link).unwrap();
        assert!(reject_linked_partial(&target).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transient_download_statuses_are_classified() {
        assert!(is_transient_download_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(is_transient_download_status(
            reqwest::StatusCode::REQUEST_TIMEOUT
        ));
        assert!(is_transient_download_status(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(is_transient_download_status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
        assert!(!is_transient_download_status(
            reqwest::StatusCode::NOT_FOUND
        ));
        assert!(!is_transient_download_status(
            reqwest::StatusCode::FORBIDDEN
        ));
        assert!(!is_transient_download_status(
            reqwest::StatusCode::BAD_REQUEST
        ));
    }

    #[test]
    fn terminal_failure_cleanup_removes_partial_and_keeps_destination() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_terminal_cleanup_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("artifact.jar");
        let part = part_path_for(&target);
        let meta = part_meta_path(&target);
        std::fs::write(&target, b"previous-good-file").unwrap();
        std::fs::write(&part, b"partial").unwrap();
        std::fs::write(&meta, b"identity").unwrap();

        let err = finish_failed_download(
            terminal_download_error(
                "HTTP 302 for https://example.com/a.jar (redirect target is not allowed; response refused)"
                    .to_string(),
            ),
            &part,
            &meta,
        );

        let message = err.to_string();
        assert!(
            message.contains("HTTP 302"),
            "original error is preserved: {message}"
        );
        assert!(message.contains("redirect target is not allowed"));
        assert!(
            !part.exists(),
            "a refused redirect must not keep resumable bytes"
        );
        assert!(
            !meta.exists(),
            "the identity sidecar must be removed with it"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"previous-good-file",
            "the destination must never be touched by a failed download"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retry_after_is_parsed_and_capped() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after_seconds(&headers, 1), 7);

        headers.insert(reqwest::header::RETRY_AFTER, "600".parse().unwrap());
        assert_eq!(retry_after_seconds(&headers, 1), 30);

        headers.insert(reqwest::header::RETRY_AFTER, "soon".parse().unwrap());
        assert_eq!(retry_after_seconds(&headers, 3), 3);
    }

    #[test]
    fn download_targets_are_deduped_case_insensitively() {
        let files = vec![
            (
                "https://example.com/a".to_string(),
                PathBuf::from("C:/Libraries/A.jar"),
                "one".to_string(),
                1,
            ),
            (
                "https://example.com/a-dup".to_string(),
                PathBuf::from("c:/libraries/a.jar"),
                "two".to_string(),
                2,
            ),
            (
                "https://example.com/b".to_string(),
                PathBuf::from("C:/Libraries/B.jar"),
                "three".to_string(),
                3,
            ),
        ];
        let deduped = dedupe_download_targets(files);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].0, "https://example.com/a");
        assert_eq!(deduped[1].0, "https://example.com/b");
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

    #[test]
    fn maven_fallback_without_hash_or_size_requires_integrity_manifest() {
        let path = temp_file("maven_no_meta.jar", b"artifact-bytes");
        let url = "https://maven.example.invalid/releases/a.jar";
        let manifest_path = integrity_manifest_path(&path).unwrap();

        // No publisher checksum, no size, no local manifest: not a cache hit.
        assert_eq!(
            cached_download_size(&path, "", 0, DownloadCachePolicy::VerifySha1, url),
            None
        );

        write_integrity_manifest(&path, url).unwrap();
        assert_eq!(
            cached_download_size(&path, "", 0, DownloadCachePolicy::VerifySha1, url),
            Some(14)
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(manifest_path);
    }

    #[test]
    fn maven_fallback_rejects_a_changed_source_url() {
        let path = temp_file("maven_url_change.jar", b"artifact-bytes");
        let manifest_path = integrity_manifest_path(&path).unwrap();
        write_integrity_manifest(&path, "https://maven.example.invalid/releases/a-1.0.jar")
            .unwrap();

        assert_eq!(
            cached_download_size(
                &path,
                "",
                0,
                DownloadCachePolicy::VerifySha1,
                "https://maven.example.invalid/releases/a-2.0.jar"
            ),
            None
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(manifest_path);
    }

    #[test]
    fn maven_fallback_rejects_a_corrupt_manifest_and_keeps_the_file() {
        let path = temp_file("maven_corrupt_manifest.jar", b"artifact-bytes");
        let manifest_path = integrity_manifest_path(&path).unwrap();
        write_integrity_manifest(&path, "https://maven.example.invalid/releases/a.jar").unwrap();
        std::fs::write(&manifest_path, b"{ this is not json").unwrap();

        assert_eq!(
            cached_download_size(
                &path,
                "",
                0,
                DownloadCachePolicy::VerifySha1,
                "https://maven.example.invalid/releases/a.jar"
            ),
            None
        );
        // A failed validation must not delete or modify the old artifact.
        assert_eq!(std::fs::read(&path).unwrap(), b"artifact-bytes");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(manifest_path);
    }

    #[test]
    fn maven_fallback_detects_local_corruption_and_keeps_the_file() {
        let path = temp_file("maven_tampered.jar", b"artifact-bytes");
        let manifest_path = integrity_manifest_path(&path).unwrap();
        let url = "https://maven.example.invalid/releases/a.jar";
        write_integrity_manifest(&path, url).unwrap();

        std::fs::write(&path, vec![b'x'; 14]).unwrap();

        assert_eq!(
            cached_download_size(&path, "", 0, DownloadCachePolicy::VerifySha1, url),
            None
        );
        assert!(path.is_file());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(manifest_path);
    }

    #[test]
    fn maven_fallback_without_hash_still_accepts_a_matching_declared_size() {
        // Size-bearing artifacts keep the historical size-based proof; the
        // manifest is only the fallback when neither hash nor size exists.
        let path = temp_file("maven_size_only.jar", b"artifact-bytes");
        assert_eq!(
            cached_download_size(
                &path,
                "",
                14,
                DownloadCachePolicy::VerifySha1,
                "https://maven.example.invalid/releases/a.jar"
            ),
            Some(14)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cached_download_requires_a_valid_hash_or_matching_size() {
        let path = temp_file("cache_valid.bin", b"abc");
        let sha = hash_file_sha1(&path).unwrap();

        assert_eq!(
            cached_download_size(
                &path,
                &sha,
                3,
                DownloadCachePolicy::VerifySha1,
                "https://example.invalid/a.bin"
            ),
            Some(3)
        );
        assert_eq!(
            cached_download_size(
                &path,
                "0000000000000000000000000000000000000000",
                3,
                DownloadCachePolicy::VerifySha1,
                "https://example.invalid/a.bin"
            ),
            None
        );
        assert_eq!(
            cached_download_size(
                &path,
                "",
                3,
                DownloadCachePolicy::VerifySha1,
                "https://example.invalid/a.bin"
            ),
            Some(3)
        );
        assert_eq!(
            cached_download_size(
                &path,
                "",
                4,
                DownloadCachePolicy::VerifySha1,
                "https://example.invalid/a.bin"
            ),
            None
        );
        assert_eq!(
            cached_download_size(
                &path,
                "",
                3,
                DownloadCachePolicy::SizeOnly,
                "https://example.invalid/a.bin"
            ),
            Some(3)
        );
        assert_eq!(
            cached_download_size(
                &path,
                "",
                4,
                DownloadCachePolicy::SizeOnly,
                "https://example.invalid/a.bin"
            ),
            None
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn download_files_observed_counts_valid_cached_files_without_network() {
        let path = temp_file("cached_plan.bin", b"hello");
        let sha = hash_file_sha1(&path).unwrap();
        let progress = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(Mutex::new(Vec::new()));
        let progress_sink = progress.clone();
        let done_sink = done.clone();

        download_files_observed(
            vec![(
                "https://example.invalid/cached.bin".to_string(),
                path.clone(),
                sha,
                5,
            )],
            DownloadCachePolicy::VerifySha1,
            |_completed, _total, _bytes, _total_bytes, _message| {},
            move |file, downloaded, total, network| {
                progress_sink.lock().unwrap().push((
                    file.to_path_buf(),
                    downloaded,
                    total,
                    network,
                ));
            },
            move |file, size| {
                done_sink.lock().unwrap().push((file.to_path_buf(), size));
            },
        )
        .await
        .unwrap();

        assert_eq!(
            progress.lock().unwrap().as_slice(),
            &[(path.clone(), 5, Some(5), 0)]
        );
        assert_eq!(done.lock().unwrap().as_slice(), &[(path.clone(), 5)]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn asset_plan_includes_cached_objects_and_total_size() {
        use crate::versions::{AssetIndexData, AssetObject};

        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_asset_plan_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cached_hash = "a".repeat(40);
        let missing_hash = "b".repeat(40);
        let cached_path = dir
            .join("objects")
            .join(&cached_hash[..2])
            .join(&cached_hash);
        std::fs::create_dir_all(cached_path.parent().unwrap()).unwrap();
        std::fs::write(&cached_path, b"abc").unwrap();

        let index = AssetIndexData {
            objects: std::collections::HashMap::from([
                (
                    "cached".to_string(),
                    AssetObject {
                        hash: cached_hash.clone(),
                        size: 3,
                        is_virtual: false,
                    },
                ),
                (
                    "missing".to_string(),
                    AssetObject {
                        hash: missing_hash.clone(),
                        size: 7,
                        is_virtual: false,
                    },
                ),
            ]),
            map_to_resources: false,
        };
        let plan = collect_asset_download_plan(&index, &dir).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.iter().map(|(_, _, _, size)| *size).sum::<u64>(), 10);
        assert!(plan.iter().any(|(_, path, _, _)| path == &cached_path));
        assert!(plan
            .iter()
            .any(|(_, path, _, _)| path.ends_with(&missing_hash)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_forced_corrupt_asset_is_planned_without_deleting_the_old_copy() {
        use crate::versions::{AssetIndexData, AssetObject};

        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_asset_forced_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let hash = "a".repeat(40);
        let path = dir.join("objects").join(&hash[..2]).join(&hash);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Wrong content with the expected size: the cheap size-only prefilter
        // alone cannot see the corruption.
        std::fs::write(&path, b"bad").unwrap();

        let index = AssetIndexData {
            objects: std::collections::HashMap::from([(
                "corrupt".to_string(),
                AssetObject {
                    hash: hash.clone(),
                    size: 3,
                    is_virtual: false,
                },
            )]),
            map_to_resources: false,
        };

        assert!(
            collect_asset_download_plan_forced(&index, &dir, &std::collections::HashSet::new())
                .unwrap()
                .is_empty(),
            "size-only planning skips the corrupt object"
        );
        let forced = std::collections::HashSet::from([path.clone()]);
        let plan = collect_asset_download_plan_forced(&index, &dir, &forced).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].1, path);
        assert!(
            path.exists(),
            "planning must not delete the old copy before a verified replacement"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn virtual_asset_replacement_leaves_no_temporary_file() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_virtual_asset_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src = dir.join("objects").join("aa").join("hash");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"abc").unwrap();
        let dest_dir = dir.join("virtual").join("legacy").join("sounds");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let dest = dest_dir.join("sound.ogg");
        std::fs::write(&dest, b"bad").unwrap();

        replace_virtual_asset(&src, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        let leftovers: Vec<_> = std::fs::read_dir(&dest_dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .map(|entry| entry.path())
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_virtual_asset_symlink_is_replaced_not_followed() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_virtual_link_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("source.bin");
        std::fs::write(&src, b"new").unwrap();
        let outside = dir.join("outside.bin");
        std::fs::write(&outside, b"keep").unwrap();
        let dest = dir.join("virtual").join("sound.ogg");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        if std::os::windows::fs::symlink_file(&outside, &dest).is_err() {
            eprintln!("skipping virtual-asset symlink test: privilege unavailable");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        replace_virtual_asset(&src, &dest).unwrap();
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"keep",
            "the symlink target must stay untouched"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_cached_file_is_not_accepted_as_complete() {
        let path = temp_file("cache_corrupt.bin", b"corrupt");
        let expected_sha = "a9993e364706816aba3e25717850c26c9cd0d89d"; // SHA1("abc")
        assert_eq!(
            cached_download_size(
                &path,
                expected_sha,
                8,
                DownloadCachePolicy::VerifySha1,
                "https://example.invalid/a.bin"
            ),
            None
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn progress_total_uses_trustworthy_size_sources_only() {
        use reqwest::StatusCode;
        assert_eq!(
            download_progress_total(StatusCode::OK, Some(100), Some(200), Some(300)),
            Some(100)
        );
        assert_eq!(
            download_progress_total(StatusCode::PARTIAL_CONTENT, None, Some(200), Some(50)),
            Some(200)
        );
        assert_eq!(
            download_progress_total(StatusCode::OK, None, None, Some(300)),
            Some(300)
        );
        assert_eq!(
            download_progress_total(StatusCode::OK, None, None, None),
            None,
            "chunked responses have no trustworthy total"
        );
        assert_eq!(
            download_progress_total(StatusCode::PARTIAL_CONTENT, None, None, Some(50)),
            None,
            "a 206 Content-Length is only the remaining range"
        );
    }

    // ==================== Proxy-only network policy ====================
    //
    // Real local sockets: a tiny HTTP proxy and a separate "direct" origin.
    // The working proxy answers the request itself, so the origin's hit
    // counter only moves when a request actually bypasses the proxy. This is
    // what proves the strict-mode "no direct fallback" guarantee.

    static PROXY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn apply_test_proxy(enabled: bool, use_system: bool, proxy: Option<String>, proxy_only: bool) {
        // Ignore the machine's real Windows proxy so the test controls the
        // only proxy source that can be used.
        set_system_proxy_override(None);
        set_global_proxy_policy(enabled, use_system, proxy, proxy_only);
    }

    /// Select the (mocked) Windows system proxy while keeping a manual proxy
    /// configured to prove it is not used as an accidental fallback.
    fn apply_test_system_proxy(
        enabled: bool,
        system: Option<String>,
        manual: Option<String>,
        proxy_only: bool,
    ) {
        set_system_proxy_override(system);
        set_global_proxy_policy(enabled, true, manual, proxy_only);
    }

    /// Restores the default policy even if an assertion panics.
    struct ProxyTestReset;

    impl Drop for ProxyTestReset {
        fn drop(&mut self) {
            set_global_proxy_policy(false, false, None, true);
            clear_system_proxy_override();
        }
    }

    struct TestOrigin {
        url: String,
        hits: Arc<AtomicUsize>,
    }

    async fn spawn_origin() -> TestOrigin {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let task_hits = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let hits = task_hits.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    if socket.read(&mut buf).await.unwrap_or(0) == 0 {
                        return;
                    }
                    hits.fetch_add(1, Ordering::SeqCst);
                    let body = b"origin-ok";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        TestOrigin {
            url: format!("http://{}/test", addr),
            hits,
        }
    }

    struct TestProxy {
        /// `host:port` exactly as `AppConfig::proxy_url()` returns it.
        addr: String,
        hits: Arc<AtomicUsize>,
    }

    async fn spawn_proxy(working: bool) -> TestProxy {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let task_hits = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let hits = task_hits.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    hits.fetch_add(1, Ordering::SeqCst);
                    if !working {
                        // Accept then close: a network-level proxy failure.
                        let _ = client.shutdown().await;
                        return;
                    }
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let n = match client.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        request.extend_from_slice(&chunk[..n]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") || request.len() > 65536 {
                            break;
                        }
                    }
                    let body = b"proxy-ok";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = client.write_all(head.as_bytes()).await;
                    let _ = client.write_all(body).await;
                    let _ = client.shutdown().await;
                });
            }
        });
        TestProxy {
            addr: format!("127.0.0.1:{}", port),
            hits,
        }
    }

    /// A proxy that answers every request with a redirect to `location`.
    async fn spawn_redirect_proxy(location: String) -> TestProxy {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let task_hits = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let hits = task_hits.clone();
                let location = location.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    hits.fetch_add(1, Ordering::SeqCst);
                    let mut buf = [0u8; 1024];
                    let _ = client.read(&mut buf).await;
                    let head = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        location
                    );
                    let _ = client.write_all(head.as_bytes()).await;
                    let _ = client.shutdown().await;
                });
            }
        });
        TestProxy {
            addr: format!("127.0.0.1:{}", port),
            hits,
        }
    }

    async fn send_test_request(url: &str) -> std::result::Result<String, String> {
        let client = global_http_client().map_err(|e| e.to_string())?;
        let response = send_with_fallback(client.get(url))
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        let body = response.text().await.map_err(|e| e.to_string())?;
        Ok(format!("{} {}", status, body))
    }

    async fn timed_request(url: &str) -> std::result::Result<String, String> {
        tokio::time::timeout(Duration::from_secs(10), send_test_request(url))
            .await
            .expect("proxy test request timed out")
    }

    #[tokio::test]
    async fn proxy_probe_does_not_follow_redirects() {
        let origin = spawn_origin().await;
        let proxy = spawn_redirect_proxy(origin.url.clone()).await;
        let reachable = probe_proxy(&format!("http://{}", proxy.addr), &origin.url).await;
        assert!(
            reachable,
            "a redirect response still proves that the proxy is reachable"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            proxy.hits.load(Ordering::SeqCst),
            1,
            "the probe must not follow the redirect back through the proxy"
        );
        assert_eq!(
            origin.hits.load(Ordering::SeqCst),
            0,
            "the probe target must never be contacted directly"
        );
    }

    #[tokio::test]
    async fn proxy_probe_reports_a_dead_proxy() {
        let proxy = spawn_proxy(false).await;
        assert!(
            !probe_proxy(
                &format!("http://{}", proxy.addr),
                "https://api.modrinth.com/"
            )
            .await,
            "a proxy that closes the connection must probe as unreachable"
        );
    }

    #[tokio::test]
    async fn proxy_only_mode_uses_the_working_proxy() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let proxy = spawn_proxy(true).await;
        apply_test_proxy(true, false, Some(proxy.addr.clone()), true);

        let result = timed_request(&origin.url).await.expect("proxied request");

        assert!(result.contains("proxy-ok"), "unexpected response: {result}");
        assert_eq!(proxy.hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            origin.hits.load(Ordering::SeqCst),
            0,
            "strict mode must not touch the direct origin"
        );
    }

    #[tokio::test]
    async fn proxy_only_mode_never_falls_back_to_direct() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let proxy = spawn_proxy(false).await;
        apply_test_proxy(true, false, Some(proxy.addr.clone()), true);

        let result = timed_request(&origin.url).await;

        assert!(result.is_err(), "a failed proxy must fail the request");
        assert!(proxy.hits.load(Ordering::SeqCst) >= 1);
        assert_eq!(
            origin.hits.load(Ordering::SeqCst),
            0,
            "direct fallback is forbidden in proxy-only mode"
        );
    }

    #[tokio::test]
    async fn fallback_mode_uses_the_working_proxy_first() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let proxy = spawn_proxy(true).await;
        apply_test_proxy(true, false, Some(proxy.addr.clone()), false);

        let result = timed_request(&origin.url).await.expect("proxied request");

        assert!(result.contains("proxy-ok"), "unexpected response: {result}");
        assert_eq!(proxy.hits.load(Ordering::SeqCst), 1);
        assert_eq!(origin.hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fallback_mode_retries_directly_after_proxy_failure() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let proxy = spawn_proxy(false).await;
        apply_test_proxy(true, false, Some(proxy.addr.clone()), false);

        let result = timed_request(&origin.url).await.expect("direct fallback");

        assert!(
            result.contains("origin-ok"),
            "unexpected response: {result}"
        );
        assert!(proxy.hits.load(Ordering::SeqCst) >= 1);
        assert_eq!(
            origin.hits.load(Ordering::SeqCst),
            1,
            "fallback mode should retry directly"
        );
    }

    #[tokio::test]
    async fn proxy_only_mode_without_proxy_refuses_direct() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        apply_test_proxy(true, false, None, true);

        let result = timed_request(&origin.url).await;

        assert!(result.is_err(), "no proxy + strict mode must fail");
        assert_eq!(
            origin.hits.load(Ordering::SeqCst),
            0,
            "strict mode must not connect directly when no proxy is set"
        );
        let policy = crate::commands::misc::cmd_updater_proxy_policy().await;
        assert!(policy.enabled);
        assert!(policy.proxy_only);
        assert!(
            policy.proxy_url.is_none(),
            "the updater must see no proxy and skip the network attempt"
        );
    }

    #[tokio::test]
    async fn master_proxy_switch_off_forces_direct_even_if_proxy_only_saved_on() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let proxy = spawn_proxy(true).await;

        // The parent switch is off, but the child value was saved as on.
        apply_test_proxy(false, false, Some(proxy.addr.clone()), true);

        let result = timed_request(&origin.url).await.expect("direct request");

        assert!(
            result.contains("origin-ok"),
            "unexpected response: {result}"
        );
        assert_eq!(
            proxy.hits.load(Ordering::SeqCst),
            0,
            "the child flag must not force traffic through a disabled proxy"
        );
        assert_eq!(origin.hits.load(Ordering::SeqCst), 1);

        let policy = crate::commands::misc::cmd_updater_proxy_policy().await;
        assert!(!policy.enabled);
        assert!(!policy.proxy_only);
        assert!(policy.proxy_url.is_none());
    }

    #[tokio::test]
    async fn system_proxy_is_used_instead_of_the_saved_manual_proxy() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let manual = spawn_proxy(true).await;
        let system = spawn_proxy(true).await;
        apply_test_system_proxy(
            true,
            Some(system.addr.clone()),
            Some(manual.addr.clone()),
            false,
        );

        let result = timed_request(&origin.url)
            .await
            .expect("system proxy request");
        assert!(result.contains("proxy-ok"), "unexpected response: {result}");
        assert_eq!(system.hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            manual.hits.load(Ordering::SeqCst),
            0,
            "the manual proxy must not be used while the system source is selected"
        );
        assert_eq!(origin.hits.load(Ordering::SeqCst), 0);

        // The request itself proves the source; the policy command probes the
        // proxy scheme over HTTPS, which the plain mock proxy cannot tunnel.
    }

    #[tokio::test]
    async fn missing_system_proxy_in_strict_mode_refuses_direct() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let manual = spawn_proxy(true).await;
        apply_test_system_proxy(true, None, Some(manual.addr.clone()), true);

        let result = timed_request(&origin.url).await;
        assert!(result.is_err(), "no system proxy + strict mode must fail");
        assert_eq!(origin.hits.load(Ordering::SeqCst), 0);
        assert_eq!(manual.hits.load(Ordering::SeqCst), 0);

        let policy = crate::commands::misc::cmd_updater_proxy_policy().await;
        assert!(policy.proxy_only);
        assert!(policy.proxy_url.is_none());
    }

    #[tokio::test]
    async fn missing_system_proxy_in_fallback_mode_uses_direct() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let manual = spawn_proxy(true).await;
        apply_test_system_proxy(true, None, Some(manual.addr.clone()), false);

        let result = timed_request(&origin.url).await.expect("direct fallback");
        assert!(
            result.contains("origin-ok"),
            "unexpected response: {result}"
        );
        assert_eq!(origin.hits.load(Ordering::SeqCst), 1);
        assert_eq!(manual.hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unavailable_system_proxy_strict_mode_refuses_direct() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let manual = spawn_proxy(true).await;
        let broken_system = spawn_proxy(false).await;
        apply_test_system_proxy(
            true,
            Some(broken_system.addr.clone()),
            Some(manual.addr.clone()),
            true,
        );

        let result = timed_request(&origin.url).await;
        assert!(
            result.is_err(),
            "broken system proxy + strict mode must fail"
        );
        assert!(broken_system.hits.load(Ordering::SeqCst) >= 1);
        assert_eq!(origin.hits.load(Ordering::SeqCst), 0);
        assert_eq!(manual.hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unavailable_system_proxy_fallback_mode_retries_direct() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let manual = spawn_proxy(true).await;
        let broken_system = spawn_proxy(false).await;
        apply_test_system_proxy(
            true,
            Some(broken_system.addr.clone()),
            Some(manual.addr.clone()),
            false,
        );

        let result = timed_request(&origin.url).await.expect("direct fallback");
        assert!(
            result.contains("origin-ok"),
            "unexpected response: {result}"
        );
        assert!(broken_system.hits.load(Ordering::SeqCst) >= 1);
        assert_eq!(origin.hits.load(Ordering::SeqCst), 1);
        assert_eq!(manual.hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn updater_policy_follows_the_selected_system_source() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let manual = spawn_proxy(true).await;
        let system = spawn_proxy(true).await;
        apply_test_system_proxy(
            true,
            Some(system.addr.clone()),
            Some(manual.addr.clone()),
            true,
        );
        // Seed the resolved cache so the policy command does not need to
        // perform the HTTPS probe against the plain mock proxy.
        *PROXY_RESOLVED
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some((system.addr.clone(), Some(format!("http://{}", system.addr))));

        let policy = crate::commands::misc::cmd_updater_proxy_policy().await;
        assert!(policy.enabled);
        assert!(policy.proxy_only);
        assert_eq!(policy.proxy_url, Some(format!("http://{}", system.addr)));
        assert_ne!(
            policy.proxy_url,
            Some(format!("http://{}", manual.addr)),
            "the manual proxy must not leak into the system-source policy"
        );
    }

    #[tokio::test]
    async fn proxy_only_setting_survives_save_and_reload() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let _reset = ProxyTestReset;
        let origin = spawn_origin().await;
        let proxy = spawn_proxy(true).await;

        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_proxy_cfg_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = crate::config::AppConfig {
            data_dir: dir.clone(),
            proxy_enabled: true,
            proxy_addr: "127.0.0.1".to_string(),
            proxy_port: proxy.addr.rsplit(':').next().unwrap().parse().unwrap(),
            use_system_proxy: Some(false),
            proxy_only: true,
            ..crate::config::AppConfig::default()
        };
        config.save().expect("save config");

        // Simulate a restart: a fresh config object is read back from disk
        // and applied to the network layer exactly like `lib.rs` does.
        let loaded = crate::config::AppConfig::load(&dir);
        assert!(loaded.proxy_only);
        assert_eq!(loaded.proxy_url(), Some(proxy.addr.clone()));
        set_system_proxy_override(None);
        set_global_proxy_policy(
            loaded.proxy_enabled,
            loaded.use_system_proxy(),
            loaded.proxy_url(),
            loaded.proxy_only,
        );

        let result = timed_request(&origin.url).await.expect("proxied request");
        assert!(result.contains("proxy-ok"), "unexpected response: {result}");
        assert_eq!(origin.hits.load(Ordering::SeqCst), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ==================== Download lifecycle fault injection ====================
    //
    // A controlled loopback HTTP origin exercises the real
    // `download_to_part_observed` code path (URL policy is bypassed only under
    // `cfg(test)` for `http://127.0.0.1`; release builds still validate the
    // HTTPS allowlist). Every test seeds a destination and checks that a
    // failed download never touches it.

    #[derive(Clone)]
    struct MockSpec {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        declared_len: Option<usize>,
        chunked: bool,
        slow: bool,
    }

    impl MockSpec {
        fn ok(body: &[u8]) -> Self {
            Self {
                status: 200,
                headers: vec![("Content-Type".into(), "application/octet-stream".into())],
                body: body.to_vec(),
                declared_len: None,
                chunked: false,
                slow: false,
            }
        }

        fn status(status: u16) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: b"error".to_vec(),
                declared_len: None,
                chunked: false,
                slow: false,
            }
        }

        fn partial(body: &[u8], total: usize, start: usize) -> Self {
            Self {
                status: 206,
                headers: vec![
                    ("Content-Type".into(), "application/octet-stream".into()),
                    (
                        "Content-Range".into(),
                        format!(
                            "bytes {}-{}/{}",
                            start,
                            start + body.len().saturating_sub(1),
                            total
                        ),
                    ),
                ],
                body: body.to_vec(),
                declared_len: Some(body.len()),
                chunked: false,
                slow: false,
            }
        }

        fn truncated(body: &[u8], declared: usize) -> Self {
            Self {
                status: 200,
                headers: vec![("Content-Type".into(), "application/octet-stream".into())],
                body: body.to_vec(),
                declared_len: Some(declared),
                chunked: false,
                slow: false,
            }
        }

        fn chunked(body: &[u8]) -> Self {
            Self {
                status: 200,
                headers: vec![("Content-Type".into(), "application/octet-stream".into())],
                body: body.to_vec(),
                declared_len: None,
                chunked: true,
                slow: false,
            }
        }

        fn html(body: &[u8]) -> Self {
            Self {
                status: 200,
                headers: vec![("Content-Type".into(), "text/html".into())],
                body: body.to_vec(),
                declared_len: None,
                chunked: false,
                slow: false,
            }
        }

        fn redirect(location: &str) -> Self {
            Self {
                status: 302,
                headers: vec![("Location".into(), location.into())],
                body: Vec::new(),
                declared_len: Some(0),
                chunked: false,
                slow: false,
            }
        }

        fn slow(body: &[u8]) -> Self {
            Self {
                status: 200,
                headers: vec![("Content-Type".into(), "application/octet-stream".into())],
                body: body.to_vec(),
                declared_len: Some(body.len()),
                chunked: false,
                slow: true,
            }
        }
    }

    struct MockServer {
        url: String,
        hits: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
    }

    async fn spawn_mock(specs: Vec<MockSpec>) -> MockServer {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let task_hits = hits.clone();
        let task_started = started.clone();
        tokio::spawn(async move {
            let mut specs = std::collections::VecDeque::from(specs);
            while let Ok((mut socket, _)) = listener.accept().await {
                let hits = task_hits.clone();
                let started = task_started.clone();
                let spec = specs.pop_front().unwrap_or_else(|| MockSpec::status(500));
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                request.extend_from_slice(&chunk[..n]);
                                if request.windows(4).any(|w| w == b"\r\n\r\n")
                                    || request.len() > 65536
                                {
                                    break;
                                }
                            }
                        }
                    }
                    hits.fetch_add(1, Ordering::SeqCst);
                    let reason = match spec.status {
                        200 => "OK",
                        206 => "Partial Content",
                        302 => "Found",
                        400 => "Bad Request",
                        403 => "Forbidden",
                        404 => "Not Found",
                        500 => "Internal Server Error",
                        503 => "Service Unavailable",
                        _ => "Status",
                    };
                    let mut head = format!(
                        "HTTP/1.1 {} {}\r\nConnection: close\r\n",
                        spec.status, reason
                    );
                    if spec.chunked {
                        head.push_str("Transfer-Encoding: chunked\r\n");
                    } else {
                        let len = spec.declared_len.unwrap_or(spec.body.len());
                        head.push_str(&format!("Content-Length: {}\r\n", len));
                    }
                    for (name, value) in &spec.headers {
                        head.push_str(&format!("{}: {}\r\n", name, value));
                    }
                    head.push_str("\r\n");
                    let _ = socket.write_all(head.as_bytes()).await;

                    if spec.chunked {
                        let _ = socket
                            .write_all(format!("{:x}\r\n", spec.body.len()).as_bytes())
                            .await;
                        let _ = socket.write_all(&spec.body).await;
                        let _ = socket.write_all(b"\r\n0\r\n\r\n").await;
                    } else if spec.slow && !spec.body.is_empty() {
                        let split = spec.body.len() / 2;
                        let _ = socket.write_all(&spec.body[..split]).await;
                        started.notify_one();
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        return;
                    } else {
                        let _ = socket.write_all(&spec.body).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });
        MockServer {
            url: format!("http://{}/artifact.bin", addr),
            hits,
            started,
        }
    }

    fn mock_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vl_download_lifecycle_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha1_hex(bytes: &[u8]) -> String {
        use sha1::{Digest, Sha1};
        let mut hasher = Sha1::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn sha512_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha512};
        let mut hasher = Sha512::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    async fn direct_proxy_lock() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = PROXY_TEST_LOCK.lock().await;
        apply_test_proxy(false, false, None, true);
        guard
    }

    fn assert_clean(part: &Path, meta: &Path) {
        assert!(
            !part.exists(),
            ".part must be removed after a terminal error"
        );
        assert!(
            !meta.exists(),
            "the identity sidecar must be removed after a terminal error"
        );
    }

    #[tokio::test]
    async fn lifecycle_success_publishes_destination_and_cleans_temp() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("success");
        let dest = dir.join("artifact.jar");

        download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"hello"),
            Some(5),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect("successful download");

        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_terminal_statuses_clean_temp_and_keep_destination() {
        let _proxy = direct_proxy_lock().await;
        for status in [400u16, 403, 404] {
            let server = spawn_mock(vec![MockSpec::status(status)]).await;
            let dir = mock_dir(&format!("status{status}"));
            let dest = dir.join("artifact.jar");
            std::fs::write(&dest, b"previous-good").unwrap();

            let err = download_to_part_observed(
                &server.url,
                &dest,
                "",
                None,
                Some(Duration::from_secs(10)),
                None,
            )
            .await
            .expect_err("terminal status must fail");
            assert!(err.to_string().contains(&status.to_string()), "got: {err}");
            assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
            assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn lifecycle_transient_503_retries_then_succeeds() {
        let _proxy = direct_proxy_lock().await;
        let mut retry = MockSpec::status(503);
        retry.headers.push(("Retry-After".into(), "1".into()));
        let server = spawn_mock(vec![retry, MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("retry503");
        let dest = dir.join("artifact.jar");

        download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"hello"),
            Some(5),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect("503 must be retried");

        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_redirect_stop_cleans_without_contacting_target() {
        let _proxy = direct_proxy_lock().await;
        let target = spawn_mock(vec![MockSpec::ok(b"must-not-be-written")]).await;
        let redirect = spawn_mock(vec![MockSpec::redirect(&target.url)]).await;
        let dir = mock_dir("redirect");
        let dest = dir.join("artifact.jar");
        std::fs::write(&dest, b"previous-good").unwrap();

        let err = download_to_part_observed(
            &redirect.url,
            &dest,
            "",
            None,
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("redirect must stop");
        assert!(err.to_string().contains("redirect"), "got: {err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        assert_eq!(
            target.hits.load(Ordering::SeqCst),
            0,
            "the refused redirect target must never be contacted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_wrong_content_type_cleans_temp() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::html(b"<html>nope</html>")]).await;
        let dir = mock_dir("contenttype");
        let dest = dir.join("artifact.jar");

        let err = download_to_part_observed(
            &server.url,
            &dest,
            "",
            None,
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("HTML must be rejected");
        assert!(err.to_string().contains("content-type"), "got: {err}");
        assert!(!dest.exists());
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_declared_size_over_expected_fails_before_writing() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("declared-size");
        let dest = dir.join("artifact.jar");
        std::fs::write(&dest, b"previous-good").unwrap();

        let err = download_to_part_observed(
            &server.url,
            &dest,
            "",
            Some(3),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("a declared body larger than the artifact must fail");
        assert!(err.to_string().contains("advertises"), "got: {err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_chunked_body_over_expected_is_stopped_while_streaming() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::chunked(b"hello")]).await;
        let dir = mock_dir("chunked-size");
        let dest = dir.join("artifact.jar");
        std::fs::write(&dest, b"previous-good").unwrap();

        let err = download_to_part_observed(
            &server.url,
            &dest,
            "",
            Some(3),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("a chunked body larger than the artifact must stop mid-stream");
        assert!(
            err.to_string().contains("more than the expected"),
            "got: {err}"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_checksum_mismatch_cleans_temp() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("checksum");
        let dest = dir.join("artifact.jar");
        std::fs::write(&dest, b"previous-good").unwrap();

        let err = download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"not-hello"),
            Some(5),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("checksum mismatch must fail");
        assert!(err.to_string().to_lowercase().contains("sha"), "got: {err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_truncated_body_resumes_with_range() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![
            MockSpec::truncated(b"hello", 10),
            MockSpec::partial(b"world", 10, 5),
        ])
        .await;
        let dir = mock_dir("resume");
        let dest = dir.join("artifact.jar");

        download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"helloworld"),
            Some(10),
            Some(Duration::from_secs(20)),
            None,
        )
        .await
        .expect("truncated body must resume");

        assert_eq!(std::fs::read(&dest).unwrap(), b"helloworld");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_existing_matching_part_resumes_across_calls() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::partial(b"world", 10, 5)]).await;
        let dir = mock_dir("resume-existing");
        let dest = dir.join("artifact.jar");
        let identity = download_identity(&server.url, &sha1_hex(b"helloworld"), Some(10));
        std::fs::write(part_path_for(&dest), b"hello").unwrap();
        std::fs::write(part_meta_path(&dest), identity).unwrap();

        download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"helloworld"),
            Some(10),
            Some(Duration::from_secs(20)),
            None,
        )
        .await
        .expect("matching .part must resume");

        assert_eq!(std::fs::read(&dest).unwrap(), b"helloworld");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_part_open_failure_removes_sidecar() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("open-fail");
        let dest = dir.join("artifact.jar");
        // A directory at the `.part` path makes File::create fail after the
        // identity sidecar was already written.
        std::fs::create_dir_all(part_path_for(&dest)).unwrap();

        let err = download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"hello"),
            Some(5),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("opening a directory as .part must fail");

        assert!(
            err.to_string().contains("Failed to create .part"),
            "got: {err}"
        );
        assert!(!dest.exists());
        assert!(
            !part_meta_path(&dest).exists(),
            "the sidecar must not survive an open failure"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_write_failure_cleans_temp() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("write-fail");
        let dest = dir.join("artifact.jar");
        std::fs::write(&dest, b"previous-good").unwrap();
        fail_next_write();

        let err = download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"hello"),
            Some(5),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("injected write failure must fail");
        assert!(
            err.to_string().contains("injected .part write"),
            "got: {err}"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_flush_failure_cleans_temp() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("flush-fail");
        let dest = dir.join("artifact.jar");
        std::fs::write(&dest, b"previous-good").unwrap();
        fail_next_flush();

        let err = download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"hello"),
            Some(5),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("injected flush failure must fail");
        assert!(
            err.to_string().contains("injected .part flush"),
            "got: {err}"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_rename_failure_cleans_temp_and_keeps_destination() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("rename-fail");
        let dest = dir.join("artifact.jar");
        // A directory at the destination makes the final rename fail.
        std::fs::create_dir_all(&dest).unwrap();

        let _err = download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"hello"),
            Some(5),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("rename onto a directory must fail");

        assert!(dest.is_dir(), "the existing destination must be preserved");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_sidecar_creation_failure_cleans_part() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("sidecar-fail");
        let dest = dir.join("artifact.jar");
        // A directory at the sidecar path makes the identity write fail.
        std::fs::create_dir_all(part_meta_path(&dest)).unwrap();

        let err = download_to_part_observed(
            &server.url,
            &dest,
            &sha1_hex(b"hello"),
            Some(5),
            Some(Duration::from_secs(10)),
            None,
        )
        .await
        .expect_err("an unwritable identity sidecar must fail the download");
        assert!(err.to_string().contains("identity"), "got: {err}");
        assert!(!dest.exists());
        assert!(
            !part_path_for(&dest).exists(),
            "no .part may survive a failed identity write"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_http_client_failure_cleans_temp() {
        let _proxy = direct_proxy_lock().await;
        let dir = mock_dir("client-fail");
        let dest = dir.join("artifact.jar");
        std::fs::write(&dest, b"previous-good").unwrap();
        fail_next_client();

        let err = download_to_part_observed(
            "http://127.0.0.1:9/artifact.bin",
            &dest,
            "",
            Some(5),
            Some(Duration::from_secs(5)),
            None,
        )
        .await
        .expect_err("injected client failure must fail");
        assert!(err.to_string().contains("client build"), "got: {err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[ignore = "production retry backoff takes about two minutes"]
    async fn lifecycle_exhausted_500_retries_then_cleans_temp() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::status(500); 12]).await;
        let dir = mock_dir("exhaust500");
        let dest = dir.join("artifact.jar");
        std::fs::write(&dest, b"previous-good").unwrap();

        let err = tokio::time::timeout(
            Duration::from_secs(180),
            download_to_part_observed(
                &server.url,
                &dest,
                "",
                None,
                Some(Duration::from_secs(10)),
                None,
            ),
        )
        .await
        .expect("exhausted retries must finish")
        .expect_err("500 must eventually fail");

        println!(
            "exhausted 500: hits={} error={err}",
            server.hits.load(Ordering::SeqCst)
        );
        assert!(err.to_string().contains("500"), "got: {err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_parallel_downloads_keep_their_own_temp_files() {
        let _proxy = direct_proxy_lock().await;
        let a = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let b = spawn_mock(vec![MockSpec::ok(b"world")]).await;
        let dir = mock_dir("parallel");
        let dest_a = dir.join("a.jar");
        let dest_b = dir.join("b.jar");
        let sha_a = sha1_hex(b"hello");
        let sha_b = sha1_hex(b"world");

        let (ra, rb) = tokio::join!(
            download_to_part_observed(
                &a.url,
                &dest_a,
                &sha_a,
                Some(5),
                Some(Duration::from_secs(10)),
                None
            ),
            download_to_part_observed(
                &b.url,
                &dest_b,
                &sha_b,
                Some(5),
                Some(Duration::from_secs(10)),
                None
            )
        );
        ra.expect("first parallel download");
        rb.expect("second parallel download");
        assert_eq!(std::fs::read(&dest_a).unwrap(), b"hello");
        assert_eq!(std::fs::read(&dest_b).unwrap(), b"world");
        assert_clean(&part_path_for(&dest_a), &part_meta_path(&dest_a));
        assert_clean(&part_path_for(&dest_b), &part_meta_path(&dest_b));

        // Two downloads of the same destination are serialized by the path
        // lock; both finish and leave no temporary files behind.
        let shared = spawn_mock(vec![
            MockSpec::ok(b"same-bytes"),
            MockSpec::ok(b"same-bytes"),
        ])
        .await;
        let dest = dir.join("shared.jar");
        let shared_sha = sha1_hex(b"same-bytes");
        let (r1, r2) = tokio::join!(
            download_to_part_observed(
                &shared.url,
                &dest,
                &shared_sha,
                Some(10),
                Some(Duration::from_secs(10)),
                None
            ),
            download_to_part_observed(
                &shared.url,
                &dest,
                &shared_sha,
                Some(10),
                Some(Duration::from_secs(10)),
                None
            )
        );
        r1.expect("first shared download");
        r2.expect("second shared download");
        assert_eq!(std::fs::read(&dest).unwrap(), b"same-bytes");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_pause_before_start_writes_no_temp_files() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::ok(b"hello")]).await;
        let dir = mock_dir("pause");
        let dest = dir.join("artifact.jar");
        std::fs::write(&dest, b"previous-good").unwrap();

        let scope = PausableInstallScope::begin();
        request_pause();
        let err = download_to_part_observed(
            &server.url,
            &dest,
            "",
            Some(5),
            Some(Duration::from_secs(5)),
            None,
        )
        .await
        .expect_err("a paused operation must stop");
        assert!(matches!(err, LauncherError::Paused));
        clear_pause();
        drop(scope);

        assert_eq!(std::fs::read(&dest).unwrap(), b"previous-good");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        assert_eq!(server.hits.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_aborted_download_keeps_part_for_resume() {
        let _proxy = direct_proxy_lock().await;
        let slow = spawn_mock(vec![MockSpec::slow(b"helloworld")]).await;
        let dir = mock_dir("abort");
        let dest = dir.join("artifact.jar");
        let url = slow.url.clone();
        let path = dest.clone();
        let sha = sha1_hex(b"helloworld");

        let task = tokio::spawn(async move {
            download_to_part_observed(
                &url,
                &path,
                &sha,
                Some(10),
                Some(Duration::from_secs(60)),
                None,
            )
            .await
        });

        // Wait until the server has sent half the body *and* the downloader
        // has persisted it, then abort the task.
        slow.started.notified().await;
        let part_path = part_path_for(&dest);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while std::fs::metadata(&part_path)
            .map(|meta| meta.len())
            .unwrap_or(0)
            < 5
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the downloader did not persist the first chunk in time"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        task.abort();
        let _ = task.await;

        assert!(
            !dest.exists(),
            "an aborted download must not publish a file"
        );
        assert_eq!(
            std::fs::read(&part_path).unwrap(),
            b"hello",
            "aborting must keep the partial bytes for a later resume"
        );
        assert!(part_meta_path(&dest).exists());

        // The saved `.part` is valid: a later run resumes and completes.
        let resume = spawn_mock(vec![MockSpec::partial(b"world", 10, 5)]).await;
        let identity = download_identity(&resume.url, &sha1_hex(b"helloworld"), Some(10));
        std::fs::write(part_meta_path(&dest), identity).unwrap();
        download_to_part_observed(
            &resume.url,
            &dest,
            &sha1_hex(b"helloworld"),
            Some(10),
            Some(Duration::from_secs(20)),
            None,
        )
        .await
        .expect("the retained .part must be resumable");
        assert_eq!(std::fs::read(&dest).unwrap(), b"helloworld");
        assert_clean(&part_path_for(&dest), &part_meta_path(&dest));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

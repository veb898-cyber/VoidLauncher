// ==================== Misc Commands ====================
// File operations, launch state, cache clearing,
// system info, and configuration commands.

use crate::config::AppConfig;
use crate::events;
use crate::AppState;
use tauri::{AppHandle, State};

#[tauri::command]
pub fn cmd_rename_file(state: State<'_, AppState>, from: String, to: String) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instances_dir = config.instances_dir();
    let from_path = std::path::Path::new(&from);
    let to_path = std::path::Path::new(&to);
    // Open the source first to fail fast on missing/unreadable files.
    let _ = std::fs::File::open(from_path).map_err(|e| e.to_string())?;
    let from_canon = from_path
        .canonicalize()
        .map_err(|_| "Access denied: invalid source path".to_string())?;
    let base_canon = instances_dir
        .canonicalize()
        .map_err(|_| "Invalid base".to_string())?;
    if !from_canon.starts_with(&base_canon) {
        return Err("Access denied: path is outside instances directory".to_string());
    }
    // Target may not exist yet; check parent
    if let Some(parent) = to_path.parent() {
        let parent_canon = parent
            .canonicalize()
            .map_err(|_| "Access denied: invalid target path".to_string())?;
        if !parent_canon.starts_with(&base_canon) {
            return Err("Access denied: target is outside instances directory".to_string());
        }
    }
    std::fs::rename(&from, &to).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_delete_file(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instances_dir = config.instances_dir();
    let canon = std::path::Path::new(&path)
        .canonicalize()
        .map_err(|_| "Access denied: invalid path".to_string())?;
    let base_canon = instances_dir
        .canonicalize()
        .map_err(|_| "Invalid base".to_string())?;
    if !canon.starts_with(&base_canon) {
        return Err("Access denied: path is outside instances directory".to_string());
    }
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

/// Read an image file picked via the OS file dialog so the renderer never
/// needs a broad filesystem scope. Restricted to image extensions and a
/// size limit — an XSS in the renderer cannot exfiltrate arbitrary files.
#[tauri::command]
pub fn cmd_read_image_file(path: String) -> Result<Vec<u8>, String> {
    const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
    // Canonicalize first: resolves symlinks and rejects non-existent paths,
    // so the extension check below cannot be bypassed by aliasing.
    let file = std::path::Path::new(&path)
        .canonicalize()
        .map_err(|_| "Invalid file path".to_string())?;
    if !file.is_file() {
        return Err("Not a file".to_string());
    }
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !["png", "jpg", "jpeg", "ico"].contains(&ext.as_str()) {
        return Err("File must be a PNG, JPG or ICO image".to_string());
    }
    let meta = std::fs::metadata(&file).map_err(|e| e.to_string())?;
    if meta.len() > MAX_IMAGE_BYTES {
        return Err("Image file is too large (max 10 MB)".to_string());
    }
    std::fs::read(&file).map_err(|e| e.to_string())
}

/// Fetch a description-page asset (screenshot, banner, badge) from ANY public
/// host and return it as a base64 data URL. The webview honors only the
/// system proxy without our proxy->direct fallback, so images embedded in
/// Modrinth/CurseForge markdown often fail to render when loaded directly.
/// SSRF guard: the URL must be http(s), and both the original and the final
/// redirect host must resolve to a PUBLIC address (loopback / private /
/// link-local targets are refused). Response is capped at 10 MB.
#[tauri::command]
/// Extract the host portion of a URL. Handles IPv6 literals in brackets
/// (with or without a port), a trailing port, and userinfo (`user@host`).
/// Returns `None` when there is no parseable host.
///
/// Security note: we deliberately parse independently of `url`'s URL crate
/// semantics here so the SSRF guard can never be confused by scheme or
/// authority edge cases that a URL parser might normalise differently.
fn extract_host(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let authority = &rest[..end];
    // Strip userinfo if present.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);

    // Bracketed IPv6 literal, e.g. `[::1]` or `[2001:db8::1]:8080`.
    if host_port.starts_with('[') {
        let close = host_port.find(']')?;
        let host = &host_port[1..close];
        if host.is_empty() { None } else { Some(host.to_string()) }
    } else {
        // IPv4 / hostname, possibly with a port.
        let host = host_port.split(':').next()?;
        if host.is_empty() { None } else { Some(host.to_string()) }
    }
}

/// True when `ip` is a routable public address. Every private / loopback /
/// link-local / unspecified / broadcast / multicast / reserved address is
/// rejected — this is the SSRF hard-line predicate.
///
/// IPv4-mapped IPv6 addresses (e.g. `::ffff:127.0.0.1`) and IPv4-compatible
/// forms (`::127.0.0.1`) are unwrapped to their embedded IPv4 and judged by
/// IPv4 rules, so a private IPv4 smuggled through the IPv6 namespace cannot
/// bypass the guard.
fn is_public_ip(ip: std::net::IpAddr) -> bool {
    // Unwrap IPv4-mapped / IPv4-compatible IPv6 addresses and re-check as v4.
    if let std::net::IpAddr::V6(v6) = ip {
        if let Some(v4) = v6.to_ipv4_mapped() {
            return is_public_ip(std::net::IpAddr::V4(v4));
        }
        // `::127.0.0.1`, `::10.0.0.1`, etc. (IPv4-compatible form, now legacy
        // but still accepted by some stacks) — inspect the trailing 4 bytes.
        if v6.segments()[..4].iter().all(|&s| s == 0) {
            let octets = v6.octets();
            let v4 = std::net::Ipv4Addr::new(octets[4], octets[5], octets[6], octets[7]);
            return is_public_ip(std::net::IpAddr::V4(v4));
        }
    }

    match ip {
        std::net::IpAddr::V4(v4) => {
            !(v4.is_loopback() || v4.is_private() || v4.is_link_local()
                || is_v4_reserved(&v4)
                || v4.is_broadcast() || v4.is_multicast() || v4.is_unspecified())
        }
        std::net::IpAddr::V6(v6) => {
            let seg = v6.segments();
            let unique_local = (seg[0] & 0xfe00) == 0xfc00;
            let link_local = (seg[0] & 0xffc0) == 0xfe80;
            // `v6.is_unicast_link_local()` covers fe80::/10; the 2001:db8::
            // documentation range and the 2001::/32 Teredo range are neither
            // routable to the public internet nor something we should fetch.
            let documentation = seg[0] == 0x2001 && seg[1] == 0x0db8;
            let teredo = seg[0] == 0x2001 && seg[1] == 0x0000;
            !(v6.is_loopback() || v6.is_multicast() || v6.is_unspecified()
                || unique_local || link_local || documentation || teredo)
        }
    }
}

/// Additional deprecated / documentation / globally-reserved IPv4 ranges the
/// stdlib's `is_private`/`is_link_local` do not cover for SSRF purposes:
///   192.0.0.0/24  (IETF protocol assignments, incl. 192.0.0.170/171)
///   192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24  (documentation)
///   198.18.0.0/15 (benchmarking)
///   240.0.0.0/4   (reserved / future use)
fn is_v4_reserved(v4: &std::net::Ipv4Addr) -> bool {
    let o = v4.octets();
    match o[0] {
        192 => {
            (o[1] == 0 && o[2] == 0)        // 192.0.0.0/24
                || (o[1] == 0 && o[2] == 2) // 192.0.2.0/24 documentation
        }
        198 => (o[1] == 18 || o[1] == 19)   // 198.18.0.0/15 benchmarking
            || (o[1] == 51 && o[2] == 100)  // 198.51.100.0/24 documentation
            || (o[1] == 0 && o[2] == 0),    // 198.0.0.0 reserved block (198.0.0.0/8 partly)
        203 => o[1] == 0 && o[2] == 113,    // 203.0.113.0/24 documentation
        240..=255 => true,                  // 240.0.0.0/4 reserved
        _ => false,
    }
}

/// Resolve a URL's host and decide whether it points at a purely public
/// address. IP literals (v4 and v6) are judged directly; hostnames are
/// resolved to all of their addresses and every one must be public (if any
/// single address is private/local the host is refused) — this catches a
/// hostname that resolves to 127.0.0.1 or a private NAT range.
async fn host_is_public(url: &str) -> Result<bool, String> {
    let host = extract_host(url).ok_or_else(|| "Invalid URL".to_string())?;
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(is_public_ip(ip));
    }
    tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        match (host.as_str(), 443u16).to_socket_addrs() {
            Ok(addrs) => {
                let ips: Vec<std::net::IpAddr> = addrs.map(|a| a.ip()).collect();
                if ips.is_empty() {
                    return Err("Host resolved to no addresses".to_string());
                }
                Ok(ips.iter().all(|ip| is_public_ip(*ip)))
            }
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cmd_fetch_page_asset(url: String) -> Result<Option<String>, String> {
    const MAX_ASSET_BYTES: usize = 10 * 1024 * 1024;

    let url = url.trim().to_string();
    if !url.starts_with("https://") {
        return Err("Only https URLs are allowed".into());
    }

    let check_url = url.clone();
    for hop in 0..4 {
        match host_is_public(&check_url).await {
            Ok(true) => break,
            Ok(false) => return Err("Refusing to fetch from a private address".into()),
            Err(_) if hop > 0 => break, // final hop unreachable: accept earlier verdicts
            Err(e) => return Err(e),
        }
    }

    let client = crate::download::global_http_client();
    let resp = crate::download::send_with_fallback(client.get(&url))
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    // Re-check the FINAL url after redirects (redirect target could be private).
    let final_url = resp.url().to_string();
    if final_url != url {
        match host_is_public(&final_url).await {
            Ok(false) => return Err("Refusing to fetch from a private address".into()),
            _ => {}
        }
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_ASSET_BYTES {
            return Ok(None);
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) if b.len() <= MAX_ASSET_BYTES => b,
        _ => return Ok(None),
    };

    // Sniff the MIME type from magic bytes; fall back to the extension.
    let mime = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        "image/svg+xml"
    } else {
        match url.split('?').next().and_then(|p| p.rsplit('.').next()).unwrap_or("") {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "avif" => "image/avif",
            _ => return Ok(None),
        }
    };
    let b64 = crate::commands::mods::base64_encode_pub(&bytes);
    Ok(Some(format!("data:{};base64,{}", mime, b64)))
}

/// Check for launcher updates by fetching `latest.json` from GitHub through
/// the launcher's HTTP stack (proxy with direct fallback). The URL is
/// hardcoded here so the renderer cannot abuse this as an SSRF proxy; doing
/// it in the backend (instead of a webview `fetch`) means users behind
/// proxies that block raw.githubusercontent.com still get update checks.
#[tauri::command]
pub async fn cmd_check_latest_version() -> Result<Option<String>, String> {
    const LATEST_JSON_URL: &str =
        "https://raw.githubusercontent.com/veb898-cyber/VoidLauncher/main/latest.json";
    let client = crate::download::global_http_client();
    let resp = crate::download::send_with_fallback(
        client.get(LATEST_JSON_URL).timeout(std::time::Duration::from_secs(15)),
    )
    .await
    .map_err(|e| format!("Update check failed: {}", e))?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let v = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|j| j.get("version").and_then(|v| v.as_str()).map(String::from));
    Ok(v)
}


// ==================== Launch State Commands ====================

/// Return the names of all instances whose game processes are currently running.
#[tauri::command]
pub fn cmd_get_launch_state(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let running = state
        .running_instances
        .lock()
        .map_err(|e| e.to_string())?;
    Ok(running.clone())
}

// ==================== Cache Commands ====================

#[tauri::command]
pub fn cmd_clear_cache(app: AppHandle, state: State<'_, AppState>) -> Result<u64, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let data_dir = config.data_dir.clone();
    drop(config);

    let mut freed: u64 = 0;
    for subdir in &["assets", "libraries"] {
        let dir = data_dir.join(subdir);
        if dir.exists() {
            let size = dir_size(&dir).unwrap_or(0);
            std::fs::remove_dir_all(&dir).map_err(|e| {
                let msg = format!("Failed to remove {:?}: {}", dir, e);
                events::emit_log(&app, "error", "cache", &msg);
                msg
            })?;
            freed += size;
            events::emit_log(
                &app,
                "info",
                "cache",
                &format!("Removed {:?} ({} MB)", dir, size / 1024 / 1024),
            );
        }
    }

    Ok(freed)
}

fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            total += dir_size(&entry.path()).unwrap_or(0);
        } else {
            total += meta.len();
        }
    }
    Ok(total)
}


// ==================== System Commands ====================

#[tauri::command]
pub fn cmd_detect_system_ram() -> u64 {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    sys.total_memory() / (1024 * 1024) // Return MB
}

// ==================== Config Commands ====================

#[tauri::command]
pub fn cmd_get_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(config.clone())
}

#[tauri::command]
pub fn cmd_save_config(
    app: AppHandle,
    state: State<'_, AppState>,
    new_config: AppConfig,
) -> Result<(), String> {
    events::emit_log(&app, "info", "config", "Saving configuration...");
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    *config = new_config;
    config.save().map_err(|e| e.to_string())?;
    crate::download::set_global_proxy(config.proxy_url());
    events::emit_log(&app, "info", "config", "Configuration saved");
    Ok(())
}

/// Resolve a path against `base` for folder-open containment. The target is
/// canonicalized (resolving `..`, absolute/drive paths, symlinks/junctions)
/// and must end up strictly inside `base` (or equal to it). If the target
/// does not exist yet it is created first so it can be canonicalized.
///
/// Returns the canonical, contained target path.
fn resolve_open_folder_path(base: &std::path::Path, target: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if !target.exists() {
        std::fs::create_dir_all(target).map_err(|e| e.to_string())?;
    }
    let target_canon = target
        .canonicalize()
        .map_err(|_| "Access denied: invalid folder path".to_string())?;
    let base_canon = base
        .canonicalize()
        .map_err(|_| "Access denied: data folder unavailable".to_string())?;
    if !target_canon.starts_with(&base_canon) {
        return Err("Access denied: folder is outside the launcher data directory".to_string());
    }
    Ok(target_canon)
}

/// Open a folder in the system file manager. Used by the settings page
/// (data folder, game logs). Creates the folder if it does not exist.
///
/// Path containment: the requested path must live inside the launcher's
/// `data_dir` (which already covers `data_dir` itself, `logs/game`, and any
/// other launcher subfolder). This prevents the renderer from opening or
/// creating arbitrary system directories.
#[tauri::command]
pub fn cmd_open_folder(state: State<'_, AppState>, app: AppHandle, path: String) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let data_dir = config.data_dir.clone();
    drop(config);

    let target_canon = resolve_open_folder_path(&data_dir, std::path::Path::new(&path))?;

    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(target_canon.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod open_folder_tests {
    use super::resolve_open_folder_path;

    #[test]
    fn allows_child_of_base() {
        let tmp = std::env::temp_dir().join(format!(
            "void-open-test-{}",
            std::process::id()
        ));
        let base = tmp.join("data");
        let child = base.join("logs").join("game");
        std::fs::create_dir_all(&child).unwrap();
        let got = resolve_open_folder_path(&base, &child).unwrap();
        assert!(got.starts_with(&base.canonicalize().unwrap()));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn allows_base_itself() {
        let tmp = std::env::temp_dir().join(format!(
            "void-open-test-base-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let got = resolve_open_folder_path(&tmp, &tmp).unwrap();
        assert_eq!(got, tmp.canonicalize().unwrap());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn rejects_path_outside_base() {
        let tmp = std::env::temp_dir().join(format!(
            "void-open-test-out-{}",
            std::process::id()
        ));
        let base = tmp.join("data");
        let outside = tmp.join("other");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        assert!(resolve_open_folder_path(&base, &outside).is_err());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn rejects_traversal_attempt() {
        let tmp = std::env::temp_dir().join(format!(
            "void-open-test-trav-{}",
            std::process::id()
        ));
        let base = tmp.join("data").join("sub");
        std::fs::create_dir_all(&base).unwrap();
        let escape = base.join("..").join("..").join("other");
        std::fs::create_dir_all(&escape).unwrap();
        assert!(resolve_open_folder_path(&base, &escape).is_err());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn creates_missing_folder_and_contains_it() {
        let tmp = std::env::temp_dir().join(format!(
            "void-open-test-create-{}",
            std::process::id()
        ));
        let base = tmp.join("data");
        let missing = base.join("logs").join("game");
        let got = resolve_open_folder_path(&base, &missing).unwrap();
        assert!(got.is_dir());
        assert!(got.starts_with(&base.canonicalize().unwrap()));
        std::fs::remove_dir_all(&tmp).ok();
    }
}

#[cfg(test)]
mod ssrf_tests {
    use super::extract_host;
    use super::is_public_ip;
    use std::net::IpAddr;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::new(a, b, c, d))
    }
    fn v6(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    // ==================== host extraction ====================

    #[test]
    fn extract_host_plain_hostname() {
        assert_eq!(extract_host("https://cdn.example.com/img.png"), Some("cdn.example.com".into()));
    }

    #[test]
    fn extract_host_with_port() {
        assert_eq!(extract_host("https://cdn.example.com:8443/img.png"), Some("cdn.example.com".into()));
    }

    #[test]
    fn extract_host_ipv4() {
        assert_eq!(extract_host("https://127.0.0.1/x"), Some("127.0.0.1".into()));
        assert_eq!(extract_host("https://192.168.1.5:80/x"), Some("192.168.1.5".into()));
    }

    #[test]
    fn extract_host_ipv6_literal() {
        assert_eq!(extract_host("https://[::1]/x"), Some("::1".into()));
        assert_eq!(extract_host("https://[2001:db8::1]/x"), Some("2001:db8::1".into()));
    }

    #[test]
    fn extract_host_ipv6_literal_with_port() {
        assert_eq!(extract_host("https://[::1]:8080/x"), Some("::1".into()));
        assert_eq!(extract_host("https://[fe80::1]:8443/x"), Some("fe80::1".into()));
    }

    #[test]
    fn extract_host_strips_userinfo() {
        assert_eq!(extract_host("https://user@example.com/x"), Some("example.com".into()));
        assert_eq!(extract_host("https://a:b@127.0.0.1/x"), Some("127.0.0.1".into()));
    }

    #[test]
    fn extract_host_missing_or_malformed() {
        assert_eq!(extract_host("https:///x"), None);
        assert_eq!(extract_host("not-a-url"), None);
        assert_eq!(extract_host("https://[::1"), None); // unterminated bracket
    }

    // ==================== IPv4 addresses ====================

    #[test]
    fn public_ipv4_is_public() {
        assert!(is_public_ip(v4(8, 8, 8, 8)));
        assert!(is_public_ip(v4(93, 184, 216, 34)));
        assert!(is_public_ip(v4(1, 1, 1, 1)));
    }

    #[test]
    fn ipv4_loopback_is_private() {
        assert!(!is_public_ip(v4(127, 0, 0, 1)));
        assert!(!is_public_ip(v4(127, 255, 255, 254)));
        assert!(!is_public_ip(v4(127, 1, 2, 3)));
    }

    #[test]
    fn private_ipv4_ranges_are_private() {
        // 10/8, 172.16/12, 192.168/16
        assert!(!is_public_ip(v4(10, 0, 0, 1)));
        assert!(!is_public_ip(v4(10, 255, 255, 255)));
        assert!(!is_public_ip(v4(172, 16, 0, 1)));
        assert!(!is_public_ip(v4(172, 31, 255, 255)));
        // 172.32.0.0 is OUTSIDE RFC1918's 172.16/12 range (that covers only
        // 172.16.0.0-172.31.255.255), so it is not private — must stay public.
        assert!(is_public_ip(v4(172, 32, 0, 1)));
        assert!(!is_public_ip(v4(192, 168, 0, 1)));
        assert!(!is_public_ip(v4(192, 168, 255, 255)));
    }

    #[test]
    fn link_local_ipv4_is_private() {
        assert!(!is_public_ip(v4(169, 254, 0, 1)));
        assert!(!is_public_ip(v4(169, 254, 169, 254)));
    }

    #[test]
    fn unspecified_broadcast_multicast_are_private() {
        assert!(!is_public_ip(v4(0, 0, 0, 0)));
        assert!(!is_public_ip(v4(255, 255, 255, 255)));
        assert!(!is_public_ip(v4(224, 0, 0, 1)));
    }

    #[test]
    fn reserved_ipv4_ranges_are_private() {
        // Documentation 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24
        assert!(!is_public_ip(v4(192, 0, 2, 1)));
        assert!(!is_public_ip(v4(198, 51, 100, 5)));
        assert!(!is_public_ip(v4(203, 0, 113, 7)));
        // Benchmarking 198.18.0.0/15
        assert!(!is_public_ip(v4(198, 18, 0, 1)));
        assert!(!is_public_ip(v4(198, 19, 255, 255)));
        // 240.0.0.0/4 reserved
        assert!(!is_public_ip(v4(240, 0, 0, 1)));
        assert!(!is_public_ip(v4(250, 0, 0, 1)));
    }

    // ==================== IPv6 addresses ====================

    #[test]
    fn ipv6_loopback_is_private() {
        assert!(!is_public_ip(v6("::1")));
    }

    #[test]
    fn ipv6_unique_local_is_private() {
        assert!(!is_public_ip(v6("fc00::1")));
        assert!(!is_public_ip(v6("fd12:3456:789a::1")));
    }

    #[test]
    fn ipv6_link_local_is_private() {
        assert!(!is_public_ip(v6("fe80::1")));
        assert!(!is_public_ip(v6("fe80::a:b:c:d")));
    }

    #[test]
    fn ipv6_unspecified_multicast_are_private() {
        assert!(!is_public_ip(v6("::")));
        assert!(!is_public_ip(v6("ff02::1")));
    }

    #[test]
    fn ipv6_documentation_and_teredo_are_private() {
        assert!(!is_public_ip(v6("2001:db8::1")));
        // Teredo 2001::/32 relays reachable private addresses
        assert!(!is_public_ip(v6("2001:0000:4136:e378:8000:63bf:3fff:fdd2")));
    }

    #[test]
    fn public_ipv6_is_public() {
        assert!(is_public_ip(v6("2606:4700:4700::64")));       // Cloudflare
        assert!(is_public_ip(v6("2a00:1450:4001:812::200e"))); // Google
        assert!(is_public_ip(v6("2001:4860:4860::8888")));
    }

    // ==================== IPv4-mapped / IPv4-compatible IPv6 ====================

    #[test]
    fn ipv4_mapped_loopback_is_private() {
        assert!(!is_public_ip(v6("::ffff:127.0.0.1")));
        assert!(!is_public_ip(v6("::ffff:7f00:0001")));
    }

    #[test]
    fn ipv4_mapped_private_is_private() {
        assert!(!is_public_ip(v6("::ffff:192.168.1.1")));
        assert!(!is_public_ip(v6("::ffff:c0a8:0101")));
        assert!(!is_public_ip(v6("::ffff:10.0.0.1")));
        assert!(!is_public_ip(v6("::ffff:0a00:0001")));
    }

    #[test]
    fn ipv4_mapped_public_is_public() {
        assert!(is_public_ip(v6("::ffff:8.8.8.8")));
        assert!(is_public_ip(v6("::ffff:0808:0808")));
    }

    #[test]
    fn ipv4_compatible_loopback_is_private() {
        // Legacy ::a.b.c.d form — no v4-mapped prefix, but the embedded
        // IPv4 is a loopback / private address.
        assert!(!is_public_ip(v6("::127.0.0.1")));
        assert!(!is_public_ip(v6("::192.168.0.1")));
        assert!(!is_public_ip(v6("::ffff:0:127.0.0.1")));
    }
}

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
pub async fn cmd_fetch_page_asset(url: String) -> Result<Option<String>, String> {
    const MAX_ASSET_BYTES: usize = 10 * 1024 * 1024;

    fn extract_host(url: &str) -> Option<String> {
        let rest = url.split("://").nth(1)?;
        let end = rest
            .find(|c: char| c == '/' || c == '?' || c == '#' || c == '@')
            .unwrap_or(rest.len());
        let authority = &rest[..end];
        // Strip userinfo if present, then port.
        let host_port = authority.rsplit('@').next().unwrap_or(authority);
        let host = host_port.split(':').next()?;
        let host = host.trim_matches(['[', ']']);
        if host.is_empty() { None } else { Some(host.to_string()) }
    }

    fn is_public_ip(ip: std::net::IpAddr) -> bool {
        match ip {
            std::net::IpAddr::V4(v4) => {
                !(v4.is_loopback() || v4.is_private() || v4.is_link_local()
                    || v4.is_broadcast() || v4.is_multicast() || v4.is_unspecified())
            }
            std::net::IpAddr::V6(v6) => {
                let seg = v6.segments();
                let unique_local = (seg[0] & 0xfe00) == 0xfc00;
                let link_local = (seg[0] & 0xffc0) == 0xfe80;
                !(v6.is_loopback() || v6.is_multicast() || v6.is_unspecified()
                    || unique_local || link_local)
            }
        }
    }

    async fn host_is_public(url: &str) -> Result<bool, String> {
        let host = extract_host(url).ok_or_else(|| "Invalid URL".to_string())?;
        // IP literals are checked directly; names are resolved first so a
        // hostname pointing at 127.0.0.1 cannot bypass the guard.
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

    let url = url.trim().to_string();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Only http(s) URLs are allowed".into());
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

#[tauri::command]
pub fn cmd_get_launch_state(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let running = state
        .running_instance_id
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

/// Open a folder in the system file manager. Used by the settings page
/// (data folder, game logs). Creates the folder if it does not exist.
#[tauri::command]
pub fn cmd_open_folder(app: AppHandle, path: String) -> Result<(), String> {
    let target = std::path::PathBuf::from(&path);
    if !target.exists() {
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(target.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

// ==================== Misc Commands ====================
// File operations, icon cache, launch state, cache clearing,
// system info, and configuration commands.

use crate::config::AppConfig;
use crate::events;
use crate::save_icon_cache_to_disk;
use crate::AppState;
use std::collections::HashMap;
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


// ==================== Icon Cache ====================

#[tauri::command]
pub fn cmd_get_icon_cache(state: State<'_, AppState>) -> Result<HashMap<String, String>, String> {
    let cache = state.icon_cache.read().map_err(|e| e.to_string())?;
    Ok(cache.clone())
}

#[tauri::command]
pub fn cmd_set_icon_cache_entry(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut cache = state.icon_cache.write().map_err(|e| e.to_string())?;
    cache.insert(key, value);
    save_icon_cache_to_disk(&config, &cache);
    Ok(())
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

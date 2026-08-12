// ==================== Version Commands ====================
// Version manifest/info and Java management commands.

use crate::java;
use crate::java_download;
use crate::is_allowed_download_host;
use crate::versions;
use crate::AppState;
use tauri::{AppHandle, State};


#[tauri::command]
pub async fn cmd_get_versions() -> Result<versions::VersionManifest, String> {
    versions::fetch_version_manifest()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_get_version_info(url: String) -> Result<versions::VersionInfo, String> {
    if !is_allowed_download_host(&url) {
        return Err("Access denied: download host not allowed".to_string());
    }
    versions::fetch_version_info(&url)
        .await
        .map_err(|e| e.to_string())
}

// ==================== Java Commands ====================

#[tauri::command]
pub fn cmd_detect_java(state: State<'_, AppState>) -> Result<Vec<java::JavaInstallation>, String> {
    let c = state.config.lock().map_err(|e| e.to_string())?;
    let data_dir = c.data_dir.clone();
    drop(c);
    let mut installations = java::detect_java_installations();
    installations.extend(
        java_download::list_managed_java(&data_dir)
            .into_iter()
            .map(|m| java::JavaInstallation {
                path: m.path,
                version: m.version,
                major_version: m.major_version,
                is_64bit: m.is_64bit,
                vendor: m.vendor,
            }),
    );
    Ok(installations)
}

#[tauri::command]
pub async fn cmd_list_available_java() -> Result<Vec<java_download::AvailableJavaVersion>, String> {
    java_download::list_available_java_versions()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_download_java(
    state: State<'_, AppState>,
    app: AppHandle,
    major_version: u32,
) -> Result<java_download::ManagedJavaRuntime, String> {
    let data_dir = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.data_dir.clone()
    };
    java_download::download_java_runtime(major_version, &data_dir, &app)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_list_managed_java(state: State<'_, AppState>) -> Vec<java_download::ManagedJavaRuntime> {
    let data_dir = {
        let c = state.config.lock().map_err(|e| e.to_string());
        match c {
            Ok(cfg) => cfg.data_dir.clone(),
            Err(_) => return Vec::new(),
        }
    };
    java_download::list_managed_java(&data_dir)
}

#[tauri::command]
pub fn cmd_remove_managed_java(state: State<'_, AppState>, major_version: u32) -> Result<(), String> {
    let data_dir = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.data_dir.clone()
    };
    java_download::remove_managed_java(major_version, &data_dir).map_err(|e| e.to_string())
}

//! Modpack catalog commands: ATLauncher / CurseForge / Modrinth browsing + install.

use crate::atlauncher;
use crate::commands::instances::validate_instance_name;
use crate::curseforge;
use crate::modrinth;
use crate::AppState;
use tauri::{AppHandle, State};

// ==================== ATLauncher ====================

#[tauri::command]
pub fn cmd_pause_modpack_install() {
    crate::download::request_pause();
}

#[tauri::command]
pub fn cmd_resume_modpack_install() {
    crate::download::clear_pause();
}

#[tauri::command]
pub async fn cmd_search_atlauncher() -> Result<Vec<atlauncher::AtModpackEntry>, String> {
    let packs = atlauncher::fetch_packs().await.map_err(|e| e.to_string())?;
    let entries = packs
        .into_iter()
        .map(|p| {
            let safe = p.safe_name();
            atlauncher::AtModpackEntry {
                id: p.id,
                name: p.name,
                safe_name: safe,
                description: p.description,
                icon: p.icon,
                versions: p.versions,
            }
        })
        .collect();
    Ok(entries)
}

#[tauri::command]
pub async fn cmd_get_atlauncher_pack_versions(
    pack_id: u64,
) -> Result<atlauncher::AtModpackEntry, String> {
    let packs = atlauncher::fetch_packs().await.map_err(|e| e.to_string())?;
    let pack = packs
        .into_iter()
        .find(|p| p.id == pack_id)
        .ok_or_else(|| format!("Pack {} not found", pack_id))?;
    let safe = pack.safe_name();
    Ok(atlauncher::AtModpackEntry {
        id: pack.id,
        name: pack.name,
        safe_name: safe,
        description: pack.description,
        icon: pack.icon,
        versions: pack.versions,
    })
}

#[tauri::command]
pub async fn cmd_get_atlauncher_version_detail(
    safe_name: String,
    version: String,
) -> Result<atlauncher::AtVersionDetailEntry, String> {
    let detail = atlauncher::fetch_version_detail(&safe_name, &version)
        .await
        .map_err(|e| e.to_string())?;
    let loader = detail.loader.as_ref().map(|l| l.type_.clone());
    let loader_version = detail
        .loader
        .as_ref()
        .and_then(|l| l.metadata.as_ref())
        .and_then(|m| {
            m.version
                .as_deref()
                .or(m.loader.as_deref())
                .map(|s| s.to_string())
        });
    Ok(atlauncher::AtVersionDetailEntry {
        version: detail.version,
        minecraft: detail.minecraft,
        loader,
        loader_version,
        mods: detail.mods,
        has_configs: detail
            .configs
            .as_ref()
            .map(|c| c.sha1.as_deref().map(|s| !s.is_empty()).unwrap_or(false))
            .unwrap_or(false),
    })
}

#[tauri::command]
pub async fn cmd_install_atlauncher_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    pack_id: u64,
    version: String,
    instance_name: String,
) -> Result<crate::instances::Instance, String> {
    validate_instance_name(&instance_name)?;
    let (instances_dir, libraries_dir, versions_dir) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        (
            config.instances_dir().clone(),
            config.libraries_dir(),
            config.versions_dir(),
        )
    };
    atlauncher::install_atlauncher_pack(
        &instances_dir,
        &libraries_dir,
        &versions_dir,
        pack_id,
        &version,
        &instance_name,
        Some(&app),
    )
    .await
    .map_err(|e| e.to_string())
}

// ==================== CurseForge ====================

const CF_MODPACK_CLASS_ID: u32 = 4471;

#[tauri::command]
pub async fn cmd_search_curseforge_modpacks(
    state: State<'_, AppState>,
    query: String,
    mc_version: Option<String>,
    loader: Option<String>,
    offset: u32,
    limit: u32,
) -> Result<curseforge::CfSearchResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::search_mods_filtered(
        &query,
        mc_version.as_deref(),
        loader.as_deref(),
        Some(CF_MODPACK_CLASS_ID),
        None,
        offset,
        limit,
        &api_key,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_get_curseforge_modpack_files(
    state: State<'_, AppState>,
    mod_id: u64,
    mc_version: Option<String>,
    loader: Option<String>,
) -> Result<curseforge::CfFilesResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::get_mod_files(mod_id, mc_version.as_deref(), loader.as_deref(), &api_key)
        .await
        .map_err(|e| e.to_string())
}

/// Fallback CurseForge CDN URL: edge.forgecdn.net/files/{id/1000}/{id%1000}/{name}.
fn forgecdn_fallback_url(file_id: u64, file_name: &str) -> String {
    format!(
        "https://edge.forgecdn.net/files/{}/{}/{}",
        file_id / 1000,
        file_id % 1000,
        file_name
    )
}

#[tauri::command]
pub async fn cmd_install_curseforge_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    mod_id: u64,
    file_id: u64,
    instance_name: String,
) -> Result<crate::instances::Instance, String> {
    validate_instance_name(&instance_name)?;
    let (instances_dir, api_key, libraries_dir) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        (
            config.instances_dir().clone(),
            config.curseforge_api_key.clone(),
            config.libraries_dir(),
        )
    };
    let file = curseforge::get_mod_file(mod_id, file_id, &api_key)
        .await
        .map_err(|e| e.to_string())?;
    let download_url = file
        .download_url
        .clone()
        .unwrap_or_else(|| forgecdn_fallback_url(file_id, &file.file_name));
    if !download_url.starts_with("https://") {
        return Err("Download URL must be HTTPS.".to_string());
    }
    if !crate::is_allowed_download_host(&download_url) {
        return Err(format!(
            "Download host is not in the allowlist: {}",
            download_url
        ));
    }
    let safe_instance = instance_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect::<String>();
    let temp_path = std::env::temp_dir()
        .join(format!("voidlauncher-cf-{}-{}.zip", safe_instance, file_id));
    let expected_sha1 = file.sha1_hash().unwrap_or("");
    crate::download::download_file_sized(&download_url, &temp_path, expected_sha1, file.file_length)
        .await
        .map_err(|e| e.to_string())?;

    let result = crate::import::import_curseforge_pack(
        &instances_dir,
        &temp_path.to_string_lossy(),
        &instance_name,
        &api_key,
        &libraries_dir,
        Some(&app),
    )
    .await;
    let _ = std::fs::remove_file(&temp_path);
    result.map_err(|e| e.to_string())
}

// ==================== Modrinth ====================

#[tauri::command]
pub async fn cmd_search_modrinth_modpacks(
    query: String,
    mc_version: Option<String>,
    loader: Option<String>,
    index: Option<String>,
    offset: u32,
    limit: u32,
) -> Result<modrinth::ModrinthSearchResponse, String> {
    modrinth::search_mods(
        &query,
        "modpack",
        mc_version.as_deref(),
        loader.as_deref(),
        index.as_deref(),
        offset,
        limit,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_get_modrinth_modpack_versions(
    project_id: String,
) -> Result<Vec<modrinth::ModrinthVersion>, String> {
    modrinth::get_versions(&project_id, None, None)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_install_modrinth_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    version_id: String,
    instance_name: String,
) -> Result<crate::instances::Instance, String> {
    validate_instance_name(&instance_name)?;
    let instances_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.instances_dir().clone()
    };
    let version = modrinth::get_version_by_id(&version_id)
        .await
        .map_err(|e| e.to_string())?;
    let file = version
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| version.files.first())
        .ok_or("Modrinth version has no files")?;
    if !file.url.starts_with("https://") {
        return Err("Download URL must be HTTPS.".to_string());
    }
    if !crate::is_allowed_download_host(&file.url) {
        return Err(format!(
            "Download host is not in the allowlist: {}",
            file.url
        ));
    }
    let safe_instance = instance_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect::<String>();
    let temp_path = std::env::temp_dir()
        .join(format!("voidlauncher-mr-{}-{}.mrpack", safe_instance, version_id));
    let expected_sha1 = file.hashes.get("sha1").map(|s| s.as_str()).unwrap_or("");
    crate::download::download_file_sized(&file.url, &temp_path, expected_sha1, file.size)
        .await
        .map_err(|e| e.to_string())?;

    let result = crate::import::import_mrpack(
        &instances_dir,
        &temp_path.to_string_lossy(),
        &instance_name,
        Some(&app),
    )
    .await;
    let _ = std::fs::remove_file(&temp_path);
    result.map_err(|e| e.to_string())
}
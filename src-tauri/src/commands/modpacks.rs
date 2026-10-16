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
    index: Option<String>,
    offset: u32,
    limit: u32,
) -> Result<curseforge::CfSearchResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::search_mods_filtered(
        &curseforge::CfSearchQuery {
            query: &query,
            mc_version: mc_version.as_deref(),
            loader: loader.as_deref(),
            class_id: Some(CF_MODPACK_CLASS_ID),
            category_id: None,
            index: index.as_deref(),
            offset,
            limit,
        },
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
    curseforge::get_mod_files(
        mod_id,
        mc_version.as_deref(),
        loader.as_deref(),
        0,
        &api_key,
    )
    .await
    .map_err(|e| e.to_string())
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
    let safe_instance = instance_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect::<String>();
    let temp_path =
        std::env::temp_dir().join(format!("voidlauncher-cf-{}-{}.zip", safe_instance, file_id));
    curseforge::download_mod_file(mod_id, file_id, &file, &api_key, &temp_path)
        .await
        .map_err(|e| e.to_string())?;

    // Pack metadata for the update lock — read while the temp zip exists.
    let (manifest, overrides) = match crate::packlock::pack_zip_info(&temp_path) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(e);
        }
    };

    let instance = match crate::import::import_curseforge_pack(
        &instances_dir,
        &temp_path.to_string_lossy(),
        &instance_name,
        &api_key,
        &libraries_dir,
        Some(&app),
    )
    .await
    {
        Ok(i) => i,
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(e.to_string());
        }
    };

    // Track the pack so update/repair can find it later. Best-effort: a
    // failed lock write leaves an untracked but fully usable instance.
    let pack_name = match curseforge::get_mod(mod_id, &api_key).await {
        Ok(p) => p.name,
        Err(_) => file.file_name.clone(),
    };
    let lock = crate::packlock::PackLock {
        provider: "curseforge".to_string(),
        pack_project_id: mod_id.to_string(),
        pack_name,
        file_id: file_id.to_string(),
        file_name: file.file_name.clone(),
        file_display_name: file.display_name.clone(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        mc_version: instance.mc_version.clone(),
        loader: crate::packlock::loader_slug(&instance.loader).to_string(),
        manifest,
        overrides,
    };
    let _ = lock.write(&instances_dir.join(&instance_name));
    let _ = std::fs::remove_file(&temp_path);
    Ok(instance)
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
    let temp_path = std::env::temp_dir().join(format!(
        "voidlauncher-mr-{}-{}.mrpack",
        safe_instance, version_id
    ));
    let expected_sha1 = file.hashes.get("sha1").map(|s| s.as_str()).unwrap_or("");
    crate::download::download_file_sized(&file.url, &temp_path, expected_sha1, file.size)
        .await
        .map_err(|e| e.to_string())?;

    // Pack metadata for the update lock — read while the temp zip exists.
    let (manifest, overrides) = match crate::packlock::mrpack_zip_info(&temp_path) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(e);
        }
    };

    let result = crate::import::import_mrpack(
        &instances_dir,
        &temp_path.to_string_lossy(),
        &instance_name,
        Some(&app),
    )
    .await;
    if let Ok(instance) = &result {
        // Track the pack so update/repair can find it later. Best-effort: a
        // failed lock write leaves an untracked but fully usable instance.
        let pack_name = match modrinth::get_project(&version.project_id).await {
            Ok(p) => p.title,
            Err(_) => version.name.clone(),
        };
        let lock = crate::packlock::PackLock {
            provider: "modrinth".to_string(),
            pack_project_id: version.project_id.clone(),
            pack_name,
            file_id: version.id.clone(),
            file_name: file.filename.clone(),
            file_display_name: version.version_number.clone(),
            installed_at: chrono::Utc::now().to_rfc3339(),
            mc_version: instance.mc_version.clone(),
            loader: crate::packlock::loader_slug(&instance.loader).to_string(),
            manifest,
            overrides,
        };
        let _ = lock.write(&instances_dir.join(&instance_name));
    }
    let _ = std::fs::remove_file(&temp_path);
    result.map_err(|e| e.to_string())
}

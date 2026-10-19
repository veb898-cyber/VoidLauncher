//! Mod & resource-pack commands: Modrinth / CurseForge API wrappers,
//! mod installation/download, local mod metadata parsing, mod icons.

use crate::commands::instances::validate_instance_name;
use crate::config::AppConfig;
use crate::content;
use crate::curseforge;
use crate::download;
use crate::instances;
use crate::is_allowed_download_host;
use crate::modrinth;
use crate::AppState;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, State};

// ==================== Modrinth API ====================

#[tauri::command]
pub async fn cmd_search_modrinth(
    query: String,
    project_type: String,
    mc_version: Option<String>,
    loader: Option<String>,
    index: Option<String>,
    offset: u32,
    limit: u32,
) -> Result<modrinth::ModrinthSearchResponse, String> {
    modrinth::search_mods(
        &query,
        &project_type,
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
pub async fn cmd_get_modrinth_versions(
    project_id: String,
    mc_version: Option<String>,
    loader: Option<String>,
) -> Result<Vec<modrinth::ModrinthVersion>, String> {
    modrinth::get_versions(&project_id, mc_version.as_deref(), loader.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_get_modrinth_project(id: String) -> Result<modrinth::ModrinthProject, String> {
    modrinth::get_project(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_get_modrinth_version_by_id(
    version_id: String,
) -> Result<modrinth::ModrinthVersionResponse, String> {
    modrinth::get_version_by_id(&version_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_get_modrinth_project_body(id: String) -> Result<String, String> {
    let project = modrinth::get_project(&id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(project.body.unwrap_or_default())
}

#[derive(Debug, serde::Serialize)]
pub struct ModUpdateResult {
    filename: String,
    name: String,
    old_version: String,
    new_version: String,
    download_url: String,
    new_filename: String,
    project_id: String,
    version_id: String,
    expected_sha1: String,
    /// `modrinth` (default) or `curseforge` — which provider to query when
    /// the frontend applies the update (CF files may need the id chain).
    #[serde(default)]
    provider: String,
    /// CurseForge file id of the new version (CF updates are identified by
    /// project id + file id, not by URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_id: Option<String>,
}

struct ModFile {
    filename: String,
    name: String,
    version: String,
    hash: Option<String>,
    /// Sidecar identity: which provider installed the file and under
    /// which ids. `unknown` files stay in the Modrinth hash pass (the
    /// pre-CF-integration behaviour for sidecar-less drops).
    provider: String,
    project_id: Option<String>,
    file_id: Option<String>,
}

struct ModScan {
    mod_files: Vec<ModFile>,
    hash_to_mod: std::collections::HashMap<String, usize>,
    all_hashes: Vec<String>,
}

/// Synchronous directory scan + JAR/zip metadata and SHA1 hashing. Runs on
/// the blocking pool: with hundreds of mods this can take seconds and would
/// otherwise freeze every other async command (UI included).
fn scan_mod_files(
    packs_dir: &std::path::Path,
    is_mods: bool,
    loader: Option<&str>,
    content_type: &str,
) -> ModScan {
    let mut mod_files: Vec<ModFile> = Vec::new();
    let mut hash_to_mod: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut all_hashes: Vec<String> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(packs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if filename.starts_with('.')
                || filename.ends_with(".voidlauncher.json")
                || filename.ends_with(".pw.toml")
            {
                continue;
            }

            if is_mods {
                // Mods: only .jar files, with loader filtering
                let is_jar = filename.ends_with(".jar") || filename.ends_with(".jar.disabled");
                if !is_jar {
                    continue;
                }

                let meta = read_mod_meta_from_jar(&path);
                let matches_loader = match loader {
                    Some("Fabric") => meta.provider == "Fabric" || meta.provider == "Local",
                    Some("Forge") => meta.provider == "Forge" || meta.provider == "Local",
                    Some("NeoForge") => meta.provider == "NeoForge" || meta.provider == "Local",
                    Some("Vanilla") => false,
                    _ => true,
                };
                if !matches_loader {
                    continue;
                }

                let ic = content::InstalledContent::read(packs_dir, &filename, content_type);
                let hash = if ic.provider == "curseforge" {
                    ic.hash
                        .clone()
                        .or_else(|| download::hash_file_sha1(&path).ok())
                } else {
                    download::hash_file_sha1(&path).ok()
                };

                // CurseForge files update through the id chain, not the
                // Modrinth hash index — keep them out of the hash pass.
                if ic.provider != "curseforge" {
                    if let Some(ref h) = hash {
                        let idx = mod_files.len();
                        hash_to_mod.insert(h.clone(), idx);
                        all_hashes.push(h.clone());
                    }
                }

                let jar_name = instances::strip_minecraft_color_codes(&meta.name);
                let name = if usable_mod_name(&jar_name, meta.slug.as_deref()).is_some() {
                    jar_name
                } else {
                    ic.project_name
                        .as_deref()
                        .and_then(|n| usable_mod_name(n, meta.slug.as_deref()))
                        .map(instances::strip_minecraft_color_codes)
                        .unwrap_or(jar_name)
                };

                mod_files.push(ModFile {
                    filename,
                    name,
                    version: meta.version,
                    hash,
                    provider: ic.provider,
                    project_id: ic.project_id,
                    file_id: ic.file_id,
                });
            } else {
                // Resourcepacks / Shaderpacks: only .zip files (skip directories)
                let is_zip = filename.ends_with(".zip") || filename.ends_with(".zip.disabled");
                if !is_zip {
                    continue;
                }

                let (name, version) = read_pack_name_and_version(&path).unwrap_or_else(|| {
                    (
                        instances::strip_minecraft_color_codes(
                            std::path::Path::new(&filename)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or(&filename),
                        ),
                        String::new(),
                    )
                });

                let ic = content::InstalledContent::read(packs_dir, &filename, content_type);
                let hash = if ic.provider == "curseforge" {
                    ic.hash
                        .clone()
                        .or_else(|| download::hash_file_sha1(&path).ok())
                } else {
                    download::hash_file_sha1(&path).ok()
                };

                if ic.provider != "curseforge" {
                    if let Some(ref h) = hash {
                        let idx = mod_files.len();
                        hash_to_mod.insert(h.clone(), idx);
                        all_hashes.push(h.clone());
                    }
                }

                mod_files.push(ModFile {
                    filename,
                    name,
                    version,
                    hash,
                    provider: ic.provider,
                    project_id: ic.project_id,
                    file_id: ic.file_id,
                });
            }
        }
    }

    ModScan {
        mod_files,
        hash_to_mod,
        all_hashes,
    }
}

const CF_BULK_LIMIT: usize = 50;

struct CfPendingUpdate {
    mod_index: usize,
    project_id: u64,
    project_name: String,
    file_id: u64,
}

fn cf_index_matches(
    index: &curseforge::CfFileIndex,
    mc_version: Option<&str>,
    loader_id: Option<u32>,
) -> bool {
    if let Some(v) = mc_version {
        if index.game_version != v {
            return false;
        }
    }
    match loader_id {
        None => true,
        Some(want) => match index.mod_loader {
            None | Some(0) => true,
            Some(id) => id == want,
        },
    }
}

fn cf_pick_update_index<'a>(
    mod_data: &'a curseforge::CfBulkMod,
    mc_version: Option<&str>,
    loader_id: Option<u32>,
    installed_file_id: Option<u64>,
) -> Option<&'a curseforge::CfFileIndex> {
    let compatible: Vec<&curseforge::CfFileIndex> = mod_data
        .latest_files_indexes
        .iter()
        .filter(|i| i.file_id != 0 && cf_index_matches(i, mc_version, loader_id))
        .collect();
    let installed_pos =
        installed_file_id.and_then(|fid| compatible.iter().position(|index| index.file_id == fid));
    if installed_pos == Some(0) {
        return None;
    }
    let newer: Vec<(usize, &curseforge::CfFileIndex)> = compatible
        .iter()
        .copied()
        .enumerate()
        .filter(|(pos, _)| installed_pos.is_none_or(|installed| *pos < installed))
        .collect();
    if let Some((_, index)) = newer.iter().find(|(_, i)| i.release_type == 1) {
        return Some(*index);
    }
    if compatible.iter().any(|i| i.release_type == 1) {
        return None;
    }
    newer.first().map(|(_, index)| *index)
}

fn cf_pick_latest_file<'a>(
    mod_data: &'a curseforge::CfBulkMod,
    mc_version: Option<&str>,
    loader: Option<&str>,
) -> Option<&'a curseforge::CfFile> {
    mod_data
        .latest_files
        .iter()
        .filter(|f| f.is_available.unwrap_or(true))
        .filter(|f| mc_version.is_none_or(|v| f.game_versions.iter().any(|g| g == v)))
        .filter(|f| {
            loader.is_none_or(|wanted| {
                f.game_versions
                    .iter()
                    .any(|g| g.eq_ignore_ascii_case(wanted))
            })
        })
        .max_by(|a, b| a.file_date.cmp(&b.file_date))
}

#[tauri::command]
pub async fn cmd_check_mod_updates(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
    mc_version: Option<String>,
    loader: Option<String>,
    pack_type: String,
) -> Result<Vec<ModUpdateResult>, String> {
    validate_instance_name(&instance_name)?;

    let packs_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        instance
            .minecraft_dir(&config.instances_dir())
            .join(&pack_type)
    };

    if !packs_dir.exists() {
        return Ok(Vec::new());
    }

    let is_mods = pack_type == "mods";
    let content_type: &'static str = if is_mods {
        "mod"
    } else if pack_type == "resourcepacks" {
        "resourcepack"
    } else {
        "shader"
    };

    let loader_for_scan = loader.clone();
    let scan = tokio::task::spawn_blocking(move || {
        scan_mod_files(
            &packs_dir,
            is_mods,
            loader_for_scan.as_deref(),
            content_type,
        )
    })
    .await
    .map_err(|e| format!("Update check scan failed: {}", e))?;
    let ModScan {
        mod_files,
        hash_to_mod,
        all_hashes,
    } = scan;

    let mut results = Vec::new();

    let cf_indices: Vec<usize> = mod_files
        .iter()
        .enumerate()
        .filter(|(_, m)| m.provider == "curseforge" && m.project_id.is_some())
        .map(|(i, _)| i)
        .collect();
    if !cf_indices.is_empty() {
        let cf_loader_id: Option<Option<u32>> = if is_mods {
            match loader.as_deref() {
                None | Some("Vanilla") => None,
                Some(l) => Some(curseforge::loader_type_id(l)),
            }
        } else {
            Some(None)
        };

        if let Some(cf_loader_id) = cf_loader_id {
            let cf_key = {
                let config = state.config.lock().map_err(|e| e.to_string())?;
                config.curseforge_api_key.clone()
            };

            let mut cf_entries: Vec<(usize, u64)> = Vec::new();
            for &idx in &cf_indices {
                if let Ok(pid) = mod_files[idx]
                    .project_id
                    .as_deref()
                    .unwrap_or("")
                    .parse::<u64>()
                {
                    cf_entries.push((idx, pid));
                }
            }

            let mut project_ids: Vec<u64> = cf_entries.iter().map(|(_, pid)| *pid).collect();
            project_ids.sort_unstable();
            project_ids.dedup();

            let mut mods_by_id: std::collections::HashMap<u64, curseforge::CfBulkMod> =
                std::collections::HashMap::new();
            let mut failed_chunks = 0;
            for chunk in project_ids.chunks(CF_BULK_LIMIT) {
                match curseforge::get_mods_by_ids(chunk, &cf_key).await {
                    Ok(mods) => {
                        failed_chunks = 0;
                        for m in mods {
                            mods_by_id.insert(m.id, m);
                        }
                    }
                    Err(e) => {
                        failed_chunks += 1;
                        let msg = format!(
                            "Update check: CurseForge bulk request failed for {} mod(s): {}",
                            chunk.len(),
                            e
                        );
                        tracing::warn!(target: "launcher", "{}", msg);
                        crate::events::emit_log(&app, "warn", "content", &msg);
                        if failed_chunks >= 2 {
                            break;
                        }
                    }
                }
            }

            let mut pending: Vec<CfPendingUpdate> = Vec::new();
            for (idx, pid) in cf_entries {
                let m = &mod_files[idx];
                let Some(mod_data) = mods_by_id.get(&pid) else {
                    continue;
                };
                let installed_file_id = m.file_id.as_deref().and_then(|s| s.parse::<u64>().ok());
                let candidate = cf_pick_update_index(
                    mod_data,
                    mc_version.as_deref(),
                    cf_loader_id,
                    installed_file_id,
                )
                .map(|index| index.file_id)
                .or_else(|| {
                    cf_pick_latest_file(mod_data, mc_version.as_deref(), loader.as_deref())
                        .map(|file| file.id)
                });
                let Some(file_id) = candidate else {
                    continue;
                };
                if installed_file_id == Some(file_id) {
                    continue;
                }
                pending.push(CfPendingUpdate {
                    mod_index: idx,
                    project_id: pid,
                    project_name: if mod_data.name.trim().is_empty() {
                        m.name.clone()
                    } else {
                        mod_data.name.clone()
                    },
                    file_id,
                });
            }

            let mut file_ids: Vec<u64> = pending.iter().map(|p| p.file_id).collect();
            file_ids.sort_unstable();
            file_ids.dedup();
            let mut files_by_id: std::collections::HashMap<u64, curseforge::CfFile> =
                std::collections::HashMap::new();
            let mut failed_file_chunks = 0;
            for chunk in file_ids.chunks(CF_BULK_LIMIT) {
                match curseforge::get_files_by_ids(chunk, &cf_key).await {
                    Ok(files) => {
                        failed_file_chunks = 0;
                        for f in files {
                            files_by_id.insert(f.id, f);
                        }
                    }
                    Err(e) => {
                        failed_file_chunks += 1;
                        let msg = format!(
                            "Update check: CurseForge file details request failed for {} file(s): {}",
                            chunk.len(),
                            e
                        );
                        tracing::warn!(target: "launcher", "{}", msg);
                        crate::events::emit_log(&app, "warn", "content", &msg);
                        if failed_file_chunks >= 2 {
                            break;
                        }
                    }
                }
            }

            for p in pending {
                let m = &mod_files[p.mod_index];
                let Some(new_file) = files_by_id.get(&p.file_id) else {
                    continue;
                };
                if !new_file.is_available.unwrap_or(true) {
                    continue;
                }
                let new_fid = new_file.id.to_string();
                if m.file_id.as_deref() == Some(new_fid.as_str()) {
                    continue;
                }
                let new_sha1 = new_file.sha1_hash().unwrap_or("");
                if !new_sha1.is_empty() && m.hash.as_deref() == Some(new_sha1) {
                    continue;
                }
                results.push(ModUpdateResult {
                    filename: m.filename.clone(),
                    name: p.project_name,
                    old_version: m.version.clone(),
                    new_version: new_file.display_name.clone(),
                    download_url: new_file.download_url.clone().unwrap_or_default(),
                    new_filename: new_file.file_name.clone(),
                    project_id: p.project_id.to_string(),
                    version_id: new_fid.clone(),
                    expected_sha1: new_sha1.to_string(),
                    provider: "curseforge".to_string(),
                    file_id: Some(new_fid),
                });
            }
        }
    }

    // ---- Modrinth pass: hash-based lookup for everything else ----
    if !all_hashes.is_empty() {
        // Query Modrinth update API
        // For mods: pass loader filter; for packs: no loader filter
        let loaders = if is_mods {
            loader.map(|l| vec![l.to_lowercase()])
        } else {
            None
        };
        let game_versions = mc_version.map(|v| vec![v]);

        let response = modrinth::check_version_updates(all_hashes, "sha1", loaders, game_versions)
            .await
            .map_err(|e| e.to_string())?;

        for (hash_str, maybe_version) in &response {
            let Some(version) = maybe_version else {
                continue;
            };
            let Some(&idx) = hash_to_mod.get(hash_str) else {
                continue;
            };
            let mod_file = &mod_files[idx];

            let file = version
                .files
                .iter()
                .find(|f| f.primary)
                .or_else(|| version.files.first());
            let Some(primary_file) = file else {
                continue;
            };

            let installed_hash = mod_file.hash.as_deref().unwrap_or("");
            // Compare only like-for-like: sha512 used to be substituted for
            // the missing sha1, so the comparison never matched and every
            // check reported a phantom update. No sha1 → report the update
            // without a hash guard (expected_sha1 empty = no verification).
            let file_hash = primary_file
                .hashes
                .get("sha1")
                .map(|s| s.as_str())
                .unwrap_or("");

            if !file_hash.is_empty() && installed_hash == file_hash {
                continue;
            }

            results.push(ModUpdateResult {
                filename: mod_file.filename.clone(),
                name: mod_file.name.clone(),
                old_version: mod_file.version.clone(),
                new_version: version.version_number.clone(),
                download_url: primary_file.url.clone(),
                new_filename: primary_file.filename.clone(),
                project_id: version.project_id.clone(),
                version_id: version.id.clone(),
                expected_sha1: file_hash.to_string(),
                provider: "modrinth".to_string(),
                file_id: None,
            });
        }
    }

    Ok(results)
}

/// Read pack name and version from sidecar metadata, falling back to pack.mcmeta
fn read_pack_name_and_version(path: &std::path::Path) -> Option<(String, String)> {
    // Try sidecar first (`.index/`, legacy stem or legacy full-filename layouts)
    let filename = path.file_name()?.to_string_lossy().to_string();
    if let Some(val) = crate::instances::read_sidecar_meta(path.parent()?, &filename) {
        let name = val["project_name"].as_str().unwrap_or("").to_string();
        let version = val["version_number"].as_str().unwrap_or("").to_string();
        if !name.is_empty() {
            return Some((name, version));
        }
    }

    // Fallback to Prism packwiz metadata (.pw.toml in .index/)
    if let Some(pw) = crate::instances::read_packwiz_meta(path.parent()?, &filename) {
        if !pw.name.is_empty() {
            return Some((pw.name, pw.version));
        }
    }

    // Fallback to pack.mcmeta (name only, no version in standard format)
    if let Ok(file) = std::fs::File::open(path) {
        if let Ok(mut archive) = zip::ZipArchive::new(file) {
            if let Ok(mcmeta_file) = archive.by_name("pack.mcmeta") {
                let bytes: Vec<u8> = std::io::BufReader::new(mcmeta_file)
                    .bytes()
                    .filter_map(|b| b.ok())
                    .collect();
                if let Ok(content) = String::from_utf8(bytes) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(name) = json["pack"]
                            .as_object()
                            .and_then(|p| p.get("description"))
                            .and_then(|d| d.as_str())
                            .or_else(|| {
                                json["pack"]
                                    .as_object()
                                    .and_then(|p| p.get("name"))
                                    .and_then(|n| n.as_str())
                            })
                        {
                            return Some((
                                instances::strip_minecraft_color_codes(name),
                                String::new(),
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

#[tauri::command]
pub async fn cmd_popular_modrinth(
    project_type: String,
    mc_version: Option<String>,
    loader: Option<String>,
    index: Option<String>,
    limit: u32,
    offset: u32,
) -> Result<modrinth::ModrinthSearchResponse, String> {
    modrinth::popular_mods(
        &project_type,
        mc_version.as_deref(),
        loader.as_deref(),
        index.as_deref(),
        limit,
        offset,
    )
    .await
    .map_err(|e| e.to_string())
}

// ==================== CurseForge API ====================

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_search_curseforge(
    state: State<'_, AppState>,
    query: String,
    mc_version: Option<String>,
    loader: Option<String>,
    project_type: Option<String>,
    index: Option<String>,
    offset: u32,
    limit: u32,
) -> Result<curseforge::CfSearchResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    let class_id = project_type
        .as_deref()
        .and_then(curseforge::class_id_for_project_type);
    curseforge::search_mods_filtered(
        &curseforge::CfSearchQuery {
            query: &query,
            mc_version: mc_version.as_deref(),
            loader: loader.as_deref(),
            class_id,
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
pub async fn cmd_get_curseforge_files(
    state: State<'_, AppState>,
    mod_id: u64,
    mc_version: Option<String>,
    loader: Option<String>,
    offset: Option<u32>,
) -> Result<curseforge::CfFilesResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::get_mod_files(
        mod_id,
        mc_version.as_deref(),
        loader.as_deref(),
        offset.unwrap_or(0),
        &api_key,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_get_curseforge_mod_detail(
    state: State<'_, AppState>,
    mod_id: u64,
) -> Result<curseforge::CfModDetail, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::get_mod(mod_id, &api_key)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_popular_curseforge(
    state: State<'_, AppState>,
    mc_version: Option<String>,
    loader: Option<String>,
    limit: u32,
) -> Result<curseforge::CfSearchResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::popular_mods(mc_version.as_deref(), loader.as_deref(), limit, &api_key)
        .await
        .map_err(|e| e.to_string())
}

// ==================== Mod Installation ====================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModMetadata {
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub file_size: u64,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    /// `true` when the slug came from the `.voidlauncher.json` sidecar
    /// (i.e. the mod was installed via the launcher and the slug is the
    /// verified Modrinth/CurseForge project ID). `false` when the slug is
    /// derived from the jar's internal metadata (e.g. `fabric.mod.json`
    /// `id`), which is the mod's *internal* identifier and may NOT match
    /// the Modrinth project slug — causing false-positive "incompatible"
    /// warnings in the compatibility check.
    #[serde(default)]
    pub slug_verified: bool,
}

/// All sidecar fields `get_mod_metadata` needs, read from the sidecar file
/// in ONE pass (the old code re-read + re-parsed the same JSON up to 4 times
/// per mod).
#[derive(Default)]
struct SidecarFields {
    project_id: Option<String>,
    provider: Option<String>,
    project_name: Option<String>,
    version_number: Option<String>,
}

fn read_sidecar_fields(mods_dir: &std::path::Path, filename: &str) -> SidecarFields {
    match crate::instances::read_sidecar_meta(mods_dir, filename) {
        Some(json) => SidecarFields {
            project_id: json["project_id"].as_str().map(str::to_string),
            provider: json["provider"].as_str().map(str::to_string),
            project_name: json["project_name"].as_str().map(str::to_string),
            version_number: json["version_number"].as_str().map(str::to_string),
        },
        None => SidecarFields::default(),
    }
}

/// Extract a local filesystem path from a `file://` URI (as passed by the UI
/// for local mod installation, e.g. `file://C:\path\to\mod.jar`). Accepts the
/// `file:///C:/...` variant too. Returns an error for anything not file-based.
fn local_path_from_uri(uri: &str) -> Result<std::path::PathBuf, String> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| "Local install requires a file:// path".to_string())?
        .trim_start_matches('/');
    if rest.is_empty() || rest == "." || rest == ".." {
        return Err("Empty or invalid local file path".to_string());
    }
    Ok(std::path::PathBuf::from(rest))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_install_mod(
    state: State<'_, AppState>,
    instance_name: String,
    modrinth_version_id: Option<String>,
    curseforge_file_id: Option<i32>,
    file_name: String,
    download_url: String,
    project_id: Option<String>,
    project_name: Option<String>,
    version_number: Option<String>,
    provider: String,
    subfolder: Option<String>,
) -> Result<String, String> {
    validate_instance_name(&instance_name)?;

    // Content subfolder: mods (default), resourcepacks, shaderpacks.
    // Local drag-drop / file picker for packs must land in the right folder
    // so list_packs finds the file and its sidecar.
    const ALLOWED: &[&str] = &["mods", "resourcepacks", "shaderpacks"];
    let safe_subfolder = subfolder
        .unwrap_or_else(|| "mods".to_string())
        .trim_matches('/')
        .trim_matches('\\')
        .to_string();
    if !ALLOWED
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&safe_subfolder))
    {
        return Err(format!("Subfolder '{}' is not allowed.", safe_subfolder));
    }
    let safe_subfolder = safe_subfolder.to_lowercase();

    let (content_dir, safe_name, dest) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        let content_dir = instance
            .minecraft_dir(&config.instances_dir())
            .join(&safe_subfolder);
        let _ = std::fs::create_dir_all(&content_dir);
        let safe_name = std::path::Path::new(&file_name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid file name")?
            .to_string();
        let dest = content_dir.join(&safe_name);
        (content_dir, safe_name, dest)
    };

    // Local file installation: the UI passes a `file://` URI in `download_url`
    // and `provider: "local"`. Copy the file into the content dir instead of
    // downloading it — the HTTPS/host allowlist checks below only apply to
    // remote downloads.
    if provider == "local" {
        let local_path = local_path_from_uri(&download_url)?;
        if !local_path.is_file() {
            return Err(format!("Local file not found: {}", local_path.display()));
        }
        std::fs::copy(&local_path, &dest).map_err(|e| e.to_string())?;
        // Reject non-archives (a picked .txt renamed to .jar, a truncated
        // file) and do not leave the bad copy behind: it would show up in
        // the mods list as a broken entry forever.
        if let Err(e) = download::verify_zip_magic(&dest) {
            let _ = std::fs::remove_file(&dest);
            return Err(e.to_string());
        }
        // Unified sidecar so the Source column shows Local instead of a
        // loader name from jar metadata (no project — nothing to update).
        let hash = download::hash_file_sha1(&dest).ok();
        let _ = content::write_sidecar(
            &content_dir,
            &safe_name,
            &content::SidecarWrite {
                provider: "local",
                content_type: content::content_type_for_dir(&safe_subfolder),
                project_id: None,
                project_name: None,
                version_id: None,
                version_number: None,
                file_id: None,
                hash: hash.as_deref(),
            },
        );
        return Ok(safe_name);
    }

    // Validate the URL is HTTPS and points to a known Modrinth / CurseForge CDN.
    if !download_url.starts_with("https://") {
        return Err("Download URL must be HTTPS.".to_string());
    }
    if !is_allowed_download_host(&download_url) {
        return Err(format!(
            "Download host is not in the allowlist: {}",
            download_url
        ));
    }
    if let Err(e) = download::download_file(&download_url, &dest, "").await {
        // A failed transfer must not leave a partial/corrupt file in the
        // mods folder (it would be listed as an installed mod).
        let _ = std::fs::remove_file(&dest);
        return Err(e.to_string());
    }
    // No SHA1 is available for mod downloads — at least verify the result is
    // a real JAR archive and not an error page / bogus response.
    if let Err(e) = download::verify_zip_magic(&dest) {
        let _ = std::fs::remove_file(&dest);
        return Err(e.to_string());
    }
    let final_name = safe_name.clone();

    // Write unified sidecar metadata (`.index/` folder, same layout as
    // Prism) so the installed file can be tracked back to its source,
    // including hash/installed_at for later compatibility & update checks.
    let hash = download::hash_file_sha1(&dest).ok();
    let _ = content::write_sidecar(
        &content_dir,
        &safe_name,
        &content::SidecarWrite {
            provider: &provider,
            content_type: content::content_type_for_dir(&safe_subfolder),
            project_id: project_id.as_deref(),
            project_name: project_name.as_deref(),
            version_id: modrinth_version_id.as_deref(),
            version_number: version_number.as_deref(),
            file_id: curseforge_file_id.map(|id| id.to_string()).as_deref(),
            hash: hash.as_deref(),
        },
    );

    Ok(final_name)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_download_to_folder(
    state: State<'_, AppState>,
    instance_name: String,
    subfolder: String,
    download_url: String,
    file_name: String,
    project_id: Option<String>,
    project_name: Option<String>,
    version_id: Option<String>,
    version_number: Option<String>,
    provider: String,
    old_filename: Option<String>,
    expected_sha1: String,
    file_id: Option<String>,
) -> Result<String, String> {
    validate_instance_name(&instance_name)?;
    // CurseForge chain mode: no direct URL in the plan — the backend
    // resolves (project id, file id) → signed URL → CDN mirrors here. Only
    // this mode may bypass the HTTPS/allowlist pre-checks (each candidate is
    // still validated inside the download layer).
    let numeric = |v: &Option<String>| {
        v.as_deref()
            .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
    };
    let cf_native = download_url.is_empty()
        && provider.eq_ignore_ascii_case("curseforge")
        && numeric(&project_id)
        && numeric(&file_id);
    if !cf_native {
        if !download_url.starts_with("https://") {
            return Err("Download URL must be HTTPS.".to_string());
        }
        if !is_allowed_download_host(&download_url) {
            return Err(format!(
                "Download host is not in the allowlist: {}",
                download_url
            ));
        }
    }
    // Whitelist allowed subfolders under the instance .minecraft dir.
    const ALLOWED: &[&str] = &["mods", "resourcepacks", "shaderpacks", "config"];
    let safe_subfolder = subfolder.trim_matches('/').trim_matches('\\');
    if !ALLOWED
        .iter()
        .any(|s| s.eq_ignore_ascii_case(safe_subfolder))
    {
        return Err(format!("Subfolder '{}' is not allowed.", subfolder));
    }
    if safe_subfolder.contains("..")
        || safe_subfolder.contains('\0')
        || safe_subfolder.contains(';')
    {
        return Err("Subfolder contains invalid characters.".to_string());
    }
    let (dest_dir, safe_name) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        let dest_dir = instance
            .minecraft_dir(&config.instances_dir())
            .join(safe_subfolder);
        let _ = std::fs::create_dir_all(&dest_dir);
        let safe_name = std::path::Path::new(&file_name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid file name")?
            .to_string();
        (dest_dir, safe_name)
    };
    // No caller-provided old filename? Resolve it from the sidecar index by
    // project id: installing another version of an already-installed project
    // (or a dependency that is already present) must replace the old file
    // instead of adding a second jar next to it.
    let old_filename = match old_filename {
        Some(f) => Some(f),
        None => project_id.as_deref().and_then(|pid| {
            content::list_installed(&dest_dir, content::content_type_for_dir(safe_subfolder))
                .into_iter()
                .find(|ic| ic.project_id.as_deref() == Some(pid))
                .map(|ic| ic.filename)
        }),
    };
    // An update of a DISABLED file must stay disabled: the `.disabled`
    // suffix is the enable switch the game honors, and dropping it silently
    // re-enabled the mod on every update.
    let keep_disabled = old_filename
        .as_deref()
        .map(|f| f.ends_with(".disabled"))
        .unwrap_or(false);
    let final_name = if keep_disabled {
        format!("{}.disabled", safe_name)
    } else {
        safe_name.clone()
    };
    let dest = dest_dir.join(&final_name);
    // Park any old file aside (rename, never delete): the previous code
    // deleted the old jar before the network transfer started, so a failed
    // download left the user with no mod at all — and, due to a stem bug,
    // usually did not even match the real filename, leaving duplicates.
    // On failure the parked file is restored; on success it is removed
    // together with its stale sidecar.
    let stashed = match old_filename {
        Some(ref raw) => crate::instances::stash_old_content_files(&dest_dir, raw)?,
        None => Vec::new(),
    };
    let downloaded = async {
        if cf_native {
            let pid = project_id
                .as_deref()
                .unwrap_or("")
                .parse::<u64>()
                .unwrap_or(0);
            let fid = file_id.as_deref().unwrap_or("").parse::<u64>().unwrap_or(0);
            let key = {
                let config = state.config.lock().map_err(|e| e.to_string())?;
                config.curseforge_api_key.clone()
            };
            curseforge::download_cf_native(pid, fid, &safe_name, &expected_sha1, &dest, &key)
                .await
                .map_err(|e| e.to_string())?;
        } else {
            download::download_file(&download_url, &dest, &expected_sha1)
                .await
                .map_err(|e| e.to_string())?;
        }
        // When no SHA1 is available, at least verify the result is a real
        // archive (JAR/ZIP magic) and not an HTML error page / bogus body.
        if expected_sha1.is_empty() && (safe_name.ends_with(".jar") || safe_name.ends_with(".zip"))
        {
            download::verify_zip_magic(&dest).map_err(|e| e.to_string())?;
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(e) = downloaded {
        // The replacement is unusable: drop it (it may exist after a
        // zip-magic failure) and put the old file back where it was.
        let _ = std::fs::remove_file(&dest);
        crate::instances::restore_stashed_files(&stashed);
        return Err(e);
    }
    // Replacement verified — the parked old file is no longer needed.
    crate::instances::discard_stashed_files(&stashed);
    if let Some(ref raw_old) = old_filename {
        crate::instances::cleanup_old_sidecar_after_update(&dest_dir, raw_old, &final_name);
    }

    // Write unified sidecar metadata (`.index/` folder) so the installed file
    // can be tracked back to its source. The verified SHA1 (or a freshly
    // computed one when the API had none) feeds the hash-based update check.
    let hash = if expected_sha1.is_empty() {
        download::hash_file_sha1(&dest).ok()
    } else {
        Some(expected_sha1.clone())
    };
    let _ = content::write_sidecar(
        &dest_dir,
        &final_name,
        &content::SidecarWrite {
            provider: &provider,
            content_type: content::content_type_for_dir(safe_subfolder),
            project_id: project_id.as_deref(),
            project_name: project_name.as_deref(),
            version_id: version_id.as_deref(),
            version_number: version_number.as_deref(),
            file_id: file_id.as_deref(),
            hash: hash.as_deref(),
        },
    );

    Ok(final_name)
}

#[tauri::command]
pub async fn cmd_list_instance_mods(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<ModMetadata>, String> {
    validate_instance_name(&instance_name)?;
    let config = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        guard.clone()
    };
    tauri::async_runtime::spawn_blocking(move || {
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        let mods_dir = instance.mods_dir(&config.instances_dir());
        if !mods_dir.exists() {
            return Ok(Vec::new());
        }
        // Roll back any file parked by an interrupted update before listing.
        crate::instances::recover_stashed_content_files(&mods_dir);
        let mut mods = Vec::new();
        let pw_index = crate::instances::load_packwiz_index(&mods_dir);
        if let Ok(entries) = std::fs::read_dir(&mods_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if filename.ends_with(".voidlauncher.json") {
                    continue;
                }
                let is_jar = filename.ends_with(".jar");
                let is_disabled = filename.ends_with(".jar.disabled");
                if !(is_jar || is_disabled) {
                    continue;
                }
                let enabled = is_jar && !is_disabled;
                let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let meta = read_mod_meta_from_jar(&path);
                let pw = pw_index.get(&filename);
                let sidecar_project_id = read_mod_sidecar_slug(&mods_dir, &filename);
                let project_id = sidecar_project_id.clone().or_else(|| {
                    pw.filter(|p| !p.project_id.is_empty())
                        .map(|p| p.project_id.clone())
                });
                let (slug, slug_verified) = project_id
                    .as_ref()
                    .map(|s| (Some(s.clone()), sidecar_project_id.is_some()))
                    .unwrap_or_else(|| (meta.slug, false));
                let provider = read_mod_sidecar_provider(&mods_dir, &filename)
                    .map(|s| crate::instances::normalize_provider(&s))
                    .or_else(|| {
                        pw.filter(|p| !p.provider.is_empty())
                            .map(|p| p.provider.clone())
                    })
                    // No sidecar / packwiz provenance: jar meta only knows the
                    // loader (Fabric/Forge/...), not the download source — show
                    // Local rather than a loader name in the Source column.
                    .unwrap_or_else(|| "Local".to_string());
                let name = pw
                    .filter(|p| !p.name.is_empty())
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| meta.name.clone());
                let version = pw
                    .filter(|p| !p.version.is_empty())
                    .map(|p| p.version.clone())
                    .unwrap_or_else(|| meta.version.clone());
                mods.push(ModMetadata {
                    filename,
                    name,
                    version,
                    provider,
                    enabled,
                    file_size,
                    icon: meta.icon,
                    slug,
                    project_id,
                    slug_verified,
                });
            }
        }
        mods.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(mods)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cmd_remove_instance_mod(
    state: State<'_, AppState>,
    instance_name: String,
    filename: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let safe_name = std::path::Path::new(&filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?
        .to_string();
    let config = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        guard.clone()
    };
    tauri::async_runtime::spawn_blocking(move || {
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        let mods_dir = instance.mods_dir(&config.instances_dir());
        let mod_path = mods_dir.join(&safe_name);
        if mod_path.exists() {
            std::fs::remove_file(&mod_path).map_err(|e| e.to_string())?;
        }
        crate::instances::remove_sidecar_meta(&mods_dir, &safe_name);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cmd_get_mod_metadata(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<ModMetadata>, String> {
    // Clone-and-release: the scan below can take a while on big mod folders;
    // holding the config lock through it would stall every other command
    // that needs it (instance lists, launch checks, ...).
    let config = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        guard.clone()
    };
    tauri::async_runtime::spawn_blocking(move || {
        get_mod_metadata(&config, &instance_name).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Session-level metadata cache: path -> (mtime, size, parsed metadata).
/// Opening the mods tab re-scans the folder on every visit; with this cache a
/// repeat visit skips re-opening every jar (the cold scan of a big pack took
/// seconds). Entries invalidate automatically when file size/mtime changes.
/// The map is ALSO persisted between runs (see `MetaCacheFile`) — a cold scan
/// of the owner's ~370-mod pack measured 1.8s while the validated on-disk
/// cache makes the first open of the next session cost only a stat pass.
type ModMetaCache = std::collections::HashMap<PathBuf, (std::time::SystemTime, u64, ModMetadata)>;
static META_CACHE: OnceLock<Mutex<ModMetaCache>> = OnceLock::new();
/// Where the metadata cache is persisted between runs; resolved once from
/// the config so the lazy statics have no config dependency.
static META_CACHE_FILE: OnceLock<PathBuf> = OnceLock::new();

/// Bump when jar parsing changes in a way that alters stored values:
/// persisted entries are keyed only by path+mtime+size and would otherwise
/// survive a parser change with stale results.
const META_CACHE_VERSION: u32 = 2;

#[derive(serde::Serialize, serde::Deserialize)]
struct MetaCacheFile {
    version: u32,
    entries: Vec<MetaCacheFileEntry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MetaCacheFileEntry {
    #[serde(default)]
    path: String,
    /// Milliseconds since the Unix epoch (0 = unknown, simply never matches
    /// a real mtime, so the entry gets re-parsed).
    #[serde(default)]
    mtime_ms: u64,
    #[serde(default)]
    size: u64,
    meta: ModMetadata,
}

fn system_time_to_millis(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn millis_to_system_time(ms: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms)
}

fn meta_cache_file(config: &AppConfig) -> &'static PathBuf {
    META_CACHE_FILE.get_or_init(|| config.data_dir.join("mod_meta_cache.json"))
}

fn meta_cache(config: &AppConfig) -> &'static Mutex<ModMetaCache> {
    META_CACHE.get_or_init(|| Mutex::new(load_meta_cache_from_disk(meta_cache_file(config))))
}

/// Best-effort load of the persisted metadata cache. Any problem (missing
/// file, version mismatch, corruption) silently degrades to an empty cache —
/// the scan then just re-parses the jars.
fn load_meta_cache_from_disk(path: &std::path::Path) -> ModMetaCache {
    let mut cache = ModMetaCache::new();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return cache;
    };
    let Ok(file) = serde_json::from_str::<MetaCacheFile>(&raw) else {
        return cache;
    };
    if file.version != META_CACHE_VERSION {
        return cache;
    }
    for e in file.entries {
        if e.path.is_empty() {
            continue;
        }
        cache.insert(
            PathBuf::from(e.path),
            (millis_to_system_time(e.mtime_ms), e.size, e.meta),
        );
    }
    cache
}

/// Best-effort persistence of the metadata cache. Write-to-tmp-then-rename,
/// so a crash mid-write can never leave a truncated (and then ignored) file.
/// A growth guard prunes entries whose files no longer exist once the cache
/// carries thousands of them; below the threshold nothing is checked, so the
/// common case stays cheap.
fn save_meta_cache_to_disk(path: &std::path::Path, cache: &ModMetaCache) {
    let mut entries: Vec<MetaCacheFileEntry> = cache
        .iter()
        .map(|(p, (mtime, size, meta))| MetaCacheFileEntry {
            path: p.to_string_lossy().to_string(),
            mtime_ms: system_time_to_millis(*mtime),
            size: *size,
            meta: meta.clone(),
        })
        .collect();
    if entries.len() > 4000 {
        entries.retain(|e| std::path::Path::new(&e.path).exists());
    }
    let Ok(json) = serde_json::to_string(&MetaCacheFile {
        version: META_CACHE_VERSION,
        entries,
    }) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Patch a network-resolved mod icon into the metadata cache entry for
/// `jar_path` (only when the entry still matches the file's mtime/size) and
/// persist the cache. Called only when the jar itself ships no logo: such a
/// mod had its icon re-fetched from the network on every app run — after the
/// first resolve the metadata list itself carries the icon, so the slow
/// fallback runs once, ever. Icons extracted from jars are NOT stored (the
/// quick local extraction stays the source of truth; embedding every jar
/// icon would bloat the metadata payload).
fn store_mod_icon_in_meta_cache(
    config: &AppConfig,
    jar_path: &std::path::Path,
    mtime: std::time::SystemTime,
    size: u64,
    icon: &str,
) {
    let mut cache = meta_cache(config).lock().unwrap_or_else(|p| p.into_inner());
    if let Some((m, s, meta)) = cache.get_mut(jar_path) {
        if *m == mtime && *s == size && meta.icon.as_deref() != Some(icon) {
            meta.icon = Some(icon.to_string());
            save_meta_cache_to_disk(meta_cache_file(config), &cache);
        }
    }
}

#[derive(Clone)]
struct ModFileEntry {
    path: PathBuf,
    filename: String,
    enabled: bool,
    mtime: std::time::SystemTime,
    size: u64,
}

/// Pure implementation of `cmd_get_mod_metadata` (unit-testable without Tauri).
pub(crate) fn get_mod_metadata(
    config: &AppConfig,
    instance_name: &str,
) -> crate::error::Result<Vec<ModMetadata>> {
    let instance = instances::get_instance(&config.instances_dir(), instance_name)?;
    let mods_dir = instance.minecraft_dir(&config.instances_dir()).join("mods");
    if !mods_dir.exists() {
        return Ok(Vec::new());
    }
    // Roll back any file parked by an interrupted update before scanning —
    // otherwise the mod stays invisible (a parked name is not a .jar and is
    // skipped by this listing) until some other screen happens to recover it.
    instances::recover_stashed_content_files(&mods_dir);
    let pw_index = std::sync::Arc::new(instances::load_packwiz_index(&mods_dir));

    // Collect candidate files (cheap stat calls) before spawning workers.
    let mut files: Vec<ModFileEntry> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(filename) = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let is_jar = filename.ends_with(".jar");
            let is_disabled = filename.ends_with(".jar.disabled");
            // Only real .jar mods are listed — datapacks/resource packs that
            // a modpack ships inside `mods/` as .zip stay invisible, exactly
            // like Prism Launcher behaves. They are not mods, just bundled data.
            if !(is_jar || is_disabled) {
                continue;
            }
            let (mtime, size) = match std::fs::metadata(&path) {
                // Truncate to whole milliseconds: the metadata cache persists
                // mtimes as ints and a raw Windows SystemTime carries 100ns
                // precision, so an un-truncated value never matches the value
                // reloaded from disk (every entry was a miss on every run).
                Ok(m) => (
                    millis_to_system_time(system_time_to_millis(
                        m.modified().unwrap_or(std::time::UNIX_EPOCH),
                    )),
                    m.len(),
                ),
                Err(_) => (std::time::UNIX_EPOCH, 0),
            };
            files.push(ModFileEntry {
                path,
                filename,
                enabled: is_jar && !is_disabled,
                mtime,
                size,
            });
        }
    }
    if files.is_empty() {
        return Ok(Vec::new());
    }

    // Serve cache hits first; only cache misses need the expensive jar parse.
    enum Job {
        Cached(ModMetadata),
        Fresh,
    }
    let mut jobs: Vec<Job> = Vec::with_capacity(files.len());
    let mut missed_idx: Vec<usize> = Vec::new();
    {
        let cache = meta_cache(config).lock().unwrap_or_else(|p| p.into_inner());
        for (i, f) in files.iter().enumerate() {
            match cache.get(&f.path) {
                Some((m, s, meta)) if *m == f.mtime && *s == f.size => {
                    jobs.push(Job::Cached(meta.clone()));
                }
                _ => {
                    jobs.push(Job::Fresh);
                    missed_idx.push(i);
                }
            }
        }
    }

    // Parse misses in parallel across a few worker threads. Each jar used to
    // be opened sequentially; a cold scan of a big pack was disk-bound and
    // took several seconds.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let fresh: Vec<&ModFileEntry> = missed_idx.iter().map(|&i| &files[i]).collect();
    let computed: Vec<Vec<(usize, ModMetadata)>> = if fresh.len() >= 4 && threads > 1 {
        let chunk_size = fresh.len().div_ceil(threads);
        let start_indices: Vec<usize> = (0..threads)
            .map(|t| t * chunk_size)
            .take_while(|&s| s < fresh.len())
            .collect();
        let mods_dir_ref = &mods_dir;
        std::thread::scope(|scope| {
            let handles: Vec<_> = start_indices
                .iter()
                .map(|&start| {
                    let slice = &fresh[start..(start + chunk_size).min(fresh.len())];
                    let pw_index = &pw_index;
                    scope.spawn(move || {
                        slice
                            .iter()
                            .enumerate()
                            .map(|(k, f)| {
                                (start + k, build_mod_metadata(mods_dir_ref, pw_index, f))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        })
    } else {
        vec![fresh
            .iter()
            .enumerate()
            .map(|(k, f)| (k, build_mod_metadata(&mods_dir, &pw_index, f)))
            .collect()]
    };

    // Store freshly computed entries in the cache, then merge results back
    // into job order (jobs[i] for i in missed_idx are replaced). When
    // anything was freshly parsed, persist the cache so the NEXT app run
    // opens the pack from validated disk data instead of re-parsing every
    // jar; files are still validated by the mtime/size stat pass above.
    {
        let mut cache = meta_cache(config).lock().unwrap_or_else(|p| p.into_inner());
        for per_thread in &computed {
            for (k, meta) in per_thread {
                let f = fresh[*k];
                cache.insert(f.path.clone(), (f.mtime, f.size, meta.clone()));
                jobs[missed_idx[*k]] = Job::Cached(meta.clone());
            }
        }
        if !missed_idx.is_empty() {
            save_meta_cache_to_disk(meta_cache_file(config), &cache);
        }
    }
    let mut mods: Vec<ModMetadata> = Vec::with_capacity(jobs.len());
    for job in jobs {
        match job {
            Job::Cached(meta) => mods.push(meta),
            Job::Fresh => unreachable!("all fresh jobs were computed"),
        }
    }
    mods.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(mods)
}

/// Build metadata for a single mod file: jar parse + ONE sidecar read +
/// packwiz fallback. Priority rules preserved from the original sequential
/// implementation.
fn build_mod_metadata(
    mods_dir: &std::path::Path,
    pw_index: &std::collections::HashMap<String, instances::PackwizMeta>,
    f: &ModFileEntry,
) -> ModMetadata {
    let ModFileEntry {
        path,
        filename,
        enabled,
        size,
        ..
    } = f;
    let meta = read_mod_meta_from_jar(path);
    let pw = pw_index.get(filename);
    // Single sidecar read for all fields.
    let sidecar = read_sidecar_fields(mods_dir, filename);
    let project_id = sidecar.project_id.clone().or_else(|| {
        pw.filter(|p| !p.project_id.is_empty())
            .map(|p| p.project_id.clone())
    });
    let jar_name = instances::strip_minecraft_color_codes(&meta.name);
    let jar_name = if usable_mod_name(&jar_name, meta.slug.as_deref()).is_some() {
        jar_name
    } else {
        String::new()
    };
    let (slug, slug_verified) = project_id
        .as_ref()
        .map(|s| (Some(s.clone()), sidecar.project_id.is_some()))
        .unwrap_or_else(|| (meta.slug, false));
    // Provider priority: sidecar (modrinth/curseforge/local) > packwiz > Local
    // (jar meta is a loader name, not a download source).
    let provider = match sidecar.provider.as_deref() {
        Some(s) => crate::instances::normalize_provider(s),
        None => pw
            .filter(|p| !p.provider.is_empty())
            .map(|p| p.provider.clone())
            .unwrap_or_else(|| "Local".to_string()),
    };
    let name = if !jar_name.is_empty() {
        jar_name
    } else {
        let fallback = sidecar
            .project_name
            .as_deref()
            .and_then(|n| usable_mod_name(n, slug.as_deref()))
            .map(str::to_string)
            .or_else(|| {
                pw.and_then(|p| usable_mod_name(&p.name, slug.as_deref()).map(str::to_string))
            })
            .unwrap_or_else(|| filename.trim_end_matches(".jar").to_string());
        instances::strip_minecraft_color_codes(&fallback)
    };
    // Version priority: sidecar (verified from Modrinth API) >
    // packwiz > sanitized JAR version. "${version}" and "Unknown"
    // are build-placeholders, not real versions.
    let mut version = sidecar
        .version_number
        .filter(|v| !v.is_empty())
        .or_else(|| {
            pw.filter(|p| !p.version.is_empty())
                .map(|p| p.version.clone())
        })
        .unwrap_or_else(|| sanitize_version(&meta.version));
    // No real version anywhere — extract one from the file name
    // (works for JARs with version-less metadata like "${file.jarVersion}").
    if version.is_empty() {
        version = crate::instances::extract_version_from_filename(filename);
    }
    // Packs installed before the sidecar carried a version have
    // nothing left to show — fall back to the CurseForge display
    // name ("[Forge 1.20.1] v2.8.0") instead of an empty cell, but
    // only when it actually looks like a version (never for garbage
    // like "mod-1.20.1-4.7.jar").
    if version.is_empty() {
        if let Some(pn) = sidecar.project_name {
            if looks_like_version(&pn) {
                version = pn;
            }
        }
    }
    ModMetadata {
        filename: filename.clone(),
        name,
        version,
        provider,
        enabled: *enabled,
        file_size: *size,
        icon: meta.icon,
        slug,
        project_id,
        slug_verified,
    }
}

/// Read the Modrinth/CurseForge project slug from the sidecar metadata
/// (`.index/` layout, legacy layout as fallback) that `cmd_install_mod`
/// writes at download time.
fn read_mod_sidecar_slug(mods_dir: &std::path::Path, filename: &str) -> Option<String> {
    let json = crate::instances::read_sidecar_meta(mods_dir, filename)?;
    json["project_id"].as_str().map(|s| s.to_string())
}

/// Read the provider (modrinth/curseforge/local) from the sidecar metadata.
fn read_mod_sidecar_provider(mods_dir: &std::path::Path, filename: &str) -> Option<String> {
    let json = crate::instances::read_sidecar_meta(mods_dir, filename)?;
    json["provider"].as_str().map(|s| s.to_string())
}

fn usable_mod_name<'a>(name: &'a str, mod_id: Option<&str>) -> Option<&'a str> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("unknown")
        || trimmed.contains("${")
        || trimmed.eq_ignore_ascii_case("%s")
    {
        return None;
    }
    if let Some(id) = mod_id.map(str::trim).filter(|id| !id.is_empty()) {
        if trimmed == id {
            return None;
        }
    }
    Some(trimmed)
}

/// Strip build-placeholder versions ("${version}", "${file.jarVersion}",
/// "Unknown", ...) so the UI never shows them as real versions.
fn sanitize_version(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains("${") || trimmed.eq_ignore_ascii_case("unknown") {
        String::new()
    } else {
        trimmed.to_string()
    }
}

/// A project display name can be a usable version ("[Forge 1.20.1] v2.8.0",
/// "v1.0.5") or plain garbage ("recipeessentials-1.20.1-4.7.jar", the raw
/// filename). Only trust it when it starts like a version and carries no
/// file extension or full-file-name smell.
fn looks_like_version(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 60 {
        return false;
    }
    let lower = trimmed.to_lowercase();
    if lower.contains(".jar") || lower.contains(".zip") || lower.contains(".pw.toml") {
        return false;
    }
    let first = trimmed.chars().next().unwrap_or('\0');
    first == '['
        || first.is_ascii_digit()
        || matches!(lower.as_bytes().first(), Some(&b'v') | Some(&b'r'))
}

#[cfg(test)]
pub(crate) fn sanitize_version_pub(raw: &str) -> String {
    sanitize_version(raw)
}

pub(crate) struct ModMetaResult {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) provider: String,
    pub(crate) icon: Option<String>,
    pub(crate) slug: Option<String>,
}

fn fallback_meta_from_filename(path: &std::path::Path) -> ModMetaResult {
    let fallback_name = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("Unknown")
        .to_string();
    let clean_name = if let Some(dash_pos) = fallback_name.rfind('-') {
        let potential_version = &fallback_name[dash_pos + 1..];
        if potential_version
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
        {
            fallback_name[..dash_pos].to_string()
        } else {
            fallback_name.clone()
        }
    } else {
        fallback_name.clone()
    };
    ModMetaResult {
        name: clean_name,
        version: "Unknown".into(),
        provider: "Local".into(),
        icon: None,
        slug: None,
    }
}

#[cfg(test)]
pub(crate) fn read_mod_meta_from_jar_pub(path: &std::path::Path) -> ModMetaResult {
    read_mod_meta_from_jar(path)
}

fn read_mod_meta_from_jar(path: &std::path::Path) -> ModMetaResult {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return fallback_meta_from_filename(path),
    };
    let reader = std::io::BufReader::new(file);
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(a) => a,
        Err(_) => return fallback_meta_from_filename(path),
    };

    // Try fabric.mod.json
    if let Ok(mut file) = archive.by_name("fabric.mod.json") {
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_ok() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                let id = json["id"].as_str().unwrap_or("Unknown");
                let name = usable_mod_name(json["name"].as_str().unwrap_or(""), Some(id))
                    .unwrap_or(id)
                    .to_string();
                let version = json["version"].as_str().unwrap_or("Unknown").to_string();
                let slug = json["id"].as_str().map(|s| s.to_string());
                let icon = json["icon"].as_str().map(|s| {
                    let clean = s
                        .trim_start_matches("/")
                        .trim_start_matches("assets/")
                        .to_string();
                    clean
                });
                return ModMetaResult {
                    name: instances::strip_minecraft_color_codes(&name),
                    version,
                    provider: "Fabric".into(),
                    icon,
                    slug,
                };
            }
        }
    }

    for toml_name in &["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Ok(mut file) = archive.by_name(toml_name) {
            let mut contents = String::new();
            if file.read_to_string(&mut contents).is_ok() {
                if let Ok(toml_val) = contents.parse::<toml::Table>() {
                    if let Some(mods_arr) = toml_val.get("mods").and_then(|v| v.as_array()) {
                        let picked = mods_arr
                            .iter()
                            .filter_map(|m| m.as_table())
                            .find(|m| {
                                let id = m.get("modId").and_then(|v| v.as_str());
                                usable_mod_name(
                                    m.get("displayName").and_then(|v| v.as_str()).unwrap_or(""),
                                    id,
                                )
                                .is_some()
                            })
                            .or_else(|| mods_arr.iter().find_map(|m| m.as_table()));
                        if let Some(picked) = picked {
                            let mod_id = picked
                                .get("modId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let display_name = usable_mod_name(
                                picked
                                    .get("displayName")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(""),
                                Some(&mod_id),
                            )
                            .unwrap_or(&mod_id)
                            .to_string();
                            let version = picked
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let logo = picked
                                .get("logoFile")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let provider = if toml_name.contains("neoforge") {
                                "NeoForge"
                            } else {
                                "Forge"
                            };
                            return ModMetaResult {
                                name: instances::strip_minecraft_color_codes(&display_name),
                                version,
                                provider: provider.into(),
                                icon: logo,
                                slug: Some(mod_id),
                            };
                        }
                    }
                }
                // Fallback to line-based parser for malformed TOML
                let mod_id =
                    extract_toml_field(&contents, "modId").unwrap_or("Unknown".to_string());
                let display_name = extract_toml_field(&contents, "displayName")
                    .and_then(|name| usable_mod_name(&name, Some(&mod_id)).map(str::to_string))
                    .unwrap_or_else(|| mod_id.clone());
                let version =
                    extract_toml_field(&contents, "version").unwrap_or("Unknown".to_string());
                let logo = extract_toml_field(&contents, "logoFile");
                let provider = if toml_name.contains("neoforge") {
                    "NeoForge"
                } else {
                    "Forge"
                };
                return ModMetaResult {
                    name: instances::strip_minecraft_color_codes(&display_name),
                    version,
                    provider: provider.into(),
                    icon: logo,
                    slug: Some(mod_id),
                };
            }
        }
    }

    for info_name in &["mcmod.info", "META-INF/mcmod.info"] {
        let Ok(mut file) = archive.by_name(info_name) else {
            continue;
        };
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_err() {
            continue;
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) else {
            continue;
        };
        let entries: Vec<&serde_json::Value> = if let Some(arr) = json.as_array() {
            arr.iter().collect()
        } else if let Some(arr) = json.get("modList").and_then(|v| v.as_array()) {
            arr.iter().collect()
        } else if json.get("modid").is_some() {
            vec![&json]
        } else {
            Vec::new()
        };
        let picked = entries
            .iter()
            .copied()
            .find(|m| {
                usable_mod_name(m["name"].as_str().unwrap_or(""), m["modid"].as_str()).is_some()
            })
            .or_else(|| entries.first().copied());
        if let Some(picked) = picked {
            let mod_id = picked["modid"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("Unknown")
                .to_string();
            let display_name =
                usable_mod_name(picked["name"].as_str().unwrap_or(""), Some(&mod_id))
                    .unwrap_or(&mod_id)
                    .to_string();
            let version = picked["version"].as_str().unwrap_or("Unknown").to_string();
            let logo = picked["logoFile"].as_str().map(|s| s.to_string());
            return ModMetaResult {
                name: instances::strip_minecraft_color_codes(&display_name),
                version,
                provider: "Forge".into(),
                icon: logo,
                slug: Some(mod_id),
            };
        }
    }

    // Fallback: extract name from filename
    fallback_meta_from_filename(path)
}

fn strip_toml_comment(val: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in val.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return &val[..i],
            _ => {}
        }
    }
    val
}

fn extract_toml_field(content: &str, field: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(field) && trimmed.contains('=') {
            let raw = trimmed.split_once('=')?.1.trim();
            let raw = strip_toml_comment(raw);
            let val = raw.trim().trim_matches('"').trim_matches('\'');
            if val.starts_with('{') {
                if let Some(start) = val.find(field) {
                    let after = &val[start..];
                    if let Some(eq_pos) = after.find('=') {
                        let v_raw = strip_toml_comment(&after[eq_pos + 1..]);
                        let v = v_raw.trim().trim_matches('"').trim_matches('\'');
                        let v = v.trim_end_matches(',').trim_end_matches('}').trim();
                        return Some(v.to_string());
                    }
                }
                continue;
            }
            return Some(val.to_string());
        }
    }
    None
}

#[tauri::command]
pub async fn cmd_get_mod_icon(
    state: State<'_, AppState>,
    instance_name: String,
    filename: String,
) -> Result<Option<String>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?.clone();
    cmd_get_mod_icon_impl(&config, &instance_name, &filename)
        .await
        .map_err(|e| e.to_string())
}

async fn cmd_get_mod_icon_impl(
    config: &crate::config::AppConfig,
    instance_name: &str,
    filename: &str,
) -> crate::error::Result<Option<String>> {
    let safe_filename = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| crate::error::LauncherError::Download("Invalid filename".into()))?
        .to_string();

    let (jar_path, curseforge_api_key) = {
        let instance = instances::get_instance(&config.instances_dir(), instance_name)
            .map_err(|e| crate::error::LauncherError::Download(e.to_string()))?;
        let mods_dir = instance.minecraft_dir(&config.instances_dir()).join("mods");
        let jar_path = mods_dir.join(&safe_filename);
        let cf_key = config.curseforge_api_key.clone();
        (jar_path, cf_key)
    };

    if !jar_path.exists() {
        return Ok(None);
    }

    // Same millisecond-truncated stat the metadata scan stores, so cache
    // lookups and patches always compare like with like.
    let (mtime, size) = match std::fs::metadata(&jar_path) {
        Ok(m) => (
            millis_to_system_time(system_time_to_millis(
                m.modified().unwrap_or(std::time::UNIX_EPOCH),
            )),
            m.len(),
        ),
        Err(_) => (std::time::UNIX_EPOCH, 0),
    };

    // An icon already *resolved* on an earlier run (a network logo stored as
    // a data URL) is served immediately — no network round trip. A plain
    // scan value is NOT displayable and must never be returned: it is the
    // icon's path INSIDE the jar ("assets/modid/icon.png" from
    // fabric.mod.json / logoFile), so the jar must be read and the real
    // image extracted below.
    {
        let cache = meta_cache(config).lock().unwrap_or_else(|p| p.into_inner());
        if let Some((m, s, meta)) = cache.get(&jar_path) {
            if *m == mtime && *s == size {
                if let Some(icon) = meta.icon.as_deref().filter(|i| i.starts_with("data:")) {
                    return Ok(Some(icon.to_string()));
                }
            }
        }
    }

    // Try extracting icon from the jar itself
    if let Ok(Some(icon)) = extract_icon_from_jar(&jar_path) {
        return Ok(Some(icon));
    }

    // Fallback: try CurseForge API by project_id from sidecar (only if API key is configured)
    if !curseforge_api_key.is_empty() {
        if let Some(mods_dir) = jar_path.parent() {
            if let Some(val) = crate::instances::read_sidecar_meta(mods_dir, &safe_filename) {
                if val["provider"]
                    .as_str()
                    .map(|p| p.eq_ignore_ascii_case("curseforge"))
                    == Some(true)
                {
                    if let Some(pid_str) = val["project_id"].as_str() {
                        if let Ok(pid) = pid_str.parse::<u64>() {
                            if let Ok(Some(icon)) =
                                fetch_curseforge_mod_icon(pid, &curseforge_api_key).await
                            {
                                store_mod_icon_in_meta_cache(config, &jar_path, mtime, size, &icon);
                                return Ok(Some(icon));
                            }
                        }
                    }
                }
            }
        }
    }

    // Fallback: try Modrinth project API for icon_url
    // (sidecar first, then Prism packwiz .pw.toml metadata from imported instances)
    if let Some(mods_dir) = jar_path.parent() {
        let mut provider = None;
        let mut pid = None;
        if let Some(val) = crate::instances::read_sidecar_meta(mods_dir, &safe_filename) {
            provider = val["provider"].as_str().map(|s| s.to_string());
            pid = val["project_id"].as_str().map(|s| s.to_string());
        }
        if pid.is_none() {
            if let Some(pw) = crate::instances::read_packwiz_meta(mods_dir, &safe_filename) {
                if !pw.provider.is_empty() && !pw.project_id.is_empty() {
                    provider = Some(pw.provider);
                    pid = Some(pw.project_id);
                }
            }
        }
        if provider
            .as_deref()
            .map(|p| p.eq_ignore_ascii_case("modrinth"))
            == Some(true)
        {
            if let Some(pid) = pid {
                if let Ok(project) = modrinth::get_project(&pid).await {
                    if let Some(icon_url) = project.icon_url {
                        if let Some(icon_data) = fetch_remote_icon(&icon_url).await {
                            store_mod_icon_in_meta_cache(
                                config, &jar_path, mtime, size, &icon_data,
                            );
                            return Ok(Some(icon_data));
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Fetch a mod's logo icon from CurseForge API and return as base64 data URL
#[cfg(test)]
pub(crate) async fn fetch_curseforge_mod_icon_pub(
    project_id: u64,
    api_key: &str,
) -> crate::error::Result<Option<String>> {
    fetch_curseforge_mod_icon(project_id, api_key).await
}

#[cfg(test)]
pub(crate) async fn cmd_get_mod_icon_pub(
    config: &crate::config::AppConfig,
    instance_name: &str,
    filename: &str,
) -> crate::error::Result<Option<String>> {
    cmd_get_mod_icon_impl(config, instance_name, filename).await
}

async fn fetch_curseforge_mod_icon(
    project_id: u64,
    api_key: &str,
) -> crate::error::Result<Option<String>> {
    let detail = crate::curseforge::get_mod(project_id, api_key)
        .await
        .map_err(|e| crate::error::LauncherError::Download(format!("CF API error: {}", e)))?;

    let thumbnail_url = match detail.logo.as_ref().and_then(|logo| logo.best_url()) {
        Some(url) => url.to_string(),
        None => return Ok(None),
    };

    const MAX_LOGO_BYTES: u64 = 3 * 1024 * 1024;
    let client = crate::download::global_http_client()?;
    let bytes =
        match fetch_remote_image_capped(&client, thumbnail_url.as_str(), MAX_LOGO_BYTES).await {
            Some(b) if !b.is_empty() => b,
            _ => return Ok(None),
        };

    let b64 = base64_encode(&bytes);
    let mime = if thumbnail_url.ends_with(".jpg") || thumbnail_url.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "image/png"
    };

    Ok(Some(format!("data:{};base64,{}", mime, b64)))
}

/// Download an image from a URL and return as base64 data URL.
async fn fetch_remote_icon(url: &str) -> Option<String> {
    const MAX_ICON_BYTES: u64 = 3 * 1024 * 1024;
    let client = match crate::download::global_http_client() {
        Ok(client) => client,
        Err(_) => return None,
    };
    let bytes = fetch_remote_image_capped(&client, url, MAX_ICON_BYTES).await?;
    if bytes.is_empty() {
        return None;
    }
    let b64 = base64_encode(&bytes);
    let mime = if url.ends_with(".jpg") || url.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "image/png"
    };
    Some(format!("data:{};base64,{}", mime, b64))
}

/// Remote images (mod icons, thumbnails, CDN logos) are buffered whole into
/// RAM and then base64-expanded (×4/3) for the renderer, so a malicious or
/// broken host could otherwise force the launcher to allocate arbitrarily
/// much memory. Fetch with a strict byte cap; anything larger is ignored.
async fn fetch_remote_image_capped<S: reqwest::IntoUrl>(
    client: &reqwest::Client,
    url: S,
    max_bytes: u64,
) -> Option<Vec<u8>> {
    let resp = crate::download::send_with_fallback(client.get(url))
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    // Pre-check Content-Length when the server advertises it.
    if let Some(len) = resp.content_length() {
        if len > max_bytes {
            tracing::warn!(target: "launcher", "Image too large ({} bytes, cap {}), skipping", len, max_bytes);
            return None;
        }
    }
    let mut remaining = max_bytes;
    let mut buf = Vec::with_capacity(resp.content_length().unwrap_or(0).min(max_bytes) as usize);
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if (remaining as usize) < chunk.len() {
            tracing::warn!(target: "launcher", "Image exceeded cap {}, skipping", max_bytes);
            return None;
        }
        buf.extend_from_slice(&chunk);
        remaining -= chunk.len() as u64;
    }
    Some(buf)
}

/// Fetch a remote icon through the launcher's HTTP stack (proxy with direct
/// fallback) and return it as a base64 data URL. The renderer must NOT load
/// CDN images directly via <img src="https://...">: the webview honors only
/// the system proxy without our fallback logic, so a flaky proxy leaves
/// icons permanently blank. Restricted to trusted download hosts so the
/// renderer cannot abuse this as an SSRF proxy.
#[tauri::command]
pub async fn cmd_fetch_icon_url(url: String) -> Result<Option<String>, String> {
    if !crate::is_allowed_download_host(&url) {
        return Err("Host not allowed".into());
    }
    Ok(fetch_remote_icon(&url).await)
}

fn extract_icon_from_jar(
    path: &std::path::Path,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader)?;

    // Try to find icon in common locations
    let mut found_icon_name = None;

    // First check fabric.mod.json for icon path
    if let Ok(mut f) = archive.by_name("fabric.mod.json") {
        let mut contents = String::new();
        if f.read_to_string(&mut contents).is_ok() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(icon) = json["icon"].as_str() {
                    let clean = icon.trim_start_matches("/").to_string();
                    found_icon_name = Some(clean);
                }
            }
        }
    }

    // Check mods.toml / neoforge.mods.toml (Forge/NeoForge) for logoFile
    if found_icon_name.is_none() {
        for meta_name in &["META-INF/mods.toml", "META-INF/neoforge.mods.toml"] {
            if let Ok(mut f) = archive.by_name(meta_name) {
                let mut contents = String::new();
                if f.read_to_string(&mut contents).is_ok() {
                    // Parse TOML for logoFile
                    if let Ok(toml_val) = contents.parse::<toml::Table>() {
                        if let Some(mods_array) = toml_val.get("mods").and_then(|v| v.as_array()) {
                            for m in mods_array {
                                if let Some(logo) = m.get("logoFile").and_then(|v| v.as_str()) {
                                    let clean = logo.trim_start_matches("/").to_string();
                                    found_icon_name = Some(clean);
                                    break;
                                }
                            }
                        }
                    }
                }
                if found_icon_name.is_some() {
                    break;
                }
            }
        }
    }

    // If we found an icon path in metadata, try to read it
    if let Some(icon_name) = found_icon_name {
        if let Ok(mut f) = archive.by_name(&icon_name) {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            let b64 = base64_encode(&buf);
            let mime = if icon_name.ends_with(".png") {
                "image/png"
            } else if icon_name.ends_with(".jpg") || icon_name.ends_with(".jpeg") {
                "image/jpeg"
            } else {
                "image/png"
            };
            return Ok(Some(format!("data:{};base64,{}", mime, b64)));
        }
    }

    // Fallback: look for icon.png in root or assets
    for candidate in &["icon.png", "logo.png", "pack.png", "assets/icon.png"] {
        if let Ok(mut f) = archive.by_name(candidate) {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            let b64 = base64_encode(&buf);
            return Ok(Some(format!("data:image/png;base64,{}", b64)));
        }
    }

    // Scan all entries for icon-like files
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().to_string();
            let lower = name.to_lowercase();
            if (lower.ends_with("icon.png")
                || lower.ends_with("logo.png")
                || lower.ends_with("pack.png"))
                && !lower.contains("META-INF")
            {
                let mut buf = Vec::new();
                let mut file = entry;
                file.read_to_end(&mut buf)?;
                let b64 = base64_encode(&buf);
                return Ok(Some(format!("data:image/png;base64,{}", b64)));
            }
        }
    }

    Ok(None)
}

/// Public wrapper around the manual base64 encoder for other modules
/// (e.g. misc.rs asset fetching). Standard alphabet with padding.
pub(crate) fn base64_encode_pub(data: &[u8]) -> String {
    base64_encode(data)
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod meta_cache_file_tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn sample_meta() -> ModMetadata {
        ModMetadata {
            filename: "sodium.jar".into(),
            name: "Sodium".into(),
            version: "0.6.0".into(),
            provider: "Modrinth".into(),
            enabled: true,
            file_size: 4242,
            icon: None,
            slug: Some("sodium".into()),
            project_id: Some("AANobbMI".into()),
            slug_verified: true,
        }
    }

    fn test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-metacache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn write_jar(path: &std::path::Path, files: &[(&str, &str)]) {
        use std::io::Write as _;
        let file = std::fs::File::create(path).expect("create jar");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in files {
            zip.start_file(*name, options).expect("start zip entry");
            zip.write_all(contents.as_bytes()).expect("write zip entry");
        }
        zip.finish().expect("finish jar");
    }

    #[test]
    fn usable_mod_name_rejects_ids_and_placeholders() {
        assert!(usable_mod_name("", Some("x")).is_none());
        assert!(usable_mod_name("Unknown", Some("x")).is_none());
        assert!(usable_mod_name("${mod_name}", Some("x")).is_none());
        assert!(usable_mod_name("x", Some("x")).is_none());
        assert_eq!(usable_mod_name("X", Some("x")), Some("X"));
        assert_eq!(
            usable_mod_name("Real Mod Name", Some("realmod")),
            Some("Real Mod Name")
        );
    }

    #[test]
    fn forge_multi_mod_jar_prefers_real_display_name() {
        let dir = test_dir("forge-multi");
        let jar = dir.join("combined.jar");
        write_jar(
            &jar,
            &[(
                "META-INF/mods.toml",
                r#"
modLoader="javafml"
loaderVersion="[47,)"
license="MIT"

[[mods]]
modId="librarycore"
version="1.0"

[[mods]]
modId="coolmod"
displayName="Cool Mod"
version="2.3"
"#,
            )],
        );
        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(meta.name, "Cool Mod");
        assert_eq!(meta.slug.as_deref(), Some("coolmod"));
        assert_eq!(meta.version, "2.3");
        assert_eq!(meta.provider, "Forge");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_mcmod_info_provides_display_name() {
        let dir = test_dir("mcmod-info");
        let jar = dir.join("old.jar");
        write_jar(
            &jar,
            &[(
                "mcmod.info",
                r#"[{"modid":"oldmod","name":"Old Mod","version":"1.2.3","mcversion":"1.12.2"}]"#,
            )],
        );
        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(meta.name, "Old Mod");
        assert_eq!(meta.slug.as_deref(), Some("oldmod"));
        assert_eq!(meta.version, "1.2.3");
        assert_eq!(meta.provider, "Forge");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn cf_index(
        file_id: u64,
        game_version: &str,
        mod_loader: Option<u32>,
    ) -> curseforge::CfFileIndex {
        curseforge::CfFileIndex {
            game_version: game_version.to_string(),
            file_id,
            filename: format!("file-{file_id}.jar"),
            release_type: 1,
            game_version_type_id: None,
            mod_loader,
        }
    }

    #[test]
    fn cf_update_index_prefers_stable_releases() {
        let project = curseforge::CfBulkMod {
            id: 1,
            name: "Test Mod".into(),
            latest_files: Vec::new(),
            latest_files_indexes: vec![
                {
                    let mut i = cf_index(40, "1.20.1", Some(1));
                    i.release_type = 2;
                    i
                },
                cf_index(30, "1.20.1", Some(1)),
                cf_index(20, "1.20.1", Some(1)),
            ],
        };
        assert_eq!(
            cf_pick_update_index(&project, Some("1.20.1"), Some(1), Some(20)).map(|i| i.file_id),
            Some(30)
        );
        assert!(cf_pick_update_index(&project, Some("1.20.1"), Some(1), Some(30)).is_none());
    }

    #[test]
    fn cf_update_index_allows_beta_only_projects() {
        let mut newer = cf_index(50, "1.20.1", Some(1));
        newer.release_type = 2;
        let mut older = cf_index(45, "1.20.1", Some(1));
        older.release_type = 2;
        let project = curseforge::CfBulkMod {
            id: 1,
            name: "Beta Only".into(),
            latest_files: Vec::new(),
            latest_files_indexes: vec![newer, older],
        };
        assert_eq!(
            cf_pick_update_index(&project, Some("1.20.1"), Some(1), Some(45)).map(|i| i.file_id),
            Some(50)
        );
    }

    #[test]
    fn cf_update_index_never_offers_the_installed_or_older_file() {
        let project = curseforge::CfBulkMod {
            id: 1,
            name: "Test Mod".into(),
            latest_files: Vec::new(),
            latest_files_indexes: vec![
                cf_index(30, "1.20.1", Some(1)),
                cf_index(20, "1.20.1", Some(1)),
                cf_index(10, "1.20.1", Some(4)),
            ],
        };
        assert_eq!(
            cf_pick_update_index(&project, Some("1.20.1"), Some(1), Some(20)).map(|i| i.file_id),
            Some(30)
        );
        assert!(cf_pick_update_index(&project, Some("1.20.1"), Some(1), Some(30)).is_none());
        assert_eq!(
            cf_pick_update_index(&project, Some("1.20.1"), Some(1), None).map(|i| i.file_id),
            Some(30)
        );
        assert!(cf_pick_update_index(&project, Some("1.21.1"), Some(1), Some(20)).is_none());
        assert_eq!(
            cf_pick_update_index(&project, Some("1.20.1"), Some(4), Some(10)).map(|i| i.file_id),
            None
        );
    }

    #[test]
    fn disk_cache_roundtrip_preserves_entries() {
        let dir = test_dir("roundtrip");
        let file = dir.join("mod_meta_cache.json");

        // Missing file → empty cache, no error.
        assert!(load_meta_cache_from_disk(&file).is_empty());

        let mut cache = ModMetaCache::new();
        let mtime = UNIX_EPOCH + Duration::from_millis(1_700_000_123_456);
        cache.insert(
            PathBuf::from("C:/inst/mods/sodium.jar"),
            (mtime, 4242, sample_meta()),
        );
        save_meta_cache_to_disk(&file, &cache);

        let loaded = load_meta_cache_from_disk(&file);
        assert_eq!(loaded.len(), 1);
        let (m, s, meta) = loaded
            .get(std::path::Path::new("C:/inst/mods/sodium.jar"))
            .expect("entry survives the roundtrip");
        assert_eq!(*m, mtime, "mtime keeps millisecond precision");
        assert_eq!(*s, 4242);
        assert_eq!(meta.name, "Sodium");
        assert_eq!(meta.project_id.as_deref(), Some("AANobbMI"));
        assert!(meta.slug_verified);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_cache_rejects_wrong_version_and_garbage() {
        let dir = test_dir("reject");
        let file = dir.join("mod_meta_cache.json");

        // Wrong format version → discarded wholesale (parser may have changed).
        std::fs::write(
            &file,
            r#"{"version":999999,"entries":[{"path":"C:/x.jar","mtime_ms":1,"size":2,"meta":{}}]}"#,
        )
        .unwrap();
        assert!(load_meta_cache_from_disk(&file).is_empty());

        // Corrupt JSON → discarded, never panics.
        std::fs::write(&file, "{not json at all").unwrap();
        assert!(load_meta_cache_from_disk(&file).is_empty());

        // Current version, but the entry itself is partial: serde defaults
        // fill non-meta fields, so the entry still loads.
        std::fs::write(
            &file,
            r#"{"version":2,"entries":[{"path":"C:/x.jar","mtime_ms":5,"size":6,"meta":{"name":"X"}}]}"#,
        )
        .unwrap();
        let loaded = load_meta_cache_from_disk(&file);
        assert_eq!(loaded.len(), 1);
        let meta = loaded
            .values()
            .next()
            .map(|(_, _, m)| m)
            .expect("partial entry loads");
        assert_eq!(meta.name, "X");
        assert_eq!(
            meta.filename, "",
            "missing fields default instead of failing"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

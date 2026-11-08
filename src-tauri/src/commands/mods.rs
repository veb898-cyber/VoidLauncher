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
    mc_version: Option<&str>,
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
                if !meta.is_compatible_with(loader, mc_version) {
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

                let (jar_version, _) = meta.version_for(loader, mc_version);
                mod_files.push(ModFile {
                    filename,
                    name,
                    version: normalize_mod_version(&jar_version, mc_version, loader),
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
                    let fallback = match mc_version {
                        Some(mc) => instances::clean_pack_stem_for_mc(&filename, mc),
                        None => instances::clean_pack_stem(&filename),
                    };
                    (fallback, String::new())
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
    let mc_version_for_scan = mc_version.clone();
    let scan = tokio::task::spawn_blocking(move || {
        scan_mod_files(
            &packs_dir,
            is_mods,
            mc_version_for_scan.as_deref(),
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

    // Fallback to pack.mcmeta's explicit `pack.name` only. In Minecraft
    // `pack.description` is prose, not a title; using it here made the
    // Version column show a sentence instead of the pack name.
    if let Ok(file) = std::fs::File::open(path) {
        if let Ok(mut archive) = zip::ZipArchive::new(file) {
            if let Ok(mcmeta_file) = archive.by_name("pack.mcmeta") {
                let bytes: Vec<u8> = std::io::BufReader::new(mcmeta_file)
                    .bytes()
                    .filter_map(|b| b.ok())
                    .collect();
                if let Ok(content) = String::from_utf8(bytes) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                        let (name, _) = instances::read_pack_meta_from_json(&json);
                        if let Some(name) = name {
                            return Some((name, String::new()));
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
    /// Mod loaders advertised by the JAR descriptors (canonical: Fabric,
    /// Forge, NeoForge). This is independent of `provider`, which is the
    /// download source (Modrinth / CurseForge / Local).
    #[serde(default)]
    pub loaders: Vec<String>,
    /// `true` when `version` is a real value for the instance loader. A
    /// placeholder or a missing descriptor yields `false`, and the frontend
    /// shows a localized "Unknown" instead of an empty line.
    #[serde(default)]
    pub version_known: bool,
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
}

fn read_sidecar_fields(mods_dir: &std::path::Path, filename: &str) -> SidecarFields {
    match crate::instances::read_sidecar_meta(mods_dir, filename) {
        Some(json) => SidecarFields {
            project_id: json["project_id"].as_str().map(str::to_string),
            provider: json["provider"].as_str().map(str::to_string),
            project_name: json["project_name"].as_str().map(str::to_string),
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

// ==================== Local content import ====================

/// Files larger than this are refused as archives (a pack or mod is not a
/// multi-gigabyte blob). The limits apply to the uncompressed total and to a
/// single entry, not to the compressed download size.
const MAX_LOCAL_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_LOCAL_ARCHIVE_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_LOCAL_ARCHIVE_ENTRY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_LOCAL_STAGING_ATTEMPTS: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalConflictPolicy {
    /// Refuse to touch an existing file with different content.
    Reject,
    /// Replace the existing file, but only when the caller proves it saw the
    /// same existing content hash.
    Replace,
    /// Install under a fresh name instead of replacing anything.
    KeepBoth,
}

impl LocalConflictPolicy {
    fn parse(raw: Option<&str>) -> Result<Self, LocalImportError> {
        match raw.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(Self::Reject),
            Some(value) if value.eq_ignore_ascii_case("replace") => Ok(Self::Replace),
            Some(value)
                if value.eq_ignore_ascii_case("keepBoth")
                    || value.eq_ignore_ascii_case("keep_both") =>
            {
                Ok(Self::KeepBoth)
            }
            Some(other) => Err(LocalImportError::new(
                "invalidConflictPolicy",
                format!("Unknown conflict policy: {other}"),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalImportStatus {
    Installed,
    AlreadyInstalled,
    DuplicateContent,
    Conflict,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalImportResult {
    pub status: LocalImportStatus,
    pub filename: String,
    pub content_type: String,
    pub enabled: bool,
    /// Resource packs can be enabled through `options.txt`; mods use the
    /// `.disabled` suffix and shader activation is controlled by Iris/Oculus.
    pub activation_supported: bool,
    /// True for a resource pack that is installed but absent from
    /// `options.txt` (the "Resourcify downloaded it but it is not active"
    /// scenario).
    pub requires_activation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_with: Option<String>,
    /// Hash of the existing conflicting file. The frontend sends it back for
    /// an explicit replace, so a file changed in the meantime cannot be
    /// overwritten by accident.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_sha256: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalImportError {
    pub code: String,
    pub message: String,
}

impl LocalImportError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for LocalImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for LocalImportError {}

#[derive(Debug, Clone)]
struct PackArchiveLayout {
    /// Top-level folder to strip when the actual pack lives one level down.
    flatten_prefix: Option<String>,
}

fn content_root_marker(content_type: &str, name: &str) -> bool {
    match content_type {
        "resourcepack" => name == "pack.mcmeta",
        "shader" => name == "shaderpack.mcmeta" || name.starts_with("shaders/"),
        _ => false,
    }
}

fn normalized_archive_entry_name(raw: &str) -> Result<Option<String>, LocalImportError> {
    let name = raw.replace('\\', "/");
    let name = name.trim_start_matches("./");
    if name.is_empty() {
        return Ok(None);
    }
    if name.contains('\0')
        || name.starts_with('/')
        || name.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || (part.len() >= 2
                    && part.as_bytes()[0].is_ascii_alphabetic()
                    && part.as_bytes()[1] == b':')
        })
        || download::is_unsafe_archive_path(name)
    {
        return Err(LocalImportError::new(
            "unsafePath",
            format!("Archive contains an unsafe path: {raw}"),
        ));
    }
    Ok(Some(name.to_string()))
}

fn inspect_pack_archive(
    path: &std::path::Path,
    content_type: &str,
) -> Result<PackArchiveLayout, LocalImportError> {
    let file = std::fs::File::open(path).map_err(|error| {
        LocalImportError::new(
            "invalidArchive",
            format!("Cannot open local archive {}: {error}", path.display()),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        LocalImportError::new(
            "invalidArchive",
            format!("Invalid local archive {}: {error}", path.display()),
        )
    })?;
    if archive.len() > MAX_LOCAL_ARCHIVE_ENTRIES {
        return Err(LocalImportError::new(
            "archiveTooLarge",
            "The archive contains too many entries",
        ));
    }

    let mut entries: Vec<(String, u64)> = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            LocalImportError::new(
                "invalidArchive",
                format!("Invalid local archive entry: {error}"),
            )
        })?;
        if entry.is_symlink() {
            return Err(LocalImportError::new(
                "symlink",
                "The archive contains a symbolic link, refusing to import it",
            ));
        }
        if entry.is_dir() {
            continue;
        }
        let Some(name) = normalized_archive_entry_name(entry.name())? else {
            continue;
        };
        let size = entry.size();
        if size > MAX_LOCAL_ARCHIVE_ENTRY_BYTES {
            return Err(LocalImportError::new(
                "archiveTooLarge",
                format!("Archive entry is too large: {name}"),
            ));
        }
        total = total.saturating_add(size);
        if total > MAX_LOCAL_ARCHIVE_TOTAL_BYTES {
            return Err(LocalImportError::new(
                "archiveTooLarge",
                "The archive expands beyond the safe size limit",
            ));
        }
        entries.push((name, size));
    }
    if entries.is_empty() {
        return Err(LocalImportError::new(
            "emptyArchive",
            "The archive contains no files",
        ));
    }

    let root_marker = entries
        .iter()
        .any(|(name, _)| content_root_marker(content_type, name));
    let flatten_prefix = if root_marker {
        None
    } else {
        let mut candidates = std::collections::BTreeSet::new();
        for (name, _) in &entries {
            let Some((first, rest)) = name.split_once('/') else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            if content_root_marker(content_type, rest) {
                candidates.insert(first.to_string());
            }
        }
        match candidates.len() {
            0 => {
                return Err(LocalImportError::new(
                    "noRoot",
                    format!(
                        "Local {content_type} archive has no {} at its root",
                        if content_type == "resourcepack" {
                            "pack.mcmeta"
                        } else {
                            "shaders/ or shaderpack.mcmeta"
                        }
                    ),
                ));
            }
            1 => candidates.into_iter().next(),
            _ => {
                return Err(LocalImportError::new(
                    "multipleRoots",
                    format!(
                        "Local {content_type} archive contains several possible pack roots: {}",
                        candidates.into_iter().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        }
    };

    // A wrapper and root-level auxiliary files (README, LICENSE, docs, ...)
    // are flattened together. Refuse a collision instead of silently choosing
    // one of the two files.
    let mut destinations = std::collections::HashSet::new();
    for (name, _) in &entries {
        let destination = match &flatten_prefix {
            Some(prefix) => name
                .strip_prefix(&format!("{prefix}/"))
                .unwrap_or(name.as_str()),
            None => name.as_str(),
        };
        if destination.is_empty() {
            continue;
        }
        let key = destination.to_ascii_lowercase();
        if !destinations.insert(key) {
            return Err(LocalImportError::new(
                "pathCollision",
                format!("Archive paths collide after normalization: {destination}"),
            ));
        }
    }

    Ok(PackArchiveLayout { flatten_prefix })
}

/// Copy the archive into `dest`, stripping the wrapper prefix and preserving
/// safe root-level auxiliary files. The writer is deterministic (fixed
/// timestamp, deflate, sorted by the source order) so re-importing the same
/// source produces the same bytes and is detected as a no-op.
fn write_normalized_archive(
    source: &std::path::Path,
    dest: &std::path::Path,
    flatten_prefix: Option<&str>,
) -> Result<(), LocalImportError> {
    use std::io::Write as _;

    let file = std::fs::File::open(source).map_err(|error| {
        LocalImportError::new(
            "invalidArchive",
            format!("Cannot open local archive {}: {error}", source.display()),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        LocalImportError::new(
            "invalidArchive",
            format!("Invalid local archive {}: {error}", source.display()),
        )
    })?;
    if archive.len() > MAX_LOCAL_ARCHIVE_ENTRIES {
        return Err(LocalImportError::new(
            "archiveTooLarge",
            "The archive contains too many entries",
        ));
    }
    let output = std::fs::File::create(dest).map_err(|error| {
        LocalImportError::new(
            "installFailed",
            format!("Cannot create normalized archive: {error}"),
        )
    })?;
    let mut writer = zip::ZipWriter::new(output);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());

    let mut copied = 0usize;
    let mut total = 0u64;
    let mut actual_total = 0u64;
    let mut destinations = std::collections::HashSet::new();
    let result = (|| -> Result<(), LocalImportError> {
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|error| {
                LocalImportError::new(
                    "invalidArchive",
                    format!("Invalid local archive entry: {error}"),
                )
            })?;
            if entry.is_symlink() {
                return Err(LocalImportError::new(
                    "symlink",
                    "The archive contains a symbolic link, refusing to import it",
                ));
            }
            if entry.is_dir() {
                continue;
            }
            let Some(name) = normalized_archive_entry_name(entry.name())? else {
                continue;
            };
            let relative = match flatten_prefix {
                Some(prefix) => name
                    .strip_prefix(&format!("{prefix}/"))
                    .unwrap_or(name.as_str()),
                None => name.as_str(),
            };
            if relative.is_empty() {
                continue;
            }
            let key = relative.to_ascii_lowercase();
            if !destinations.insert(key) {
                return Err(LocalImportError::new(
                    "pathCollision",
                    format!("Archive paths collide after normalization: {relative}"),
                ));
            }
            let size = entry.size();
            if size > MAX_LOCAL_ARCHIVE_ENTRY_BYTES {
                return Err(LocalImportError::new(
                    "archiveTooLarge",
                    format!("Archive entry is too large: {relative}"),
                ));
            }
            total = total.saturating_add(size);
            if total > MAX_LOCAL_ARCHIVE_TOTAL_BYTES {
                return Err(LocalImportError::new(
                    "archiveTooLarge",
                    "The archive expands beyond the safe size limit",
                ));
            }
            writer.start_file(relative, options).map_err(|error| {
                LocalImportError::new(
                    "installFailed",
                    format!("Cannot normalize archive: {error}"),
                )
            })?;
            let mut buffer = [0u8; 64 * 1024];
            let mut entry_written = 0u64;
            loop {
                let read = entry.read(&mut buffer).map_err(|error| {
                    LocalImportError::new(
                        "invalidArchive",
                        format!("Cannot read local archive: {error}"),
                    )
                })?;
                if read == 0 {
                    break;
                }
                entry_written = entry_written.saturating_add(read as u64);
                actual_total = actual_total.saturating_add(read as u64);
                if entry_written > MAX_LOCAL_ARCHIVE_ENTRY_BYTES
                    || actual_total > MAX_LOCAL_ARCHIVE_TOTAL_BYTES
                {
                    return Err(LocalImportError::new(
                        "archiveTooLarge",
                        "The archive expands beyond the safe size limit",
                    ));
                }
                writer.write_all(&buffer[..read]).map_err(|error| {
                    LocalImportError::new(
                        "installFailed",
                        format!("Cannot write normalized archive: {error}"),
                    )
                })?;
            }
            copied += 1;
        }
        if copied == 0 {
            return Err(LocalImportError::new(
                "emptyArchive",
                "The archive contains no files at the pack root",
            ));
        }
        writer.finish().map_err(|error| {
            LocalImportError::new(
                "installFailed",
                format!("Cannot finish normalized archive: {error}"),
            )
        })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

#[cfg(test)]
fn local_archive_nested_prefix(
    path: &std::path::Path,
    content_type: &str,
) -> Result<Option<String>, String> {
    inspect_pack_archive(path, content_type)
        .map(|layout| layout.flatten_prefix)
        .map_err(|error| error.message)
}

#[cfg(test)]
fn repack_archive_under_prefix(path: &std::path::Path, prefix: &str) -> Result<(), String> {
    let temp = path.with_file_name(format!(
        ".{}.voidlauncher-normalize.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("archive")
    ));
    let _ = std::fs::remove_file(&temp);
    write_normalized_archive(path, &temp, Some(prefix)).map_err(|error| error.message)?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("Cannot replace local archive: {error}")
    })
}

fn validate_mod_archive(path: &std::path::Path) -> Result<(), LocalImportError> {
    download::verify_zip_magic(path).map_err(|error| {
        LocalImportError::new("invalidArchive", format!("Invalid JAR archive: {error}"))
    })?;
    let file = std::fs::File::open(path).map_err(|error| {
        LocalImportError::new(
            "invalidArchive",
            format!("Cannot open JAR archive {}: {error}", path.display()),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        LocalImportError::new(
            "invalidArchive",
            format!("Invalid JAR archive {}: {error}", path.display()),
        )
    })?;
    if archive.is_empty() {
        return Err(LocalImportError::new(
            "emptyArchive",
            "The JAR archive contains no files",
        ));
    }
    let mut has_file = false;
    let mut total = 0u64;
    let mut actual_total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            LocalImportError::new("invalidArchive", format!("Invalid JAR entry: {error}"))
        })?;
        if entry.is_symlink() {
            return Err(LocalImportError::new(
                "symlink",
                "The JAR contains a symbolic link, refusing to import it",
            ));
        }
        if entry.is_dir() {
            continue;
        }
        let size = entry.size();
        if size > MAX_LOCAL_ARCHIVE_ENTRY_BYTES {
            return Err(LocalImportError::new(
                "archiveTooLarge",
                format!("JAR entry is too large: {}", entry.name()),
            ));
        }
        total = total.saturating_add(size);
        if total > MAX_LOCAL_ARCHIVE_TOTAL_BYTES {
            return Err(LocalImportError::new(
                "archiveTooLarge",
                "The JAR expands beyond the safe size limit",
            ));
        }
        has_file = true;
        // Read every entry to the end: this verifies the compressed stream
        // and CRC, not just the central directory. A truncated JAR with an
        // intact index would otherwise look installable.
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = entry.read(&mut buffer).map_err(|error| {
                LocalImportError::new(
                    "invalidArchive",
                    format!("Cannot read JAR entry {}: {error}", entry.name()),
                )
            })?;
            if read == 0 {
                break;
            }
            actual_total = actual_total.saturating_add(read as u64);
            if actual_total > MAX_LOCAL_ARCHIVE_TOTAL_BYTES {
                return Err(LocalImportError::new(
                    "archiveTooLarge",
                    "The JAR expands beyond the safe size limit",
                ));
            }
        }
    }
    if !has_file {
        return Err(LocalImportError::new(
            "emptyArchive",
            "The JAR archive contains no files",
        ));
    }
    Ok(())
}

fn validate_archive_entries_readable(path: &std::path::Path) -> Result<(), LocalImportError> {
    let file = std::fs::File::open(path).map_err(|error| {
        LocalImportError::new(
            "invalidArchive",
            format!("Cannot open local archive {}: {error}", path.display()),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        LocalImportError::new(
            "invalidArchive",
            format!("Invalid local archive {}: {error}", path.display()),
        )
    })?;
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            LocalImportError::new(
                "invalidArchive",
                format!("Invalid local archive entry: {error}"),
            )
        })?;
        if entry.is_symlink() {
            return Err(LocalImportError::new(
                "symlink",
                "The archive contains a symbolic link, refusing to import it",
            ));
        }
        if entry.is_dir() {
            continue;
        }
        let mut buffer = [0u8; 64 * 1024];
        let mut entry_total = 0u64;
        loop {
            let read = entry.read(&mut buffer).map_err(|error| {
                LocalImportError::new(
                    "invalidArchive",
                    format!("Cannot read local archive entry: {error}"),
                )
            })?;
            if read == 0 {
                break;
            }
            entry_total = entry_total.saturating_add(read as u64);
            total = total.saturating_add(read as u64);
            if entry_total > MAX_LOCAL_ARCHIVE_ENTRY_BYTES {
                return Err(LocalImportError::new(
                    "archiveTooLarge",
                    "An archive entry is too large",
                ));
            }
            if total > MAX_LOCAL_ARCHIVE_TOTAL_BYTES {
                return Err(LocalImportError::new(
                    "archiveTooLarge",
                    "The archive expands beyond the safe size limit",
                ));
            }
        }
    }
    Ok(())
}

fn local_directory_has_marker(path: &std::path::Path, content_type: &str) -> bool {
    match content_type {
        "resourcepack" => path.join("pack.mcmeta").is_file(),
        "shader" => path.join("shaderpack.mcmeta").is_file() || path.join("shaders").is_dir(),
        _ => false,
    }
}

#[derive(Debug, Clone)]
struct PackDirectoryLayout {
    flatten_root: Option<std::path::PathBuf>,
}

fn inspect_pack_directory(
    source: &std::path::Path,
    content_type: &str,
) -> Result<PackDirectoryLayout, LocalImportError> {
    if local_directory_has_marker(source, content_type) {
        return Ok(PackDirectoryLayout { flatten_root: None });
    }
    let entries = std::fs::read_dir(source).map_err(|error| {
        LocalImportError::new(
            "readFailed",
            format!("Cannot read local directory {}: {error}", source.display()),
        )
    })?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            return Err(LocalImportError::new(
                "symlink",
                format!(
                    "Local content contains a symbolic link, refusing to copy: {}",
                    entry.path().display()
                ),
            ));
        }
        if file_type.is_dir() && local_directory_has_marker(&entry.path(), content_type) {
            candidates.push(entry.path());
        }
    }
    match candidates.len() {
        0 => Err(LocalImportError::new(
            "noRoot",
            format!(
                "Local {content_type} folder has no {} at its root",
                if content_type == "resourcepack" {
                    "pack.mcmeta"
                } else {
                    "shaders/ or shaderpack.mcmeta"
                }
            ),
        )),
        1 => Ok(PackDirectoryLayout {
            flatten_root: candidates.into_iter().next(),
        }),
        _ => Err(LocalImportError::new(
            "multipleRoots",
            format!(
                "Local {content_type} folder contains several possible pack roots: {}",
                candidates
                    .iter()
                    .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

#[cfg(test)]
fn local_directory_root(
    source: &std::path::Path,
    content_type: &str,
) -> Result<std::path::PathBuf, String> {
    inspect_pack_directory(source, content_type)
        .map(|layout| layout.flatten_root.unwrap_or_else(|| source.to_path_buf()))
        .map_err(|error| error.message)
}

fn remove_local_path(path: &std::path::Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

fn validate_local_directory_size(
    root: &std::path::Path,
    limit: u64,
) -> Result<u64, LocalImportError> {
    fn collect(
        path: &std::path::Path,
        limit: u64,
        total: &mut u64,
    ) -> Result<(), LocalImportError> {
        let entries = std::fs::read_dir(path).map_err(|error| {
            LocalImportError::new(
                "readFailed",
                format!("Cannot read {}: {error}", path.display()),
            )
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                LocalImportError::new(
                    "readFailed",
                    format!("Cannot inspect {}: {error}", path.display()),
                )
            })?;
            if file_type.is_symlink() {
                return Err(LocalImportError::new(
                    "symlink",
                    format!(
                        "Local content contains a symbolic link, refusing to copy: {}",
                        path.display()
                    ),
                ));
            }
            if file_type.is_dir() {
                collect(&path, limit, total)?;
            } else if file_type.is_file() {
                let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                *total = total.saturating_add(size);
                if *total > limit {
                    return Err(LocalImportError::new(
                        "archiveTooLarge",
                        "The pack folder exceeds the safe size limit",
                    ));
                }
            }
        }
        Ok(())
    }

    let mut total = 0u64;
    collect(root, limit, &mut total)?;
    Ok(total)
}

fn copy_local_directory(source: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("Cannot create {}: {e}", dest.display()))?;
    let entries =
        std::fs::read_dir(source).map_err(|e| format!("Cannot read {}: {e}", source.display()))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Cannot inspect {}: {e}", from.display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "Local content contains a symbolic link, refusing to copy: {}",
                from.display()
            ));
        }
        if file_type.is_dir() {
            copy_local_directory(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("Cannot copy {} to {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn copy_local_directory_flattened(
    source: &std::path::Path,
    layout: &PackDirectoryLayout,
    dest: &std::path::Path,
) -> Result<(), LocalImportError> {
    let Some(wrapper) = &layout.flatten_root else {
        return copy_local_directory(source, dest)
            .map_err(|message| LocalImportError::new("installFailed", message));
    };

    copy_local_directory(wrapper, dest)
        .map_err(|message| LocalImportError::new("installFailed", message))?;

    // Preserve root-level auxiliary files (README, LICENSE, docs, ...) next
    // to the flattened pack. A collision is an error, not a silent overwrite.
    let entries = std::fs::read_dir(source).map_err(|error| {
        LocalImportError::new(
            "readFailed",
            format!("Cannot read local directory {}: {error}", source.display()),
        )
    })?;
    for entry in entries.flatten() {
        if entry.path() == *wrapper {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| {
            LocalImportError::new(
                "readFailed",
                format!("Cannot inspect {}: {error}", entry.path().display()),
            )
        })?;
        if file_type.is_symlink() {
            return Err(LocalImportError::new(
                "symlink",
                format!(
                    "Local content contains a symbolic link, refusing to copy: {}",
                    entry.path().display()
                ),
            ));
        }
        let to = dest.join(entry.file_name());
        if to.exists() {
            return Err(LocalImportError::new(
                "pathCollision",
                format!(
                    "Local files collide after normalization: {}",
                    entry.file_name().to_string_lossy()
                ),
            ));
        }
        if file_type.is_dir() {
            copy_local_directory(&entry.path(), &to)
                .map_err(|message| LocalImportError::new("installFailed", message))?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &to).map_err(|error| {
                LocalImportError::new("installFailed", format!("Cannot copy local file: {error}"))
            })?;
        }
    }
    Ok(())
}

fn local_staging_path(content_dir: &std::path::Path, is_dir: bool) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..MAX_LOCAL_STAGING_ATTEMPTS {
        let suffix = if is_dir { "dir" } else { "tmp" };
        let candidate = content_dir.join(format!(
            ".voidlauncher-import-{}-{stamp}-{attempt}.{suffix}",
            std::process::id()
        ));
        if !candidate.exists() {
            return candidate;
        }
    }
    content_dir.join(format!(
        ".voidlauncher-import-{}-{stamp}.fallback",
        std::process::id()
    ))
}

fn hash_local_file(path: &std::path::Path) -> Result<String, LocalImportError> {
    download::hash_file_sha256(path).map_err(|error| {
        LocalImportError::new(
            "readFailed",
            format!("Cannot hash {}: {error}", path.display()),
        )
    })
}

fn hash_local_directory(path: &std::path::Path) -> Result<(String, u64), LocalImportError> {
    use sha2::{Digest, Sha256};

    fn collect(
        root: &std::path::Path,
        current: &std::path::Path,
        files: &mut Vec<(String, std::path::PathBuf, u64)>,
        total: &mut u64,
    ) -> Result<(), LocalImportError> {
        let entries = std::fs::read_dir(current).map_err(|error| {
            LocalImportError::new(
                "readFailed",
                format!("Cannot read {}: {error}", current.display()),
            )
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                LocalImportError::new(
                    "readFailed",
                    format!("Cannot inspect {}: {error}", path.display()),
                )
            })?;
            if file_type.is_symlink() {
                return Err(LocalImportError::new(
                    "symlink",
                    format!(
                        "Local content contains a symbolic link, refusing to copy: {}",
                        path.display()
                    ),
                ));
            }
            if file_type.is_dir() {
                collect(root, &path, files, total)?;
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let size = std::fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                *total = total.saturating_add(size);
                files.push((relative, path, size));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    let mut total = 0u64;
    collect(path, path, &mut files, &mut total)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (relative, file_path, size) in files {
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        hasher.update(size.to_le_bytes());
        let mut file = std::fs::File::open(&file_path).map_err(|error| {
            LocalImportError::new(
                "readFailed",
                format!("Cannot read {}: {error}", file_path.display()),
            )
        })?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                LocalImportError::new(
                    "readFailed",
                    format!("Cannot read {}: {error}", file_path.display()),
                )
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok((hex::encode(hasher.finalize()), total))
}

struct ExistingLocalEntry {
    filename: String,
    path: std::path::PathBuf,
    is_dir: bool,
    size: u64,
}

fn list_existing_local_entries(
    content_dir: &std::path::Path,
    content_type: &str,
) -> Result<Vec<ExistingLocalEntry>, LocalImportError> {
    let mut entries = Vec::new();
    let read_dir = match std::fs::read_dir(content_dir) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => {
            return Err(LocalImportError::new(
                "readFailed",
                format!("Cannot read content folder: {error}"),
            ));
        }
    };
    for entry in read_dir.flatten() {
        let filename = entry.file_name().to_string_lossy().to_string();
        if filename.starts_with('.')
            || filename.ends_with(".voidlauncher.json")
            || filename.ends_with(".pw.toml")
        {
            continue;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let lower = filename.to_ascii_lowercase();
        let matches = match content_type {
            "mod" => {
                !file_type.is_dir() && (lower.ends_with(".jar") || lower.ends_with(".jar.disabled"))
            }
            "resourcepack" | "shader" => {
                file_type.is_dir() || lower.ends_with(".zip") || lower.ends_with(".zip.disabled")
            }
            _ => false,
        };
        if !matches {
            continue;
        }
        let size = if file_type.is_dir() {
            0
        } else {
            std::fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        };
        entries.push(ExistingLocalEntry {
            filename,
            path,
            is_dir: file_type.is_dir(),
            size,
        });
    }
    entries.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(entries)
}

fn find_same_name_target(
    content_dir: &std::path::Path,
    content_type: &str,
    clean_name: &str,
) -> Result<Option<ExistingLocalEntry>, LocalImportError> {
    let disabled = format!("{clean_name}.disabled");
    let mut matches = list_existing_local_entries(content_dir, content_type)?
        .into_iter()
        .filter(|entry| {
            entry.filename.eq_ignore_ascii_case(clean_name)
                || entry.filename.eq_ignore_ascii_case(&disabled)
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(LocalImportError::new(
            "ambiguousTarget",
            format!(
                "Both enabled and disabled versions of {clean_name} exist; resolve them manually first"
            ),
        ));
    }
    Ok(matches.pop())
}

fn normalize_import_filename(name: &str, content_type: &str, is_dir: bool) -> String {
    if is_dir {
        return name.to_string();
    }
    let extension = if content_type == "mod" {
        ".jar"
    } else {
        ".zip"
    };
    if name
        .get(name.len().saturating_sub(extension.len())..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(extension))
    {
        format!("{}{}", &name[..name.len() - extension.len()], extension)
    } else {
        name.to_string()
    }
}

fn unique_local_name(
    content_dir: &std::path::Path,
    content_type: &str,
    clean_name: &str,
    is_dir: bool,
    force_disabled: bool,
) -> Result<String, LocalImportError> {
    let (stem, extension) = if is_dir {
        (clean_name.to_string(), String::new())
    } else {
        let extension = if content_type == "mod" {
            ".jar"
        } else {
            ".zip"
        };
        let stem = clean_name
            .strip_suffix(extension)
            .unwrap_or(clean_name)
            .to_string();
        (stem, extension.to_string())
    };
    for index in 2..MAX_LOCAL_STAGING_ATTEMPTS {
        let candidate = format!("{stem} ({index}){extension}");
        let disabled = format!("{candidate}.disabled");
        if !content_dir.join(&candidate).exists() && !content_dir.join(&disabled).exists() {
            return Ok(if force_disabled { disabled } else { candidate });
        }
    }
    Err(LocalImportError::new(
        "installFailed",
        "Cannot find a free file name for the imported content",
    ))
}

fn local_activation_state(
    content_type: &str,
    filename: &str,
    options_path: Option<&std::path::Path>,
) -> (bool, bool, bool) {
    match content_type {
        "resourcepack" => {
            let active = options_path
                .map(instances::read_options_resource_packs)
                .map(|(packs, _)| packs)
                .unwrap_or_default();
            let enabled = instances::is_pack_enabled_in_list(filename, &active);
            (enabled, true, !enabled)
        }
        "mod" => (
            !filename.to_ascii_lowercase().ends_with(".disabled"),
            false,
            false,
        ),
        "shader" => (
            !filename.to_ascii_lowercase().ends_with(".disabled"),
            false,
            false,
        ),
        _ => (true, false, false),
    }
}

fn local_import_result(
    status: LocalImportStatus,
    filename: String,
    content_type: &str,
    options_path: Option<&std::path::Path>,
) -> LocalImportResult {
    let (enabled, activation_supported, requires_activation) =
        local_activation_state(content_type, &filename, options_path);
    LocalImportResult {
        status,
        filename,
        content_type: content_type.to_string(),
        enabled,
        activation_supported,
        requires_activation,
        duplicate_of: None,
        conflict_with: None,
        existing_sha256: None,
    }
}

fn restore_local_stash(stashed: &[(std::path::PathBuf, std::path::PathBuf)]) {
    for (original, parked) in stashed {
        if parked.exists() {
            remove_local_path(original);
            let _ = std::fs::rename(parked, original);
        }
    }
}

fn discard_local_stash(stashed: &[(std::path::PathBuf, std::path::PathBuf)]) {
    for (_, parked) in stashed {
        remove_local_path(parked);
    }
}

fn stash_existing_target(
    content_dir: &std::path::Path,
    target: &std::path::Path,
) -> Result<Vec<(std::path::PathBuf, std::path::PathBuf)>, LocalImportError> {
    let target_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| LocalImportError::new("installFailed", "Invalid target file name"))?;
    if target.is_dir() {
        let parked = content_dir.join(format!("{}{}", target_name, crate::instances::STASH_SUFFIX));
        remove_local_path(&parked);
        std::fs::rename(target, &parked).map_err(|error| {
            LocalImportError::new(
                "installFailed",
                format!("Cannot park existing content: {error}"),
            )
        })?;
        Ok(vec![(target.to_path_buf(), parked)])
    } else {
        crate::instances::stash_old_content_files(content_dir, target_name)
            .map_err(|error| LocalImportError::new("installFailed", error))
    }
}

fn validate_existing_local_source(
    source: &std::path::Path,
    content_type: &str,
) -> Result<(), LocalImportError> {
    let metadata = std::fs::metadata(source).map_err(|error| {
        LocalImportError::new("notFound", format!("Local file not found: {error}"))
    })?;
    if metadata.is_dir() {
        if content_type == "mod" {
            return Err(LocalImportError::new(
                "modMustBeJar",
                "Mods must be .jar archives, not folders",
            ));
        }
        if !local_directory_has_marker(source, content_type) {
            return Err(LocalImportError::new(
                "noRoot",
                "The pack folder has no pack marker at its root",
            ));
        }
        let (_, total) = hash_local_directory(source)?;
        if total > MAX_LOCAL_ARCHIVE_TOTAL_BYTES {
            return Err(LocalImportError::new(
                "archiveTooLarge",
                "The pack folder exceeds the safe size limit",
            ));
        }
        return Ok(());
    }
    if content_type == "mod" {
        validate_mod_archive(source)
    } else {
        let layout = inspect_pack_archive(source, content_type)?;
        if layout.flatten_prefix.is_some() {
            return Err(LocalImportError::new(
                "invalidLayout",
                "The archive has an extra top-level folder; move it out of the content folder and import it again so the launcher can normalize it",
            ));
        }
        validate_archive_entries_readable(source)
    }
}

fn install_local_content_into(
    content_dir: &std::path::Path,
    options_path: Option<&std::path::Path>,
    source: &std::path::Path,
    content_type: &str,
    conflict: LocalConflictPolicy,
    expected_existing_sha256: Option<&str>,
) -> Result<LocalImportResult, LocalImportError> {
    if !matches!(content_type, "mod" | "resourcepack" | "shader") {
        return Err(LocalImportError::new(
            "unsupportedType",
            format!("Unsupported content type: {content_type}"),
        ));
    }
    let base = content_dir.parent().unwrap_or(content_dir);
    crate::import::ensure_no_reparse_points(base, content_dir).map_err(|error| {
        LocalImportError::new(
            "installFailed",
            format!("Refusing to write through a junction/symlink: {error}"),
        )
    })?;
    std::fs::create_dir_all(content_dir).map_err(|error| {
        LocalImportError::new(
            "installFailed",
            format!("Cannot create content folder: {error}"),
        )
    })?;
    let content_dir = content_dir.canonicalize().map_err(|error| {
        LocalImportError::new(
            "installFailed",
            format!("Cannot resolve content folder: {error}"),
        )
    })?;
    // Roll back a directory parked by an interrupted replacement before any
    // new operation starts (the listings already do this for files).
    crate::instances::recover_stashed_content_files(&content_dir);
    let source_metadata = std::fs::metadata(source).map_err(|error| {
        LocalImportError::new("notFound", format!("Local file not found: {error}"))
    })?;
    // Keep the user-visible name (including a symlink's name and original
    // casing) even though content operations use the canonical target.
    let source_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| *name != "." && *name != "..")
        .ok_or_else(|| LocalImportError::new("notFound", "Invalid local file name"))?
        .to_string();
    let source = source.canonicalize().map_err(|error| {
        LocalImportError::new("notFound", format!("Cannot resolve local path: {error}"))
    })?;

    // A file/folder that already lives in the content directory (Resourcify
    // downloads straight into resourcepacks) must never be copied onto
    // itself or parked/restored. Validate it read-only and report its state.
    if source.starts_with(&content_dir) {
        if source.parent() != Some(content_dir.as_path()) {
            return Err(LocalImportError::new(
                "sourceInsideContent",
                "The selected path is inside the content folder but not at its root",
            ));
        }
        validate_existing_local_source(&source, content_type)?;
        return Ok(local_import_result(
            LocalImportStatus::AlreadyInstalled,
            source_name,
            content_type,
            options_path,
        ));
    }
    if content_dir.starts_with(&source) {
        return Err(LocalImportError::new(
            "sourceContainsContent",
            "The selected folder contains the instance content folder",
        ));
    }

    let is_dir = source_metadata.is_dir();
    if !is_dir && source_metadata.len() > MAX_LOCAL_ARCHIVE_TOTAL_BYTES {
        return Err(LocalImportError::new(
            "archiveTooLarge",
            "The selected archive exceeds the safe size limit",
        ));
    }
    let lower_name = source_name.to_ascii_lowercase();
    if content_type == "mod" {
        if is_dir {
            return Err(LocalImportError::new(
                "modMustBeJar",
                "Mods must be .jar archives, not folders",
            ));
        }
        if !lower_name.ends_with(".jar") && !lower_name.ends_with(".jar.disabled") {
            return Err(LocalImportError::new(
                "modMustBeJar",
                "Mods must be .jar archives",
            ));
        }
    } else if !is_dir && !lower_name.ends_with(".zip") && !lower_name.ends_with(".zip.disabled") {
        return Err(LocalImportError::new(
            "packMustBeZip",
            "Resource packs and shaders must be .zip archives or folders",
        ));
    }

    let force_disabled = lower_name.ends_with(".disabled");
    let clean_name = if force_disabled {
        &source_name[..source_name.len() - ".disabled".len()]
    } else {
        &source_name
    };
    let clean_name = normalize_import_filename(clean_name, content_type, is_dir);
    if clean_name.is_empty() {
        return Err(LocalImportError::new("notFound", "Invalid local file name"));
    }

    // Stage and validate everything before touching the live content folder.
    let staging = local_staging_path(&content_dir, is_dir);
    remove_local_path(&staging);
    let staged_hash = (|| -> Result<String, LocalImportError> {
        if is_dir {
            validate_local_directory_size(&source, MAX_LOCAL_ARCHIVE_TOTAL_BYTES)?;
            let layout = inspect_pack_directory(&source, content_type)?;
            copy_local_directory_flattened(&source, &layout, &staging)?;
            if !local_directory_has_marker(&staging, content_type) {
                return Err(LocalImportError::new(
                    "noRoot",
                    "The normalized pack folder has no pack marker at its root",
                ));
            }
            let (hash, total) = hash_local_directory(&staging)?;
            if total > MAX_LOCAL_ARCHIVE_TOTAL_BYTES {
                return Err(LocalImportError::new(
                    "archiveTooLarge",
                    "The pack folder exceeds the safe size limit",
                ));
            }
            Ok(hash)
        } else if content_type == "mod" {
            std::fs::copy(&source, &staging).map_err(|error| {
                LocalImportError::new("installFailed", format!("Cannot copy local mod: {error}"))
            })?;
            validate_mod_archive(&staging)?;
            hash_local_file(&staging)
        } else {
            let layout = inspect_pack_archive(&source, content_type)?;
            write_normalized_archive(&source, &staging, layout.flatten_prefix.as_deref())?;
            let normalized = inspect_pack_archive(&staging, content_type)?;
            if normalized.flatten_prefix.is_some() {
                return Err(LocalImportError::new(
                    "invalidLayout",
                    "The normalized archive still has a wrapper folder",
                ));
            }
            hash_local_file(&staging)
        }
    })();
    let staged_hash = match staged_hash {
        Ok(hash) => hash,
        Err(error) => {
            remove_local_path(&staging);
            return Err(error);
        }
    };

    let cleanup = |error: LocalImportError| -> LocalImportError {
        remove_local_path(&staging);
        error
    };

    let existing_target = match find_same_name_target(&content_dir, content_type, &clean_name) {
        Ok(target) => target,
        Err(error) => return Err(cleanup(error)),
    };

    let staged_is_dir = is_dir;
    let mut target_name = match &existing_target {
        Some(target) => target.filename.clone(),
        None if force_disabled => format!("{clean_name}.disabled"),
        None => clean_name.clone(),
    };
    let mut replace_target = false;

    if let Some(target) = &existing_target {
        if target.is_dir != staged_is_dir {
            return Err(cleanup(LocalImportError::new(
                "ambiguousTarget",
                format!(
                    "{} exists as a different kind of content; remove it manually first",
                    target.filename
                ),
            )));
        }
        let existing_hash = if target.is_dir {
            hash_local_directory(&target.path).map(|(hash, _)| hash)
        } else {
            hash_local_file(&target.path)
        };
        let existing_hash = match existing_hash {
            Ok(hash) => hash,
            Err(error) => return Err(cleanup(error)),
        };
        if existing_hash == staged_hash {
            remove_local_path(&staging);
            return Ok(local_import_result(
                LocalImportStatus::AlreadyInstalled,
                target.filename.clone(),
                content_type,
                options_path,
            ));
        }
        match conflict {
            LocalConflictPolicy::Replace => {
                if expected_existing_sha256 != Some(existing_hash.as_str()) {
                    remove_local_path(&staging);
                    let mut result = local_import_result(
                        LocalImportStatus::Conflict,
                        target.filename.clone(),
                        content_type,
                        options_path,
                    );
                    result.conflict_with = Some(target.filename.clone());
                    result.existing_sha256 = Some(existing_hash);
                    return Ok(result);
                }
                replace_target = true;
            }
            LocalConflictPolicy::KeepBoth => {
                let keep_disabled =
                    force_disabled || target.filename.to_ascii_lowercase().ends_with(".disabled");
                target_name = match unique_local_name(
                    &content_dir,
                    content_type,
                    &clean_name,
                    staged_is_dir,
                    keep_disabled,
                ) {
                    Ok(name) => name,
                    Err(error) => return Err(cleanup(error)),
                };
            }
            LocalConflictPolicy::Reject => {
                remove_local_path(&staging);
                let mut result = local_import_result(
                    LocalImportStatus::Conflict,
                    target.filename.clone(),
                    content_type,
                    options_path,
                );
                result.conflict_with = Some(target.filename.clone());
                result.existing_sha256 = Some(existing_hash);
                return Ok(result);
            }
        }
    }

    // Same content under a different name is a possible duplicate. Do not
    // silently add a second copy; the user can explicitly choose KeepBoth.
    if !replace_target {
        let entries = match list_existing_local_entries(&content_dir, content_type) {
            Ok(entries) => entries,
            Err(error) => return Err(cleanup(error)),
        };
        let mut duplicate = None;
        for entry in entries {
            if existing_target
                .as_ref()
                .is_some_and(|target| target.filename == entry.filename)
            {
                continue;
            }
            if entry.is_dir != staged_is_dir {
                continue;
            }
            if !entry.is_dir
                && entry.size != std::fs::metadata(&staging).map(|m| m.len()).unwrap_or(0)
            {
                continue;
            }
            let hash = if entry.is_dir {
                hash_local_directory(&entry.path).map(|(hash, _)| hash)
            } else {
                hash_local_file(&entry.path)
            };
            if hash.ok().as_deref() == Some(staged_hash.as_str()) {
                duplicate = Some(entry);
                break;
            }
        }
        if let Some(duplicate) = duplicate {
            if conflict != LocalConflictPolicy::KeepBoth {
                remove_local_path(&staging);
                let mut result = local_import_result(
                    LocalImportStatus::DuplicateContent,
                    duplicate.filename.clone(),
                    content_type,
                    options_path,
                );
                result.duplicate_of = Some(duplicate.filename);
                return Ok(result);
            }
        }
    }

    let target_path = content_dir.join(&target_name);
    if !replace_target && target_path.exists() {
        remove_local_path(&staging);
        return Err(LocalImportError::new(
            "ambiguousTarget",
            format!(
                "{} appeared while the content was being prepared; reload and try again",
                target_name
            ),
        ));
    }
    let stashed = if replace_target {
        match stash_existing_target(&content_dir, &target_path) {
            Ok(stashed) => stashed,
            Err(error) => return Err(cleanup(error)),
        }
    } else {
        Vec::new()
    };
    if target_path.exists() {
        restore_local_stash(&stashed);
        remove_local_path(&staging);
        return Err(LocalImportError::new(
            "ambiguousTarget",
            format!(
                "{} appeared while the content was being prepared; reload and try again",
                target_name
            ),
        ));
    }

    if let Err(error) = std::fs::rename(&staging, &target_path) {
        restore_local_stash(&stashed);
        remove_local_path(&staging);
        return Err(LocalImportError::new(
            "installFailed",
            format!("Cannot install local content: {error}"),
        ));
    }
    discard_local_stash(&stashed);

    let hash = download::hash_file_sha1(&target_path).ok();
    let _ = content::write_sidecar(
        &content_dir,
        &target_name,
        &content::SidecarWrite {
            provider: "local",
            content_type,
            project_id: None,
            project_name: None,
            version_id: None,
            version_number: None,
            file_id: None,
            hash: hash.as_deref(),
        },
    );

    Ok(local_import_result(
        LocalImportStatus::Installed,
        target_name,
        content_type,
        options_path,
    ))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_install_local_content(
    state: State<'_, AppState>,
    instance_name: String,
    source_path: String,
    subfolder: String,
    conflict: Option<String>,
    expected_existing_sha256: Option<String>,
) -> Result<LocalImportResult, LocalImportError> {
    validate_instance_name(&instance_name)
        .map_err(|error| LocalImportError::new("invalidInstance", error))?;
    let _guard =
        crate::commands::instances::InstanceMutationGuard::acquire(state.inner(), &instance_name)
            .map_err(|error| LocalImportError::new("installFailed", error))?;
    let subfolder = subfolder.trim_matches(['/', '\\']).to_ascii_lowercase();
    if !matches!(subfolder.as_str(), "mods" | "resourcepacks" | "shaderpacks") {
        return Err(LocalImportError::new(
            "unsupportedType",
            format!("Subfolder '{subfolder}' is not allowed"),
        ));
    }
    let content_type = content::content_type_for_dir(&subfolder);
    let policy = LocalConflictPolicy::parse(conflict.as_deref())?;
    let (content_dir, options_path) = {
        let config = state.config.lock().map_err(|error| {
            LocalImportError::new("installFailed", format!("Config lock failed: {error}"))
        })?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|error| LocalImportError::new("invalidInstance", error.to_string()))?;
        let minecraft_dir = instance.minecraft_dir(&config.instances_dir());
        (
            minecraft_dir.join(&subfolder),
            minecraft_dir.join("options.txt"),
        )
    };
    let source = std::path::PathBuf::from(source_path);
    tauri::async_runtime::spawn_blocking(move || {
        install_local_content_into(
            &content_dir,
            Some(&options_path),
            &source,
            content_type,
            policy,
            expected_existing_sha256.as_deref(),
        )
    })
    .await
    .map_err(|error| LocalImportError::new("installFailed", error.to_string()))?
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
    let _guard =
        crate::commands::instances::InstanceMutationGuard::acquire(state.inner(), &instance_name)?;

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
        let content_type = content::content_type_for_dir(&safe_subfolder);
        let options_path = content_dir
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("options.txt");
        let result = install_local_content_into(
            &content_dir,
            Some(&options_path),
            &local_path,
            content_type,
            LocalConflictPolicy::Reject,
            None,
        )
        .map_err(|error| error.message)?;
        return match result.status {
            LocalImportStatus::Installed | LocalImportStatus::AlreadyInstalled => {
                Ok(result.filename)
            }
            LocalImportStatus::DuplicateContent => Err(format!(
                "{} is already installed as {}",
                local_path.display(),
                result.duplicate_of.unwrap_or_default()
            )),
            LocalImportStatus::Conflict => Err(format!(
                "{} already exists with different content",
                result.filename
            )),
        };
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
    if let Some(base) = content_dir.parent() {
        crate::import::ensure_no_reparse_points(base, &dest).map_err(|e| e.to_string())?;
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
    let _guard =
        crate::commands::instances::InstanceMutationGuard::acquire(state.inner(), &instance_name)?;
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
    if let Some(base) = dest_dir.parent() {
        crate::import::ensure_no_reparse_points(base, &dest).map_err(|e| e.to_string())?;
    }
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
        let loader = match &instance.loader {
            instances::LoaderType::Fabric => Some("Fabric"),
            instances::LoaderType::Forge => Some("Forge"),
            instances::LoaderType::NeoForge => Some("NeoForge"),
            instances::LoaderType::Vanilla => Some("Vanilla"),
        };
        let mc_version = instance.mc_version.as_str();
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
                let (jar_version, jar_version_known) = meta.version_for(loader, Some(mc_version));
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
                let version = if jar_version_known {
                    jar_version
                } else {
                    pw.filter(|p| !p.version.is_empty())
                        .map(|p| p.version.clone())
                        .unwrap_or(jar_version)
                };
                let version = normalize_mod_version(&version, Some(mc_version), loader);
                let version_known = !version.is_empty();
                mods.push(ModMetadata {
                    filename,
                    name,
                    version,
                    provider,
                    loaders: meta.loaders.clone(),
                    version_known,
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
    let _guard =
        crate::commands::instances::InstanceMutationGuard::acquire(state.inner(), &instance_name)?;
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
        if let Some(base) = mods_dir.parent() {
            crate::import::ensure_no_reparse_points(base, &mod_path).map_err(|e| e.to_string())?;
        }
        if mod_path.exists() {
            std::fs::remove_file(&mod_path).map_err(|e| e.to_string())?;
        }
        crate::instances::remove_sidecar_meta(&mods_dir, &safe_name);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

struct ModProjectRef {
    filename: String,
    provider: String,
    project_id: String,
}

fn collect_mod_project_refs(
    config: &AppConfig,
    instance_name: &str,
) -> crate::error::Result<Vec<ModProjectRef>> {
    let instance = instances::get_instance(&config.instances_dir(), instance_name)?;
    let mods_dir = instance.minecraft_dir(&config.instances_dir()).join("mods");
    let mut refs = Vec::new();
    let Ok(entries) = std::fs::read_dir(&mods_dir) else {
        return Ok(refs);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(filename.ends_with(".jar") || filename.ends_with(".jar.disabled")) {
            continue;
        }
        let sidecar = read_sidecar_fields(&mods_dir, filename);
        let provider = sidecar
            .provider
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let Some(project_id) = sidecar.project_id.filter(|id| !id.trim().is_empty()) else {
            continue;
        };
        if provider != "curseforge" && provider != "modrinth" {
            continue;
        }
        refs.push(ModProjectRef {
            filename: filename.to_string(),
            provider,
            project_id,
        });
    }
    Ok(refs)
}

/// Resolve provider project titles for the installed mods. Runs after the
/// local list is already visible: the jar parser provides an instant fallback,
/// and this command replaces it with the canonical CurseForge/Modrinth title
/// once the (persistent) project-name cache has been populated.
#[tauri::command]
pub async fn cmd_resolve_mod_project_names(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<std::collections::HashMap<String, String>, String> {
    let config = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        guard.clone()
    };
    let scan_config = config.clone();
    let refs = tauri::async_runtime::spawn_blocking(move || {
        collect_mod_project_refs(&scan_config, &instance_name)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    if refs.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let cached = project_name_cache(&config)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let mut curseforge_ids: Vec<u64> = Vec::new();
    let mut modrinth_ids: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in &refs {
        if !seen.insert(project_name_key(&r.provider, &r.project_id)) {
            continue;
        }
        if cached.contains_key(&project_name_key(&r.provider, &r.project_id)) {
            continue;
        }
        match r.provider.as_str() {
            "curseforge" => {
                if let Ok(id) = r.project_id.parse::<u64>() {
                    curseforge_ids.push(id);
                }
            }
            "modrinth" => modrinth_ids.push(r.project_id.clone()),
            _ => {}
        }
    }

    let mut fetched = ProjectNameCache::new();
    const BATCH: usize = 50;
    if !config.curseforge_api_key.is_empty() {
        for chunk in curseforge_ids.chunks(BATCH) {
            match curseforge::get_mods_by_ids(chunk, &config.curseforge_api_key).await {
                Ok(projects) => {
                    for p in projects {
                        if let Some(name) = clean_project_title(&p.name) {
                            fetched.insert(project_name_key("curseforge", &p.id.to_string()), name);
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    target: "launcher",
                    "Could not resolve CurseForge mod titles: {}",
                    e
                ),
            }
        }
    }
    for chunk in modrinth_ids.chunks(BATCH) {
        match modrinth::get_projects_by_ids(chunk).await {
            Ok(projects) => {
                for p in projects {
                    if let Some(name) = clean_project_title(&p.title) {
                        fetched.insert(project_name_key("modrinth", &p.id), name);
                    }
                }
            }
            Err(e) => tracing::warn!(
                target: "launcher",
                "Could not resolve Modrinth mod titles: {}",
                e
            ),
        }
    }

    let mut out = std::collections::HashMap::new();
    if !fetched.is_empty() {
        let mut cache = project_name_cache(&config)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for (key, name) in fetched {
            cache.insert(key, name);
        }
        save_project_name_cache_to_disk(project_name_cache_file(&config), &cache);
        for r in &refs {
            if let Some(name) = cache.get(&project_name_key(&r.provider, &r.project_id)) {
                out.insert(r.filename.clone(), name.clone());
            }
        }
    } else {
        for r in &refs {
            if let Some(name) = cached.get(&project_name_key(&r.provider, &r.project_id)) {
                out.insert(r.filename.clone(), name.clone());
            }
        }
    }
    Ok(out)
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
const META_CACHE_VERSION: u32 = 7;

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
const PROJECT_NAME_CACHE_VERSION: u32 = 1;
type ProjectNameCache = std::collections::HashMap<String, String>;
static PROJECT_NAME_CACHE: OnceLock<Mutex<ProjectNameCache>> = OnceLock::new();
static PROJECT_NAME_CACHE_FILE: OnceLock<PathBuf> = OnceLock::new();

fn project_name_cache_file(config: &AppConfig) -> &'static PathBuf {
    PROJECT_NAME_CACHE_FILE.get_or_init(|| config.data_dir.join("project_name_cache.json"))
}

fn project_name_key(provider: &str, project_id: &str) -> String {
    format!(
        "{}:{}",
        provider.trim().to_ascii_lowercase(),
        project_id.trim()
    )
}

fn load_project_name_cache_from_disk(path: &std::path::Path) -> ProjectNameCache {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct ProjectNameCacheFile {
        #[serde(default)]
        version: u32,
        #[serde(default)]
        entries: ProjectNameCache,
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return ProjectNameCache::new();
    };
    let Ok(file) = serde_json::from_str::<ProjectNameCacheFile>(&raw) else {
        return ProjectNameCache::new();
    };
    if file.version != PROJECT_NAME_CACHE_VERSION {
        return ProjectNameCache::new();
    }
    file.entries
}

fn project_name_cache(config: &AppConfig) -> &'static Mutex<ProjectNameCache> {
    PROJECT_NAME_CACHE.get_or_init(|| {
        Mutex::new(load_project_name_cache_from_disk(project_name_cache_file(
            config,
        )))
    })
}

fn save_project_name_cache_to_disk(path: &std::path::Path, cache: &ProjectNameCache) {
    let json = serde_json::to_string(&serde_json::json!({
        "version": PROJECT_NAME_CACHE_VERSION,
        "entries": cache,
    }));
    let Ok(json) = json else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Provider titles are the most readable names available (e.g.
/// "Sinytra Connector" instead of the jar's internal "connectormod").
fn clean_project_title(raw: &str) -> Option<String> {
    let name = usable_mod_name(raw, None)?;
    let cleaned = instances::strip_minecraft_color_codes(name).replace(" '", "'");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

fn project_name_override(meta: &ModMetadata, cache: &ProjectNameCache) -> Option<String> {
    let provider = meta.provider.to_ascii_lowercase();
    if provider != "curseforge" && provider != "modrinth" {
        return None;
    }
    let project_id = meta.project_id.as_deref()?;
    cache.get(&project_name_key(&provider, project_id)).cloned()
}

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
    let mc_version = instance.mc_version.clone();
    let loader = match &instance.loader {
        instances::LoaderType::Fabric => Some("fabric"),
        instances::LoaderType::Forge => Some("forge"),
        instances::LoaderType::NeoForge => Some("neoforge"),
        // A vanilla instance has no mod loader descriptor to read a version
        // from; treat every mod's version as Unknown rather than borrowing a
        // Fabric/Forge version.
        instances::LoaderType::Vanilla => Some("vanilla"),
    };
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

    let project_names = project_name_cache(config)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();

    // Serve cache hits first; only cache misses need the expensive jar parse.
    enum Job {
        Cached(Box<ModMetadata>),
        Fresh,
    }
    let mut jobs: Vec<Job> = Vec::with_capacity(files.len());
    let mut missed_idx: Vec<usize> = Vec::new();
    {
        let cache = meta_cache(config).lock().unwrap_or_else(|p| p.into_inner());
        for (i, f) in files.iter().enumerate() {
            match cache.get(&f.path) {
                Some((m, s, meta)) if *m == f.mtime && *s == f.size => {
                    jobs.push(Job::Cached(Box::new(meta.clone())));
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
        let project_names_ref = &project_names;
        let mc_version_ref = &mc_version;
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
                                (
                                    start + k,
                                    build_mod_metadata(
                                        mods_dir_ref,
                                        pw_index,
                                        project_names_ref,
                                        Some(mc_version_ref.as_str()),
                                        loader,
                                        f,
                                    ),
                                )
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
            .map(|(k, f)| {
                (
                    k,
                    build_mod_metadata(
                        &mods_dir,
                        &pw_index,
                        &project_names,
                        Some(mc_version.as_str()),
                        loader,
                        f,
                    ),
                )
            })
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
                jobs[missed_idx[*k]] = Job::Cached(Box::new(meta.clone()));
            }
        }
        if !missed_idx.is_empty() {
            save_meta_cache_to_disk(meta_cache_file(config), &cache);
        }
    }
    let mut mods: Vec<ModMetadata> = Vec::with_capacity(jobs.len());
    for job in jobs {
        match job {
            Job::Cached(mut meta) => {
                if let Some(name) = project_name_override(&meta, &project_names) {
                    meta.name = name;
                }
                mods.push(*meta);
            }
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
    project_names: &ProjectNameCache,
    mc_version: Option<&str>,
    loader: Option<&str>,
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
    let (jar_version, version_known) = meta.version_for(loader, mc_version);
    let pw = instances::packwiz_meta(pw_index, filename);
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
    let canonical_name = project_id.as_ref().and_then(|pid| {
        project_names
            .get(&project_name_key(&provider, pid))
            .cloned()
    });
    let name = canonical_name
        .filter(|n| usable_mod_name(n, None).is_some())
        .or_else(|| Some(jar_name).filter(|n| !n.is_empty()))
        .or_else(|| {
            sidecar
                .project_name
                .as_deref()
                .filter(|n| !looks_like_version(n))
                .and_then(|n| usable_mod_name(n, slug.as_deref()))
                .map(str::to_string)
        })
        .or_else(|| pw.and_then(|p| usable_mod_name(&p.name, slug.as_deref()).map(str::to_string)))
        .unwrap_or_else(|| filename.trim_end_matches(".jar").to_string());
    let name = instances::strip_minecraft_color_codes(&name);
    // Prism's Mod::version() returns the version declared by the JAR itself
    // (fabric.mod.json / mods.toml), not the provider's version name, the
    // packwiz metadata or a number guessed from the file name. For a
    // multi-loader JAR the version comes from the descriptor of the instance
    // loader; another loader's version is never substituted. "${version}",
    // "${file.jarVersion}", "Unknown" and "NONE" are build placeholders.
    let version = normalize_mod_version(&jar_version, mc_version, loader);
    let version_known = version_known && !version.is_empty();
    ModMetadata {
        filename: filename.clone(),
        name,
        version,
        provider,
        loaders: meta.loaders.clone(),
        version_known,
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

fn usable_mod_name<'a>(name: &'a str, _mod_id: Option<&str>) -> Option<&'a str> {
    let trimmed = name.trim();
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("unknown")
        || trimmed.contains("${")
        || trimmed.eq_ignore_ascii_case("%s")
        || lower.ends_with(".jar")
        || lower.ends_with(".zip")
    {
        return None;
    }
    // `displayName == modId` is common and valid (spark, cataclysm). Prism
    // Launcher shows that name too, so the id equality check must not discard
    // the only human-readable value the jar provides.
    Some(trimmed)
}

/// Strip build-placeholder versions ("${version}", "${file.jarVersion}",
/// "Unknown", "NONE", ...) so the UI never shows them as real versions.
fn sanitize_version(raw: &str) -> String {
    let trimmed = raw.trim();
    let placeholder = trimmed.is_empty()
        || trimmed.contains("${")
        || trimmed.eq_ignore_ascii_case("unknown")
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("n/a")
        || trimmed.eq_ignore_ascii_case("null")
        || trimmed == "-";
    if placeholder {
        String::new()
    } else {
        trimmed.to_string()
    }
}

/// Remove only *known technical* tokens from a displayed mod version:
/// an exact leading/trailing instance Minecraft version and the instance
/// loader suffix. This is deliberately conservative: arbitrary suffixes,
/// digits and dots are never stripped, because they may be the real version.
fn normalize_mod_version(raw: &str, mc_version: Option<&str>, loader: Option<&str>) -> String {
    let mut version = sanitize_version(raw);
    if version.is_empty() {
        return version;
    }

    if let Some(mc) = mc_version.map(str::trim).filter(|v| !v.is_empty()) {
        // `1.20.1-1.5.2` -> `1.5.2` (only when the remainder is version-like).
        if let Some(rest) = version.strip_prefix(mc) {
            if let Some(rest) = rest
                .strip_prefix(['-', '_', '+'])
                .filter(|r| matches!(r.chars().next(), Some(c) if c.is_ascii_digit() || c == 'v' || c == 'V'))
            {
                version = rest.to_string();
            }
        }
        // `1.4.8+1.21.1` / `1.4.8-1.21.1` -> `1.4.8`.
        for separator in ['+', '-', '_'] {
            let suffix = format!("{separator}{mc}");
            if version.len() > suffix.len() && version.ends_with(&suffix) {
                let rest =
                    version[..version.len() - suffix.len()].trim_end_matches(['+', '-', '_', ' ']);
                if !rest.is_empty() {
                    version = rest.to_string();
                }
                break;
            }
        }
    }

    if let Some(loader) = loader.map(str::trim).filter(|v| !v.is_empty()) {
        let lower = version.to_ascii_lowercase();
        for separator in ['-', '+', '_', ' '] {
            let suffix = format!("{separator}{}", loader.to_ascii_lowercase());
            if version.len() > suffix.len() && lower.ends_with(&suffix) {
                let rest =
                    version[..version.len() - suffix.len()].trim_end_matches(['+', '-', '_', ' ']);
                if !rest.is_empty() {
                    version = rest.to_string();
                }
                break;
            }
        }
    }

    sanitize_version(&version)
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
    pub(crate) loaders: Vec<String>,
    /// Raw version declared by each descriptor, keyed by canonical loader.
    pub(crate) version_by_loader: std::collections::HashMap<String, String>,
    pub(crate) icon: Option<String>,
    pub(crate) slug: Option<String>,
    /// True when at least one supported mod descriptor was found. A JAR with
    /// no descriptor is a local/unknown drop, not an arbitrary-loader mod.
    pub(crate) has_descriptor: bool,
}

impl ModMetaResult {
    /// Version for the requested loader. A descriptor of another loader is
    /// never substituted: an absent descriptor means Unknown. `None` means the
    /// caller has no instance loader (or a non-mod instance); only then is the
    /// first detected descriptor used.
    pub(crate) fn version_for(
        &self,
        loader: Option<&str>,
        mc_version: Option<&str>,
    ) -> (String, bool) {
        let wanted = loader.and_then(canonical_loader);
        let raw = match wanted {
            Some("NeoForge") => self
                .version_by_loader
                .get("NeoForge")
                .or_else(|| {
                    // NeoForge used `META-INF/mods.toml` through MC 1.20.4;
                    // the rename to `neoforge.mods.toml` happened for 1.20.5.
                    // Treat the Forge descriptor as the applicable one only on
                    // those older NeoForge versions.
                    neoforge_uses_forge_descriptor(mc_version)
                        .then(|| self.version_by_loader.get("Forge"))
                        .flatten()
                })
                .cloned()
                .unwrap_or_default(),
            Some(wanted) => self
                .version_by_loader
                .get(wanted)
                .cloned()
                .unwrap_or_default(),
            None => self
                .loaders
                .iter()
                .find_map(|loader| self.version_by_loader.get(loader))
                .cloned()
                .unwrap_or_default(),
        };
        let version = sanitize_version(&raw);
        let known = !version.is_empty();
        (version, known)
    }

    pub(crate) fn is_compatible_with(
        &self,
        loader: Option<&str>,
        mc_version: Option<&str>,
    ) -> bool {
        if loader.is_some_and(|value| value.eq_ignore_ascii_case("vanilla")) {
            return false;
        }
        let Some(wanted) = loader.and_then(canonical_loader) else {
            return true;
        };
        // No descriptor at all: keep local/unknown jars visible, as before.
        // A known descriptor set, however, must contain the instance loader.
        if wanted == "NeoForge" && neoforge_uses_forge_descriptor(mc_version) {
            return self
                .loaders
                .iter()
                .any(|value| value == "NeoForge" || value == "Forge")
                || (!self.has_descriptor && self.loaders.is_empty());
        }
        self.loaders.iter().any(|value| value == wanted)
            || (!self.has_descriptor && self.loaders.is_empty())
    }
}

/// True for MC versions where NeoForge still used Forge's `mods.toml`.
fn neoforge_uses_forge_descriptor(mc_version: Option<&str>) -> bool {
    let Some(raw) = mc_version else {
        return false;
    };
    let mut parts = raw
        .split(['.', '-', '+'])
        .filter_map(|part| part.parse::<u32>().ok());
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    (major, minor, patch) < (1, 20, 5)
}

pub(crate) fn canonical_loader(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "fabric" => Some("Fabric"),
        "forge" => Some("Forge"),
        "neoforge" => Some("NeoForge"),
        _ => None,
    }
}

/// One descriptor found inside a JAR. Loader names are canonical here and are
/// never reused for the download-source `provider` field.
struct ModDescriptor {
    loader: &'static str,
    name: String,
    version: String,
    icon: Option<String>,
    slug: Option<String>,
    /// True when `name` came from a human-readable display name rather than a
    /// fallback to the mod id. Used to pick the combined display name.
    name_from_display: bool,
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
        loaders: Vec::new(),
        version_by_loader: std::collections::HashMap::new(),
        icon: None,
        slug: None,
        // A JAR without any mod descriptor is a local/unknown drop. It stays
        // visible under every mod loader (the historical behaviour), but no
        // loader is claimed for it.
        has_descriptor: false,
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
    if let Some(meta) = parse_mod_meta_archive(&mut archive) {
        if !meta.loaders.is_empty() {
            return meta;
        }
        // A recognized but unsupported descriptor (for example Quilt) in the
        // outer archive must not hide a supported descriptor in a nested jar.
        if let Some(nested) = read_nested_jar_meta(&mut archive) {
            return nested;
        }
        return meta;
    }
    if let Some(meta) = read_nested_jar_meta(&mut archive) {
        return meta;
    }

    // Fallback: extract name from filename
    fallback_meta_from_filename(path)
}

/// Read metadata from a JarJar / Fabric nested jar. Sinytra Connector, for
/// example, has no metadata in the outer archive: the real mod descriptor
/// lives in `META-INF/jarjar/*.jar`.
fn read_nested_jar_meta<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Option<ModMetaResult> {
    const MAX_NESTED_JAR_BYTES: u64 = 64 * 1024 * 1024;
    let candidates: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|name| {
            (name.starts_with("META-INF/jarjar/") || name.starts_with("META-INF/jars/"))
                && name.ends_with(".jar")
        })
        .collect();
    for name in candidates {
        let Ok(mut file) = archive.by_name(&name) else {
            continue;
        };
        let size = file.size();
        if size == 0 || size > MAX_NESTED_JAR_BYTES {
            continue;
        }
        let mut bytes = Vec::with_capacity(size as usize);
        if file.read_to_end(&mut bytes).is_err() {
            continue;
        }
        let Ok(mut nested) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
            continue;
        };
        if let Some(meta) = parse_mod_meta_archive(&mut nested) {
            return Some(meta);
        }
    }
    None
}

const MAX_MOD_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MOD_ICON_BYTES: u64 = 8 * 1024 * 1024;

fn read_zip_text_limited<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Option<String> {
    let file = archive.by_name(name).ok()?;
    if file.size() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn read_zip_bytes_limited<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Option<Vec<u8>> {
    let file = archive.by_name(name).ok()?;
    if file.size() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    Some(bytes)
}

/// `Implementation-Version` from `META-INF/MANIFEST.MF`, normalized the way
/// Prism Launcher does: a missing header (or Gradle's unexpanded
/// `archiveVersion` placeholder) becomes "NONE" instead of a build string.
fn read_manifest_version<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> String {
    let Some(contents) =
        read_zip_text_limited(archive, "META-INF/MANIFEST.MF", MAX_MOD_METADATA_BYTES)
    else {
        return "NONE".to_string();
    };
    for line in contents.lines() {
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("Implementation-Version") {
            continue;
        }
        let raw = raw.trim();
        if raw.is_empty() || raw.contains("task ':jar' property 'archiveVersion'") {
            return "NONE".to_string();
        }
        return raw.to_string();
    }
    "NONE".to_string()
}

fn parse_mod_meta_archive<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Option<ModMetaResult> {
    // Every supported descriptor is parsed independently. A JAR may ship
    // several of them (Fabric + Forge + NeoForge), and each one keeps its own
    // declared version. The old implementation returned as soon as the Fabric
    // descriptor carried a real-looking version, which mislabelled
    // NeoForge/Forge multi-loader jars as Fabric-only.
    let mut descriptors: Vec<ModDescriptor> = Vec::new();

    if let Some(descriptor) = parse_fabric_descriptor(archive) {
        push_descriptor(&mut descriptors, descriptor);
    }

    // Forge/NeoForge packs commonly put `${file.jarVersion}` in mods.toml;
    // Prism resolves it from the JAR manifest instead of showing the
    // placeholder. Read it once for both the TOML parsers.
    let manifest_version = read_manifest_version(archive);
    for (path, loader) in [
        ("META-INF/neoforge.mods.toml", "NeoForge"),
        ("META-INF/mods.toml", "Forge"),
    ] {
        if let Some(descriptor) = parse_toml_descriptor(archive, path, loader, &manifest_version) {
            push_descriptor(&mut descriptors, descriptor);
        }
    }

    // Legacy Forge metadata. Do not add a second Forge row when a modern
    // descriptor already provided the same loader (and therefore a better
    // version/name pair).
    if !descriptors
        .iter()
        .any(|descriptor| descriptor.loader == "Forge")
    {
        if let Some(descriptor) = parse_legacy_forge_descriptor(archive) {
            push_descriptor(&mut descriptors, descriptor);
        }
    }

    combine_descriptors(descriptors, has_unknown_descriptor(archive))
}

/// Append a descriptor unless the same loader was already found. The first
/// descriptor for a loader wins; later duplicates (for example a stale
/// `mcmod.info` next to `mods.toml`) cannot overwrite a newer, richer one.
fn push_descriptor(descriptors: &mut Vec<ModDescriptor>, descriptor: ModDescriptor) {
    if descriptors
        .iter()
        .any(|existing| existing.loader == descriptor.loader)
    {
        return;
    }
    descriptors.push(descriptor);
}

fn parse_fabric_descriptor<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Option<ModDescriptor> {
    let contents = read_zip_text_limited(archive, "fabric.mod.json", MAX_MOD_METADATA_BYTES)?;
    let json = serde_json::from_str::<serde_json::Value>(&contents).ok()?;

    let id = json["id"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or("Unknown");
    let display =
        usable_mod_name(json["name"].as_str().unwrap_or(""), Some(id)).map(str::to_string);
    let name = display.clone().unwrap_or_else(|| id.to_string());
    let version = json["version"].as_str().unwrap_or("").to_string();
    let slug = json["id"].as_str().map(str::to_string);
    let icon = json["icon"].as_str().map(|value| {
        value
            .trim_start_matches('/')
            .trim_start_matches("assets/")
            .to_string()
    });

    Some(ModDescriptor {
        loader: "Fabric",
        name: instances::strip_minecraft_color_codes(&name),
        version,
        icon,
        slug,
        name_from_display: display.is_some(),
    })
}

fn parse_toml_descriptor<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    path: &str,
    loader: &'static str,
    manifest_version: &str,
) -> Option<ModDescriptor> {
    let contents = read_zip_text_limited(archive, path, MAX_MOD_METADATA_BYTES)?;

    let mut mod_id = None;
    let mut display = None;
    let mut version = None;
    let mut logo = None;

    if let Ok(toml_val) = contents.parse::<toml::Table>() {
        if let Some(mods_arr) = toml_val.get("mods").and_then(|value| value.as_array()) {
            let picked = mods_arr
                .iter()
                .filter_map(|entry| entry.as_table())
                .find(|entry| {
                    let id = entry.get("modId").and_then(|value| value.as_str());
                    usable_mod_name(
                        entry
                            .get("displayName")
                            .and_then(|value| value.as_str())
                            .unwrap_or(""),
                        id,
                    )
                    .is_some()
                })
                .or_else(|| mods_arr.iter().find_map(|entry| entry.as_table()));
            if let Some(picked) = picked {
                let id = picked
                    .get("modId")
                    .and_then(|value| value.as_str())
                    .unwrap_or("Unknown")
                    .to_string();
                display = usable_mod_name(
                    picked
                        .get("displayName")
                        .and_then(|value| value.as_str())
                        .unwrap_or(""),
                    Some(&id),
                )
                .map(str::to_string);
                version = picked
                    .get("version")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                logo = picked
                    .get("logoFile")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                mod_id = Some(id);
            }
        }
    }

    // Fallback to the line-based parser for malformed TOML. An empty or
    // unrelated TOML file is not a mod descriptor at all.
    if mod_id.is_none() {
        mod_id = extract_toml_field(&contents, "modId");
        display = extract_toml_field(&contents, "displayName")
            .filter(|name| usable_mod_name(name, mod_id.as_deref()).is_some());
        version = extract_toml_field(&contents, "version");
        logo = extract_toml_field(&contents, "logoFile");
    }
    if mod_id.is_none() && display.is_none() && version.is_none() {
        return None;
    }

    let mod_id = mod_id.unwrap_or_else(|| "Unknown".to_string());
    let name = display.clone().unwrap_or_else(|| mod_id.clone());
    let mut version = version.unwrap_or_default();
    if version == "${file.jarVersion}" {
        version = manifest_version.to_string();
    }

    Some(ModDescriptor {
        loader,
        name: instances::strip_minecraft_color_codes(&name),
        version,
        icon: logo,
        slug: Some(mod_id),
        name_from_display: display.is_some(),
    })
}

fn parse_legacy_forge_descriptor<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Option<ModDescriptor> {
    for info_name in &["mcmod.info", "META-INF/mcmod.info"] {
        let Some(contents) = read_zip_text_limited(archive, info_name, MAX_MOD_METADATA_BYTES)
        else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) else {
            continue;
        };
        let entries: Vec<&serde_json::Value> = if let Some(arr) = json.as_array() {
            arr.iter().collect()
        } else if let Some(arr) = json.get("modList").and_then(|value| value.as_array()) {
            arr.iter().collect()
        } else if json.get("modid").is_some() {
            vec![&json]
        } else {
            Vec::new()
        };
        let picked = entries
            .iter()
            .copied()
            .find(|entry| {
                usable_mod_name(
                    entry["name"].as_str().unwrap_or(""),
                    entry["modid"].as_str(),
                )
                .is_some()
            })
            .or_else(|| entries.first().copied());
        let Some(picked) = picked else {
            continue;
        };
        let mod_id = picked["modid"]
            .as_str()
            .filter(|value| !value.is_empty())
            .unwrap_or("Unknown")
            .to_string();
        let display = usable_mod_name(picked["name"].as_str().unwrap_or(""), Some(&mod_id))
            .map(str::to_string);
        let name = display.clone().unwrap_or_else(|| mod_id.clone());
        let version = picked["version"].as_str().unwrap_or("").to_string();
        let logo = picked["logoFile"].as_str().map(str::to_string);
        return Some(ModDescriptor {
            loader: "Forge",
            name: instances::strip_minecraft_color_codes(&name),
            version,
            icon: logo,
            slug: Some(mod_id),
            name_from_display: display.is_some(),
        });
    }
    None
}

/// Descriptors for loaders this launcher does not support. Their presence is
/// recorded so such a JAR is not advertised as compatible with Fabric/Forge/
/// NeoForge just because no supported descriptor was found.
fn has_unknown_descriptor<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> bool {
    const UNKNOWN_DESCRIPTORS: &[&str] = &[
        "quilt.mod.json",
        "META-INF/quilt.mod.json",
        "litemod.json",
        "META-INF/litemod.json",
    ];
    UNKNOWN_DESCRIPTORS
        .iter()
        .any(|name| archive.by_name(name).is_ok())
}

fn combine_descriptors(
    descriptors: Vec<ModDescriptor>,
    unknown_descriptor: bool,
) -> Option<ModMetaResult> {
    if descriptors.is_empty() && !unknown_descriptor {
        return None;
    }

    // Prefer a descriptor that actually declared a human-readable display
    // name. Falls back to the first descriptor (Fabric, then NeoForge, then
    // Forge) so the list always has a stable title.
    let name = descriptors
        .iter()
        .find(|descriptor| descriptor.name_from_display)
        .map(|descriptor| descriptor.name.clone())
        .or_else(|| {
            descriptors
                .first()
                .map(|descriptor| descriptor.name.clone())
        })
        .unwrap_or_else(|| "Unknown".to_string());
    let icon = descriptors
        .iter()
        .find_map(|descriptor| descriptor.icon.clone());
    let slug = descriptors
        .iter()
        .find_map(|descriptor| descriptor.slug.clone());

    let mut loaders: Vec<String> = Vec::new();
    let mut version_by_loader = std::collections::HashMap::new();
    for descriptor in &descriptors {
        if loaders.iter().any(|loader| loader == descriptor.loader) {
            continue;
        }
        loaders.push(descriptor.loader.to_string());
        version_by_loader.insert(descriptor.loader.to_string(), descriptor.version.clone());
    }

    Some(ModMetaResult {
        name,
        loaders,
        version_by_loader,
        icon,
        slug,
        has_descriptor: true,
    })
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
    let mut in_mods = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_mods = trimmed == "[[mods]]";
            continue;
        }
        if !in_mods {
            continue;
        }
        let Some((key, raw)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() != field {
            continue;
        }
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
    if let Some(contents) =
        read_zip_text_limited(&mut archive, "fabric.mod.json", MAX_MOD_METADATA_BYTES)
    {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
            if let Some(icon) = json["icon"].as_str() {
                let clean = icon.trim_start_matches("/").to_string();
                found_icon_name = Some(clean);
            }
        }
    }

    // Check mods.toml / neoforge.mods.toml (Forge/NeoForge) for logoFile
    if found_icon_name.is_none() {
        for meta_name in &["META-INF/mods.toml", "META-INF/neoforge.mods.toml"] {
            if let Some(contents) =
                read_zip_text_limited(&mut archive, meta_name, MAX_MOD_METADATA_BYTES)
            {
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

    // If we found an icon path in metadata, try to read it
    if let Some(icon_name) = found_icon_name {
        if let Some(buf) = read_zip_bytes_limited(&mut archive, &icon_name, MAX_MOD_ICON_BYTES) {
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
        if let Some(buf) = read_zip_bytes_limited(&mut archive, candidate, MAX_MOD_ICON_BYTES) {
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
                let size = entry.size();
                if size > MAX_MOD_ICON_BYTES {
                    continue;
                }
                let mut buf = Vec::new();
                let file = entry;
                file.take(MAX_MOD_ICON_BYTES + 1).read_to_end(&mut buf)?;
                if buf.len() as u64 > MAX_MOD_ICON_BYTES {
                    continue;
                }
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
            loaders: vec!["Fabric".into()],
            version_known: true,
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
    fn usable_mod_name_rejects_placeholders_and_filenames() {
        assert!(usable_mod_name("", Some("x")).is_none());
        assert!(usable_mod_name("Unknown", Some("x")).is_none());
        assert!(usable_mod_name("${mod_name}", Some("x")).is_none());
        assert!(usable_mod_name("mod.jar", None).is_none());
        assert_eq!(usable_mod_name("x", Some("x")), Some("x"));
        assert_eq!(usable_mod_name("X", Some("x")), Some("X"));
        assert_eq!(
            usable_mod_name("Real Mod Name", Some("realmod")),
            Some("Real Mod Name")
        );
    }

    #[test]
    fn display_name_equal_to_mod_id_is_kept() {
        let dir = test_dir("same-name");
        let jar = dir.join("spark.jar");
        write_jar(
            &jar,
            &[(
                "META-INF/mods.toml",
                r#"
modLoader="javafml"
loaderVersion="[47,)"
license="MIT"

[[mods]]
modId="spark"
displayName="spark"
version="1.10.53"
"#,
            )],
        );
        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(meta.name, "spark");
        assert_eq!(meta.slug.as_deref(), Some("spark"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_jar_with_nested(
        path: &std::path::Path,
        outer_files: &[(&str, &str)],
        nested_name: &str,
        nested_files: &[(&str, &str)],
    ) {
        use std::io::Write as _;
        let mut nested_zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in nested_files {
            nested_zip
                .start_file(*name, options)
                .expect("start nested entry");
            nested_zip
                .write_all(contents.as_bytes())
                .expect("write nested entry");
        }
        let nested_bytes = nested_zip.finish().expect("finish nested jar").into_inner();

        let file = std::fs::File::create(path).expect("create outer jar");
        let mut zip = zip::ZipWriter::new(file);
        for (name, contents) in outer_files {
            zip.start_file(*name, options).expect("start outer entry");
            zip.write_all(contents.as_bytes())
                .expect("write outer entry");
        }
        zip.start_file(nested_name, options)
            .expect("start nested jar entry");
        zip.write_all(&nested_bytes).expect("write nested jar");
        zip.finish().expect("finish outer jar");
    }

    #[test]
    fn nested_jarjar_metadata_is_used_when_outer_has_none() {
        let dir = test_dir("nested-jarjar");
        let jar = dir.join("Connector.jar");
        write_jar_with_nested(
            &jar,
            &[("pack.mcmeta", r#"{"pack":{"pack_format":15}}"#)],
            "META-INF/jarjar/Connector-mod.jar",
            &[(
                "META-INF/mods.toml",
                r#"
modLoader="javafml"
loaderVersion="[47,)"
license="MIT"

[[mods]]
modId="connectormod"
displayName="Connector"
version="1.0.0"
"#,
            )],
        );
        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(meta.name, "Connector");
        assert_eq!(meta.slug.as_deref(), Some("connectormod"));
        assert_eq!(meta.loaders, vec!["Forge".to_string()]);
        assert_eq!(
            meta.version_for(Some("Forge"), None),
            ("1.0.0".to_string(), true)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mod_version_comes_from_jar_not_sidecar_packwiz_or_filename() {
        let dir = test_dir("mod-version");
        let filename = "sound-physics-remastered-1.21.1-1.4.8.jar";
        let jar = dir.join(filename);
        write_jar(
            &jar,
            &[(
                "fabric.mod.json",
                r#"{"id":"sound_physics_remastered","name":"Sound Physics Remastered","version":"1.4.8+1.21.1"}"#,
            )],
        );
        std::fs::create_dir_all(dir.join(".index")).unwrap();
        std::fs::write(
            dir.join(".index").join("sound.voidlauncher.json"),
            r#"{"provider":"modrinth","project_id":"sound","project_name":"Sound Physics Remastered","version_number":"[FABRIC] [1.21.1] Sound Physics Remastered 1.4.8"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join(".index").join("sound.pw.toml"),
            format!(
                "filename = '{filename}'\nname = 'Sound Physics Remastered'\nx-prismlauncher-version-number = '9.9.9'\n[update.modrinth]\nmod-id = 'sound'\n"
            ),
        )
        .unwrap();

        let pw = instances::load_packwiz_index(&dir);
        let meta = build_mod_metadata(
            &dir,
            &pw,
            &ProjectNameCache::new(),
            Some("1.21.1"),
            Some("fabric"),
            &ModFileEntry {
                path: jar,
                filename: filename.into(),
                enabled: true,
                mtime: UNIX_EPOCH,
                size: 0,
            },
        );

        assert_eq!(meta.version, "1.4.8");
        assert_eq!(meta.provider, "Modrinth");
        assert_eq!(meta.loaders, vec!["Fabric".to_string()]);
        assert!(meta.version_known);
        assert_eq!(meta.name, "Sound Physics Remastered");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_file_uri_accepts_windows_paths_and_rejects_remote_urls() {
        assert_eq!(
            local_path_from_uri(r"file://C:\Temp\mod.jar").unwrap(),
            std::path::PathBuf::from(r"C:\Temp\mod.jar")
        );
        assert_eq!(
            local_path_from_uri("file:///C:/Temp/mod.jar").unwrap(),
            std::path::PathBuf::from("C:/Temp/mod.jar")
        );
        assert!(local_path_from_uri("https://example.com/mod.jar").is_err());
        assert!(local_path_from_uri("file://").is_err());
    }

    #[test]
    fn pack_description_is_not_used_as_a_title() {
        let dir = test_dir("pack-description-title");
        let zip_path = dir.join("Faithful 32x - 26.3.zip");
        write_jar(
            &zip_path,
            &[(
                "pack.mcmeta",
                r#"{"pack":{"pack_format":15,"description":"A long descriptive sentence"}}"#,
            )],
        );

        // The caller must fall back to the cleaned filename, not the prose
        // description, when pack.mcmeta has no explicit pack.name.
        assert!(read_pack_name_and_version(&zip_path).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_nested_folder_is_flattened() {
        let dir = test_dir("nested-resourcepack");
        let zip_path = dir.join("wrapped.zip");
        write_jar(
            &zip_path,
            &[
                ("Wrapped/pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
                ("Wrapped/pack.png", "png"),
            ],
        );

        assert_eq!(
            local_archive_nested_prefix(&zip_path, "resourcepack").unwrap(),
            Some("Wrapped".to_string())
        );
        repack_archive_under_prefix(&zip_path, "Wrapped").unwrap();

        let file = std::fs::File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert!(archive.by_name("pack.mcmeta").is_ok());
        assert!(archive.by_name("pack.png").is_ok());
        assert!(archive.by_name("Wrapped/pack.mcmeta").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_pack_directory_flattens_one_wrapper_folder() {
        let dir = test_dir("nested-resourcepack-dir");
        let source = dir.join("source");
        let wrapper = source.join("Wrapped");
        std::fs::create_dir_all(wrapper.join("assets")).unwrap();
        std::fs::write(
            wrapper.join("pack.mcmeta"),
            r#"{"pack":{"pack_format":15}}"#,
        )
        .unwrap();
        std::fs::write(wrapper.join("assets/icon.txt"), "x").unwrap();

        let root = local_directory_root(&source, "resourcepack").unwrap();
        assert_eq!(root, wrapper);
        let dest = dir.join("dest");
        copy_local_directory(&root, &dest).unwrap();
        assert!(dest.join("pack.mcmeta").is_file());
        assert!(dest.join("assets/icon.txt").is_file());
        assert!(!dest.join("Wrapped").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_pack_validation_rejects_archives_without_root_marker() {
        let dir = test_dir("bad-resourcepack");
        let zip_path = dir.join("bad.zip");
        write_jar(&zip_path, &[("assets/foo.txt", "x")]);

        assert!(local_archive_nested_prefix(&zip_path, "resourcepack").is_err());
        let valid = dir.join("valid.zip");
        write_jar(&valid, &[("pack.mcmeta", r#"{"pack":{"pack_format":15}}"#)]);
        assert_eq!(
            local_archive_nested_prefix(&valid, "resourcepack").unwrap(),
            None
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mod_version_strips_only_known_mc_and_loader_tokens() {
        assert_eq!(
            normalize_mod_version("1.4.8+1.21.1", Some("1.21.1"), Some("fabric")),
            "1.4.8"
        );
        assert_eq!(
            normalize_mod_version("1.20.1-1.5.2-neoforge", Some("1.20.1"), Some("neoforge")),
            "1.5.2"
        );
        assert_eq!(
            normalize_mod_version("1.0.7+forge", Some("1.20.1"), Some("forge")),
            "1.0.7"
        );
        // Different MC/loader: the text may be a genuine version component.
        assert_eq!(
            normalize_mod_version("1.4.8+1.21.1", Some("1.20.1"), Some("forge")),
            "1.4.8+1.21.1"
        );
        assert_eq!(
            normalize_mod_version("1.2.3-beta", Some("1.20.1"), Some("fabric")),
            "1.2.3-beta"
        );
        assert_eq!(
            normalize_mod_version("1.21.1", Some("1.21.1"), Some("fabric")),
            "1.21.1",
            "a version equal to the MC version is not a technical suffix"
        );
    }

    #[test]
    fn disabled_mod_keeps_packwiz_name_version_and_provider() {
        let dir = test_dir("disabled-mod-meta");
        let filename = "Resourcify (26.3-fabric)-1.8.7.jar.disabled";
        let jar = dir.join(filename);
        write_jar(
            &jar,
            &[(
                "fabric.mod.json",
                r#"{"id":"resourcify","name":"Resourcify","version":"1.8.7"}"#,
            )],
        );
        std::fs::create_dir_all(dir.join(".index")).unwrap();
        std::fs::write(
            dir.join(".index").join("resourcify.pw.toml"),
            "filename = 'Resourcify (26.3-fabric)-1.8.7.jar'\nname = 'Resourcify'\nx-prismlauncher-version-number = '1.8.7'\n[update.modrinth]\nmod-id = 'RLzHAoZe'\n",
        )
        .unwrap();

        let pw = instances::load_packwiz_index(&dir);
        let meta = build_mod_metadata(
            &dir,
            &pw,
            &ProjectNameCache::new(),
            Some("26.3"),
            Some("fabric"),
            &ModFileEntry {
                path: jar,
                filename: filename.into(),
                enabled: false,
                mtime: UNIX_EPOCH,
                size: 0,
            },
        );

        assert!(!meta.enabled);
        assert_eq!(meta.name, "Resourcify");
        assert_eq!(meta.version, "1.8.7");
        assert_eq!(meta.provider, "Modrinth");
        assert_eq!(meta.loaders, vec!["Fabric".to_string()]);
        assert!(meta.version_known);
        assert_eq!(meta.project_id.as_deref(), Some("RLzHAoZe"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_titles_are_cleaned_and_filtered() {
        assert_eq!(
            clean_project_title("L_Ender 's Cataclysm").as_deref(),
            Some("L_Ender's Cataclysm")
        );
        assert_eq!(
            clean_project_title("§aSinytra Connector").as_deref(),
            Some("Sinytra Connector")
        );
        assert!(clean_project_title("cataclysm-3.31.jar").is_none());
        assert!(clean_project_title("   ").is_none());
    }

    #[test]
    fn project_names_override_cached_metadata_by_normalized_key() {
        let mut meta = sample_meta();
        meta.provider = "CurseForge".into();
        meta.project_id = Some("361579".into());
        let mut cache = ProjectNameCache::new();
        cache.insert(project_name_key("curseforge", "361579"), "spark".into());
        assert_eq!(
            project_name_override(&meta, &cache).as_deref(),
            Some("spark")
        );
        meta.project_id = Some("890127".into());
        assert!(project_name_override(&meta, &cache).is_none());
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
        assert_eq!(meta.loaders, vec!["Forge".to_string()]);
        assert_eq!(
            meta.version_for(Some("Forge"), None),
            ("2.3".to_string(), true)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forge_placeholder_version_resolves_from_manifest() {
        let dir = test_dir("forge-manifest-version");
        let jar = dir.join("aether.jar");
        write_jar(
            &jar,
            &[
                (
                    "META-INF/MANIFEST.MF",
                    "Manifest-Version: 1.0\r\nImplementation-Version: 1.20.1-1.5.2-neoforge\r\n\r\n",
                ),
                (
                    "META-INF/neoforge.mods.toml",
                    r#"
modLoader="javafml"
loaderVersion="[47,)"
license="MIT"

[[mods]]
modId="aether"
displayName="The Aether"
version="${file.jarVersion}"
"#,
                ),
            ],
        );
        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(
            meta.version_for(Some("NeoForge"), None),
            ("1.20.1-1.5.2-neoforge".to_string(), true)
        );
        assert_eq!(meta.loaders, vec!["NeoForge".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forge_placeholder_without_manifest_version_shows_none() {
        let dir = test_dir("forge-manifest-none");
        let jar = dir.join("oldforge.jar");
        write_jar(
            &jar,
            &[
                ("META-INF/MANIFEST.MF", "Manifest-Version: 1.0\n\n"),
                (
                    "META-INF/mods.toml",
                    r#"
modLoader="javafml"
loaderVersion="[47,)"
license="MIT"

[[mods]]
modId="oldforge"
displayName="Old Forge Mod"
version="${file.jarVersion}"
"#,
                ),
            ],
        );
        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(
            meta.version_for(Some("Forge"), None),
            (String::new(), false)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn metadata_reads_are_capped_and_never_allocate_an_unbounded_entry() {
        let dir = test_dir("metadata-cap");
        let jar = dir.join("cap.jar");
        write_jar(
            &jar,
            &[("META-INF/MANIFEST.MF", "Implementation-Version: 1.2.3\n")],
        );
        let file = std::fs::File::open(&jar).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert!(
            read_zip_text_limited(&mut archive, "META-INF/MANIFEST.MF", 4).is_none(),
            "a descriptor over the cap must be ignored"
        );
        assert_eq!(
            read_zip_text_limited(&mut archive, "META-INF/MANIFEST.MF", 1024).as_deref(),
            Some("Implementation-Version: 1.2.3\n")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_directory_size_budget_is_checked_before_copying() {
        let dir = test_dir("local-dir-budget");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), vec![0u8; 32]).unwrap();
        std::fs::write(dir.join("b.bin"), vec![0u8; 32]).unwrap();
        let error = validate_local_directory_size(&dir, 40).unwrap_err();
        assert_eq!(error.code, "archiveTooLarge");
        assert!(validate_local_directory_size(&dir, 64).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toml_fallback_requires_an_exact_key_inside_the_mods_section() {
        let source = r#"
modLoader="javafml"
versionRange="[1.20,)"
[[dependencies.example]]
modId="dependency"
versionRange="[1.0,)"
[[mods]]
modId="examplemod"
displayName="Example"
version="2.0.0"
"#;
        assert_eq!(
            extract_toml_field(source, "version"),
            Some("2.0.0".to_string())
        );
        assert_eq!(
            extract_toml_field(source, "modId"),
            Some("examplemod".to_string())
        );
        assert_eq!(extract_toml_field(source, "versionRange"), None);
        assert_eq!(
            extract_toml_field("versionRange=\"[1.20,)\"", "version"),
            None
        );
    }

    #[test]
    fn multi_loader_jar_keeps_a_version_for_each_descriptor() {
        let dir = test_dir("multi-loader-version");
        let jar = dir.join("autochefsdelight.jar");
        write_jar(
            &jar,
            &[
                (
                    "fabric.mod.json",
                    r#"{"id":"autochefsdelight","name":"Autochef's Delight","version":"${version}"}"#,
                ),
                (
                    "META-INF/mods.toml",
                    r#"
modLoader="javafml"
loaderVersion="[40,)"
license="ARR"

[[mods]]
modId="autochefsdelight"
version="1.0.7+forge"
displayName="Autochef's Delight"
"#,
                ),
            ],
        );
        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(
            meta.loaders,
            vec!["Fabric".to_string(), "Forge".to_string()]
        );
        assert_eq!(
            meta.version_for(Some("Forge"), None),
            ("1.0.7+forge".to_string(), true)
        );
        // The Fabric descriptor is present but its version is an unexpanded
        // build placeholder. The Forge version must not be shown as Fabric.
        assert_eq!(
            meta.version_for(Some("Fabric"), None),
            (String::new(), false)
        );
        assert_eq!(
            meta.version_for(Some("NeoForge"), None),
            (String::new(), false)
        );
        assert_eq!(meta.name, "Autochef's Delight");
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
        assert_eq!(meta.loaders, vec!["Forge".to_string()]);
        assert_eq!(
            meta.version_for(Some("Forge"), None),
            ("1.2.3".to_string(), true)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fabric_json(version: &str) -> String {
        format!(r#"{{"id":"examplemod","name":"Example Mod","version":"{version}"}}"#)
    }

    fn forge_toml(version: &str) -> String {
        format!(
            "modLoader=\"javafml\"\nloaderVersion=\"[47,)\"\nlicense=\"MIT\"\n\n[[mods]]\nmodId=\"examplemod\"\ndisplayName=\"Example Mod\"\nversion=\"{version}\"\n"
        )
    }

    #[test]
    fn forge_descriptor_is_usable_on_neoforge_only_through_mc_1_20_4() {
        let dir = test_dir("neoforge-legacy-descriptor");
        let jar = dir.join("legacy.jar");
        let source = forge_toml("7.8.9");
        write_jar(&jar, &[("META-INF/mods.toml", source.as_str())]);
        let meta = read_mod_meta_from_jar_pub(&jar);

        assert!(meta.is_compatible_with(Some("NeoForge"), Some("1.20.4")));
        assert_eq!(
            meta.version_for(Some("NeoForge"), Some("1.20.4")),
            ("7.8.9".to_string(), true)
        );
        assert!(!meta.is_compatible_with(Some("NeoForge"), Some("1.20.5")));
        assert!(!meta.is_compatible_with(Some("NeoForge"), Some("1.21.4")));
        assert_eq!(
            meta.version_for(Some("NeoForge"), Some("1.21.4")),
            (String::new(), false)
        );
        assert!(meta.is_compatible_with(Some("Forge"), Some("1.20.4")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_descriptor_jars_report_exactly_their_loader() {
        let dir = test_dir("descriptor-single");

        let fabric_jar = dir.join("fabric-only.jar");
        let fabric_source = fabric_json("1.2.3");
        write_jar(&fabric_jar, &[("fabric.mod.json", fabric_source.as_str())]);

        let forge_jar = dir.join("forge-only.jar");
        let forge_source = forge_toml("2.3.4");
        write_jar(&forge_jar, &[("META-INF/mods.toml", forge_source.as_str())]);

        let neo_jar = dir.join("neoforge-only.jar");
        let neo_source = forge_toml("3.4.5");
        write_jar(
            &neo_jar,
            &[("META-INF/neoforge.mods.toml", neo_source.as_str())],
        );

        let fabric = read_mod_meta_from_jar_pub(&fabric_jar);
        assert_eq!(fabric.loaders, vec!["Fabric".to_string()]);
        assert_eq!(
            fabric.version_for(Some("Fabric"), None),
            ("1.2.3".to_string(), true)
        );
        assert_eq!(
            fabric.version_for(Some("Forge"), None),
            (String::new(), false)
        );
        assert!(fabric.is_compatible_with(Some("Fabric"), None));
        assert!(!fabric.is_compatible_with(Some("Forge"), None));
        assert!(!fabric.is_compatible_with(Some("NeoForge"), None));

        let forge = read_mod_meta_from_jar_pub(&forge_jar);
        assert_eq!(forge.loaders, vec!["Forge".to_string()]);
        assert_eq!(
            forge.version_for(Some("Forge"), None),
            ("2.3.4".to_string(), true)
        );
        assert!(forge.is_compatible_with(Some("Forge"), None));
        assert!(!forge.is_compatible_with(Some("Fabric"), None));

        let neo = read_mod_meta_from_jar_pub(&neo_jar);
        assert_eq!(neo.loaders, vec!["NeoForge".to_string()]);
        assert_eq!(
            neo.version_for(Some("NeoForge"), None),
            ("3.4.5".to_string(), true)
        );
        assert!(neo.is_compatible_with(Some("NeoForge"), None));
        assert!(!neo.is_compatible_with(Some("Forge"), None));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fabric_and_neoforge_descriptors_are_both_detected() {
        let dir = test_dir("descriptor-fabric-neo");
        let jar = dir.join("fabric-neo.jar");
        let fabric_source = fabric_json("2.0.0");
        let neo_source = forge_toml("3.0.0");
        write_jar(
            &jar,
            &[
                ("fabric.mod.json", fabric_source.as_str()),
                ("META-INF/neoforge.mods.toml", neo_source.as_str()),
            ],
        );

        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(
            meta.loaders,
            vec!["Fabric".to_string(), "NeoForge".to_string()]
        );
        assert_eq!(
            meta.version_for(Some("Fabric"), None),
            ("2.0.0".to_string(), true)
        );
        assert_eq!(
            meta.version_for(Some("NeoForge"), None),
            ("3.0.0".to_string(), true)
        );
        assert_eq!(
            meta.version_for(Some("Forge"), None),
            (String::new(), false)
        );
        assert!(meta.is_compatible_with(Some("Fabric"), None));
        assert!(meta.is_compatible_with(Some("NeoForge"), None));
        assert!(!meta.is_compatible_with(Some("Forge"), None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forge_and_neoforge_descriptors_are_both_detected() {
        let dir = test_dir("descriptor-forge-neo");
        let jar = dir.join("forge-neo.jar");
        let forge_source = forge_toml("1.1.0");
        let neo_source = forge_toml("1.2.0");
        write_jar(
            &jar,
            &[
                ("META-INF/mods.toml", forge_source.as_str()),
                ("META-INF/neoforge.mods.toml", neo_source.as_str()),
            ],
        );

        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(
            meta.loaders,
            vec!["NeoForge".to_string(), "Forge".to_string()]
        );
        assert_eq!(
            meta.version_for(Some("Forge"), None),
            ("1.1.0".to_string(), true)
        );
        assert_eq!(
            meta.version_for(Some("NeoForge"), None),
            ("1.2.0".to_string(), true)
        );
        assert!(meta.is_compatible_with(Some("Forge"), None));
        assert!(meta.is_compatible_with(Some("NeoForge"), None));
        assert!(!meta.is_compatible_with(Some("Fabric"), None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_three_descriptors_are_detected_with_their_own_versions() {
        let dir = test_dir("descriptor-all");
        let jar = dir.join("all-loaders.jar");
        let fabric_source = fabric_json("2.0.0");
        let forge_source = forge_toml("4.0.0");
        let neo_source = forge_toml("3.0.0");
        write_jar(
            &jar,
            &[
                ("fabric.mod.json", fabric_source.as_str()),
                ("META-INF/mods.toml", forge_source.as_str()),
                ("META-INF/neoforge.mods.toml", neo_source.as_str()),
            ],
        );

        let meta = read_mod_meta_from_jar_pub(&jar);
        assert_eq!(
            meta.loaders,
            vec![
                "Fabric".to_string(),
                "NeoForge".to_string(),
                "Forge".to_string()
            ]
        );
        assert_eq!(
            meta.version_for(Some("Fabric"), None),
            ("2.0.0".to_string(), true)
        );
        assert_eq!(
            meta.version_for(Some("Forge"), None),
            ("4.0.0".to_string(), true)
        );
        assert_eq!(
            meta.version_for(Some("NeoForge"), None),
            ("3.0.0".to_string(), true)
        );
        // Minecraft's version is unrelated to the mod's version.
        assert_ne!(meta.version_for(Some("Fabric"), None).0, "1.21.1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_placeholder_descriptor_version_is_unknown() {
        let dir = test_dir("descriptor-unknown-version");

        let missing = dir.join("missing-version.jar");
        write_jar(
            &missing,
            &[(
                "fabric.mod.json",
                r#"{"id":"examplemod","name":"Example Mod"}"#,
            )],
        );
        let meta = read_mod_meta_from_jar_pub(&missing);
        assert_eq!(
            meta.version_for(Some("Fabric"), None),
            (String::new(), false)
        );

        let placeholder = dir.join("placeholder-version.jar");
        let source = fabric_json("${version}");
        write_jar(&placeholder, &[("fabric.mod.json", source.as_str())]);
        let meta = read_mod_meta_from_jar_pub(&placeholder);
        assert_eq!(
            meta.version_for(Some("Fabric"), None),
            (String::new(), false)
        );

        let none = dir.join("none-version.jar");
        write_jar(
            &none,
            &[(
                "META-INF/mods.toml",
                "modLoader=\"javafml\"\n[[mods]]\nmodId=\"examplemod\"\ndisplayName=\"Example Mod\"\nversion=\"NONE\"\n",
            )],
        );
        let meta = read_mod_meta_from_jar_pub(&none);
        assert_eq!(
            meta.version_for(Some("Forge"), None),
            (String::new(), false)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_descriptor_does_not_claim_a_supported_loader() {
        let dir = test_dir("descriptor-quilt");
        let jar = dir.join("quilt-only.jar");
        write_jar(
            &jar,
            &[(
                "quilt.mod.json",
                r#"{"schema_version":1,"quilt_loader":{"id":"examplemod","version":"1.0.0"}}"#,
            )],
        );

        let meta = read_mod_meta_from_jar_pub(&jar);
        assert!(meta.has_descriptor);
        assert!(meta.loaders.is_empty());
        assert!(!meta.is_compatible_with(Some("Fabric"), None));
        assert!(!meta.is_compatible_with(Some("Forge"), None));
        assert!(!meta.is_compatible_with(Some("NeoForge"), None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_mod_files_includes_multi_loader_mods_for_each_supported_loader() {
        let dir = test_dir("scan-multi-loader");

        let fabric_source = fabric_json("1.0.0");
        write_jar(
            &dir.join("fabric-only.jar"),
            &[("fabric.mod.json", fabric_source.as_str())],
        );

        let forge_source = forge_toml("1.0.0");
        write_jar(
            &dir.join("forge-only.jar"),
            &[("META-INF/mods.toml", forge_source.as_str())],
        );

        let multi_fabric_source = fabric_json("2.0.0");
        let multi_neo_source = forge_toml("3.0.0");
        write_jar(
            &dir.join("multi.jar"),
            &[
                ("fabric.mod.json", multi_fabric_source.as_str()),
                ("META-INF/neoforge.mods.toml", multi_neo_source.as_str()),
            ],
        );

        let local_source = "{}";
        write_jar(&dir.join("local.jar"), &[("pack.mcmeta", local_source)]);

        write_jar(
            &dir.join("quilt-only.jar"),
            &[(
                "quilt.mod.json",
                r#"{"schema_version":1,"quilt_loader":{"id":"examplemod","version":"1.0.0"}}"#,
            )],
        );

        let names = |loader: &str| {
            let mut names: Vec<String> =
                scan_mod_files(&dir, true, Some("1.21.1"), Some(loader), "mod")
                    .mod_files
                    .into_iter()
                    .map(|file| file.filename)
                    .collect();
            names.sort();
            names
        };

        assert_eq!(
            names("Fabric"),
            vec![
                "fabric-only.jar".to_string(),
                "local.jar".to_string(),
                "multi.jar".to_string()
            ]
        );
        assert_eq!(
            names("Forge"),
            vec!["forge-only.jar".to_string(), "local.jar".to_string()]
        );
        assert_eq!(
            names("NeoForge"),
            vec!["local.jar".to_string(), "multi.jar".to_string()]
        );
        assert!(names("Vanilla").is_empty());

        // The multi-loader mod must carry the version of the descriptor that
        // matches the scanned loader, never the other loader's version.
        let fabric_scan = scan_mod_files(&dir, true, Some("1.21.1"), Some("Fabric"), "mod");
        let multi = fabric_scan
            .mod_files
            .iter()
            .find(|file| file.filename == "multi.jar")
            .expect("multi-loader mod must be visible to Fabric");
        assert_eq!(multi.version, "2.0.0");

        let neo_scan = scan_mod_files(&dir, true, Some("1.21.1"), Some("NeoForge"), "mod");
        let multi = neo_scan
            .mod_files
            .iter()
            .find(|file| file.filename == "multi.jar")
            .expect("multi-loader mod must be visible to NeoForge");
        assert_eq!(multi.version, "3.0.0");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_provider_stays_separate_from_mod_loaders() {
        let dir = test_dir("source-vs-loader");
        let filename = "examplemod-1.0.0.jar";
        let jar = dir.join(filename);
        let source = fabric_json("1.0.0");
        write_jar(&jar, &[("fabric.mod.json", source.as_str())]);

        let pw = instances::load_packwiz_index(&dir);
        let meta = build_mod_metadata(
            &dir,
            &pw,
            &ProjectNameCache::new(),
            Some("1.21.1"),
            Some("fabric"),
            &ModFileEntry {
                path: jar,
                filename: filename.into(),
                enabled: true,
                mtime: UNIX_EPOCH,
                size: 0,
            },
        );

        // No sidecar/packwiz provenance: the download source is Local, while
        // the JAR descriptor still reports Fabric as a supported loader.
        assert_eq!(meta.provider, "Local");
        assert_eq!(meta.loaders, vec!["Fabric".to_string()]);
        assert_eq!(meta.version, "1.0.0");
        assert!(meta.version_known);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn curseforge_sidecar_remains_the_source_not_a_loader() {
        let dir = test_dir("curseforge-source");
        let filename = "examplemod-1.0.0.jar";
        let jar = dir.join(filename);
        let source = forge_toml("1.0.0");
        write_jar(&jar, &[("META-INF/mods.toml", source.as_str())]);
        std::fs::create_dir_all(dir.join(".index")).unwrap();
        std::fs::write(
            dir.join(".index").join("examplemod-1.0.0.voidlauncher.json"),
            r#"{"provider":"curseforge","project_id":"123456","project_name":"Example Mod","file_id":"789"}"#,
        )
        .unwrap();

        let pw = instances::load_packwiz_index(&dir);
        let meta = build_mod_metadata(
            &dir,
            &pw,
            &ProjectNameCache::new(),
            Some("1.21.1"),
            Some("forge"),
            &ModFileEntry {
                path: jar,
                filename: filename.into(),
                enabled: true,
                mtime: UNIX_EPOCH,
                size: 0,
            },
        );

        assert_eq!(meta.provider, "CurseForge");
        assert_eq!(meta.loaders, vec!["Forge".to_string()]);
        assert_eq!(meta.project_id.as_deref(), Some("123456"));
        assert_eq!(meta.version, "1.0.0");
        assert!(meta.version_known);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mod_without_descriptor_version_reports_unknown_not_empty() {
        let dir = test_dir("metadata-unknown-version");
        let filename = "mystery-mod.jar";
        let jar = dir.join(filename);
        write_jar(
            &jar,
            &[(
                "fabric.mod.json",
                r#"{"id":"mysterymod","name":"Mystery Mod","version":"${version}"}"#,
            )],
        );

        let pw = instances::load_packwiz_index(&dir);
        let meta = build_mod_metadata(
            &dir,
            &pw,
            &ProjectNameCache::new(),
            Some("1.21.1"),
            Some("fabric"),
            &ModFileEntry {
                path: jar,
                filename: filename.into(),
                enabled: true,
                mtime: UNIX_EPOCH,
                size: 0,
            },
        );

        assert_eq!(meta.version, "");
        assert!(!meta.version_known);
        assert_eq!(meta.loaders, vec!["Fabric".to_string()]);
        assert_eq!(meta.provider, "Local");
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
            format!(
                r#"{{"version":{},"entries":[{{"path":"C:/x.jar","mtime_ms":5,"size":6,"meta":{{"name":"X"}}}}]}}"#,
                META_CACHE_VERSION
            ),
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

    fn local_test_install(
        content_dir: &std::path::Path,
        options_path: Option<&std::path::Path>,
        source: &std::path::Path,
        content_type: &str,
        conflict: LocalConflictPolicy,
        expected: Option<&str>,
    ) -> Result<LocalImportResult, LocalImportError> {
        install_local_content_into(
            content_dir,
            options_path,
            source,
            content_type,
            conflict,
            expected,
        )
    }

    fn zip_entry_names(path: &std::path::Path) -> Vec<String> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        (0..archive.len())
            .filter_map(|index| {
                archive
                    .by_index(index)
                    .ok()
                    .map(|entry| entry.name().to_string())
            })
            .collect()
    }

    fn write_valid_resourcepack(path: &std::path::Path) {
        write_jar(
            path,
            &[
                ("pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
                ("assets/example/icon.txt", "icon"),
            ],
        );
    }

    #[test]
    fn local_mod_import_preserves_jar_and_marks_local_provider() {
        let dir = test_dir("local-mod-import");
        let source = dir.join("example-1.0.jar");
        write_jar(
            &source,
            &[(
                "fabric.mod.json",
                r#"{"id":"examplemod","name":"Example Mod","version":"1.0.0"}"#,
            )],
        );
        let original = std::fs::read(&source).unwrap();
        let content = dir.join("mods");

        let result = local_test_install(
            &content,
            None,
            &source,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();

        assert_eq!(result.status, LocalImportStatus::Installed);
        assert_eq!(result.filename, "example-1.0.jar");
        assert!(result.enabled);
        assert!(!result.activation_supported);
        assert_eq!(
            std::fs::read(content.join(&result.filename)).unwrap(),
            original
        );

        let meta = read_mod_meta_from_jar_pub(&content.join(&result.filename));
        assert_eq!(meta.loaders, vec!["Fabric".to_string()]);
        let installed = content::InstalledContent::read(&content, &result.filename, "mod");
        assert_eq!(installed.provider, "local");
        assert!(installed.project_id.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_import_rejects_corrupt_jar() {
        let dir = test_dir("local-mod-corrupt");
        let source = dir.join("broken.jar");
        std::fs::write(&source, b"PK\x03\x04this is not a zip").unwrap();
        let content = dir.join("mods");

        let error = local_test_install(
            &content,
            None,
            &source,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "invalidArchive");
        assert!(!content.join("broken.jar").exists());
        assert!(!std::fs::read_dir(&content)
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".voidlauncher-import")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_import_accepts_zip_without_descriptor_without_fake_loader() {
        let dir = test_dir("local-mod-no-descriptor");
        let source = dir.join("mystery.jar");
        write_jar(&source, &[("data.txt", "not a descriptor")]);
        let content = dir.join("mods");

        let result = local_test_install(
            &content,
            None,
            &source,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(result.status, LocalImportStatus::Installed);
        let meta = read_mod_meta_from_jar_pub(&content.join(&result.filename));
        assert!(!meta.has_descriptor);
        assert!(meta.loaders.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_import_accepts_legacy_descriptor() {
        let dir = test_dir("local-mod-legacy");
        let source = dir.join("legacy.jar");
        write_jar(
            &source,
            &[(
                "mcmod.info",
                r#"[{"modid":"legacy","name":"Legacy Mod","version":"1.2.3"}]"#,
            )],
        );
        let content = dir.join("mods");

        let result = local_test_install(
            &content,
            None,
            &source,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(result.status, LocalImportStatus::Installed);
        let meta = read_mod_meta_from_jar_pub(&content.join(&result.filename));
        assert_eq!(meta.loaders, vec!["Forge".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_import_is_noop_for_identical_file() {
        let dir = test_dir("local-mod-noop");
        let source = dir.join("same.jar");
        write_jar(
            &source,
            &[(
                "fabric.mod.json",
                r#"{"id":"same","name":"Same","version":"1.0.0"}"#,
            )],
        );
        let content = dir.join("mods");
        let first = local_test_install(
            &content,
            None,
            &source,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        let bytes = std::fs::read(content.join(&first.filename)).unwrap();

        let second = local_test_install(
            &content,
            None,
            &source,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(second.status, LocalImportStatus::AlreadyInstalled);
        assert_eq!(second.filename, first.filename);
        assert_eq!(std::fs::read(content.join(&first.filename)).unwrap(), bytes);
        let files = std::fs::read_dir(&content)
            .unwrap()
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .count();
        assert_eq!(files, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_import_reports_conflict_and_replaces_only_with_matching_hash() {
        let dir = test_dir("local-mod-conflict");
        let source_a = dir.join("a").join("mod.jar");
        let source_b = dir.join("b").join("mod.jar");
        std::fs::create_dir_all(source_a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source_b.parent().unwrap()).unwrap();
        write_jar(
            &source_a,
            &[(
                "fabric.mod.json",
                r#"{"id":"m","name":"M","version":"1.0"}"#,
            )],
        );
        write_jar(
            &source_b,
            &[(
                "fabric.mod.json",
                r#"{"id":"m","name":"M","version":"2.0"}"#,
            )],
        );
        let content = dir.join("mods");
        local_test_install(
            &content,
            None,
            &source_a,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        let original = std::fs::read(content.join("mod.jar")).unwrap();

        let conflict = local_test_install(
            &content,
            None,
            &source_b,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(conflict.status, LocalImportStatus::Conflict);
        assert_eq!(conflict.conflict_with.as_deref(), Some("mod.jar"));
        let expected = conflict.existing_sha256.clone().expect("hash for replace");
        assert_eq!(std::fs::read(content.join("mod.jar")).unwrap(), original);

        let replaced = local_test_install(
            &content,
            None,
            &source_b,
            "mod",
            LocalConflictPolicy::Replace,
            Some(&expected),
        )
        .unwrap();
        assert_eq!(replaced.status, LocalImportStatus::Installed);
        assert_ne!(std::fs::read(content.join("mod.jar")).unwrap(), original);

        let stale = local_test_install(
            &content,
            None,
            &source_a,
            "mod",
            LocalConflictPolicy::Replace,
            Some("deadbeef"),
        )
        .unwrap();
        assert_eq!(stale.status, LocalImportStatus::Conflict);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_import_preserves_disabled_state() {
        let dir = test_dir("local-mod-disabled");
        let source_a = dir.join("a").join("mod.jar");
        let source_b = dir.join("b").join("mod.jar");
        std::fs::create_dir_all(source_a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source_b.parent().unwrap()).unwrap();
        write_jar(
            &source_a,
            &[(
                "fabric.mod.json",
                r#"{"id":"m","name":"M","version":"1.0"}"#,
            )],
        );
        write_jar(
            &source_b,
            &[(
                "fabric.mod.json",
                r#"{"id":"m","name":"M","version":"2.0"}"#,
            )],
        );
        let content = dir.join("mods");
        local_test_install(
            &content,
            None,
            &source_a,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        std::fs::rename(content.join("mod.jar"), content.join("mod.jar.disabled")).unwrap();

        let conflict = local_test_install(
            &content,
            None,
            &source_b,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(conflict.conflict_with.as_deref(), Some("mod.jar.disabled"));
        let expected = conflict.existing_sha256.clone().unwrap();

        let replaced = local_test_install(
            &content,
            None,
            &source_b,
            "mod",
            LocalConflictPolicy::Replace,
            Some(&expected),
        )
        .unwrap();
        assert_eq!(replaced.status, LocalImportStatus::Installed);
        assert_eq!(replaced.filename, "mod.jar.disabled");
        assert!(!replaced.enabled);
        assert!(content.join("mod.jar.disabled").exists());
        assert!(!content.join("mod.jar").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_import_failure_keeps_previous_file() {
        let dir = test_dir("local-mod-failure");
        let source_a = dir.join("a").join("mod.jar");
        let bad = dir.join("b").join("mod.jar");
        std::fs::create_dir_all(source_a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
        write_jar(
            &source_a,
            &[(
                "fabric.mod.json",
                r#"{"id":"m","name":"M","version":"1.0"}"#,
            )],
        );
        std::fs::write(&bad, b"PK\x03\x04broken").unwrap();
        let content = dir.join("mods");
        local_test_install(
            &content,
            None,
            &source_a,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        let original = std::fs::read(content.join("mod.jar")).unwrap();

        let error = local_test_install(
            &content,
            None,
            &bad,
            "mod",
            LocalConflictPolicy::Replace,
            Some("irrelevant"),
        )
        .unwrap_err();
        assert_eq!(error.code, "invalidArchive");
        assert_eq!(std::fs::read(content.join("mod.jar")).unwrap(), original);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_import_is_scoped_to_one_instance_content_dir() {
        let dir = test_dir("local-mod-scope");
        let source = dir.join("scoped.jar");
        write_jar(
            &source,
            &[(
                "fabric.mod.json",
                r#"{"id":"m","name":"M","version":"1.0"}"#,
            )],
        );
        let one = dir.join("one").join("mods");
        let two = dir.join("two").join("mods");
        local_test_install(
            &one,
            None,
            &source,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert!(one.join("scoped.jar").exists());
        assert!(!std::fs::read_dir(&two)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_installs_standard_and_wrapper_archives() {
        let dir = test_dir("local-rp-standard");
        let content = dir.join("resourcepacks");
        let standard = dir.join("Faithful.zip");
        write_valid_resourcepack(&standard);
        let installed = local_test_install(
            &content,
            Some(&dir.join("options.txt")),
            &standard,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(installed.status, LocalImportStatus::Installed);
        assert!(installed.requires_activation);
        assert!(installed.activation_supported);

        let wrapper = dir.join("Wrapped.zip");
        write_jar(
            &wrapper,
            &[
                ("Wrapped/pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
                ("Wrapped/assets/example/icon-wrapped.txt", "icon"),
            ],
        );
        let installed = local_test_install(
            &content,
            Some(&dir.join("options.txt")),
            &wrapper,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(installed.filename, "Wrapped.zip");
        let names = zip_entry_names(&content.join(&installed.filename));
        assert!(names.iter().any(|name| name == "pack.mcmeta"));
        assert!(names
            .iter()
            .any(|name| name == "assets/example/icon-wrapped.txt"));
        assert!(!names.iter().any(|name| name.starts_with("Wrapped/")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_wrapper_keeps_readme_and_license() {
        let dir = test_dir("local-rp-wrapper-aux");
        let content = dir.join("resourcepacks");
        let source = dir.join("Faithful.zip");
        write_jar(
            &source,
            &[
                ("README.txt", "read me"),
                ("LICENSE", "license text"),
                ("Faithful/pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
                ("Faithful/assets/example/icon.txt", "icon"),
            ],
        );

        let installed = local_test_install(
            &content,
            Some(&dir.join("options.txt")),
            &source,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        let names = zip_entry_names(&content.join(&installed.filename));
        assert!(names.iter().any(|name| name == "pack.mcmeta"));
        assert!(names.iter().any(|name| name == "README.txt"));
        assert!(names.iter().any(|name| name == "LICENSE"));
        assert!(!names.iter().any(|name| name.starts_with("Faithful/")));

        let again = local_test_install(
            &content,
            Some(&dir.join("options.txt")),
            &source,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(again.status, LocalImportStatus::AlreadyInstalled);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_rejects_collisions_and_multiple_roots() {
        let dir = test_dir("local-rp-layout-errors");
        let content = dir.join("resourcepacks");
        let options = dir.join("options.txt");

        let collision = dir.join("collision.zip");
        write_jar(
            &collision,
            &[
                ("README.txt", "root"),
                ("Faithful/pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
                ("Faithful/README.txt", "wrapper"),
            ],
        );
        let error = local_test_install(
            &content,
            Some(&options),
            &collision,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "pathCollision");

        let two_roots = dir.join("two-roots.zip");
        write_jar(
            &two_roots,
            &[
                ("A/pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
                ("B/pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
            ],
        );
        let error = local_test_install(
            &content,
            Some(&options),
            &two_roots,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "multipleRoots");
        assert!(!content.join("collision.zip").exists());
        assert!(!content.join("two-roots.zip").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_rejects_corrupt_zip_and_path_traversal() {
        let dir = test_dir("local-rp-invalid");
        let content = dir.join("resourcepacks");
        let options = dir.join("options.txt");

        let corrupt = dir.join("corrupt.zip");
        std::fs::write(&corrupt, b"PK\x03\x04truncated").unwrap();
        let error = local_test_install(
            &content,
            Some(&options),
            &corrupt,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "invalidArchive");

        let traversal = dir.join("traversal.zip");
        write_jar(
            &traversal,
            &[
                ("../evil.txt", "evil"),
                ("pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
            ],
        );
        let error = local_test_install(
            &content,
            Some(&options),
            &traversal,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "unsafePath");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_reimport_is_noop_and_preserves_options() {
        let dir = test_dir("local-rp-noop");
        let content = dir.join("resourcepacks");
        let options = dir.join("options.txt");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(
            &options,
            "fov:70.0\nresourcePacks:[\"vanilla\",\"file/Old.zip\"]\n",
        )
        .unwrap();
        let before = std::fs::read_to_string(&options).unwrap();
        let source = dir.join("Faithful.zip");
        write_valid_resourcepack(&source);

        let first = local_test_install(
            &content,
            Some(&options),
            &source,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(first.status, LocalImportStatus::Installed);
        assert_eq!(std::fs::read_to_string(&options).unwrap(), before);
        assert!(first.requires_activation);

        let second = local_test_install(
            &content,
            Some(&options),
            &source,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(second.status, LocalImportStatus::AlreadyInstalled);
        assert_eq!(second.filename, first.filename);
        assert_eq!(std::fs::read_to_string(&options).unwrap(), before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_activation_preserves_other_packs_and_order() {
        let dir = test_dir("local-rp-enable");
        let options = dir.join("options.txt");
        std::fs::write(
            &options,
            "fov:70.0\nresourcePacks:[\"vanilla\",\"file/A.zip\",\"file/B.zip\"]\n",
        )
        .unwrap();

        instances::set_options_resource_pack_enabled(&options, "New.zip", true).unwrap();
        let (active, _) = instances::read_options_resource_packs(&options);
        assert_eq!(
            active,
            vec![
                "vanilla".to_string(),
                "file/A.zip".to_string(),
                "file/B.zip".to_string(),
                "file/New.zip".to_string()
            ]
        );
        assert!(options.with_extension("txt.bak").exists());
        let text = std::fs::read_to_string(&options).unwrap();
        assert!(text.contains("fov:70.0"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_already_in_content_dir_is_detected_without_copy() {
        let dir = test_dir("local-rp-resourcify");
        let content = dir.join("resourcepacks");
        std::fs::create_dir_all(&content).unwrap();
        let existing = content.join("Resourcify.zip");
        write_valid_resourcepack(&existing);
        let bytes = std::fs::read(&existing).unwrap();
        let options = dir.join("options.txt");
        std::fs::write(&options, "resourcePacks:[\"vanilla\"]\n").unwrap();

        let result = local_test_install(
            &content,
            Some(&options),
            &existing,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(result.status, LocalImportStatus::AlreadyInstalled);
        assert_eq!(result.filename, "Resourcify.zip");
        assert!(result.requires_activation);
        assert!(result.activation_supported);
        assert_eq!(std::fs::read(&existing).unwrap(), bytes);
        assert!(!content.join(".index").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_directory_flattens_without_double_nesting() {
        let dir = test_dir("local-rp-dir");
        let content = dir.join("resourcepacks");
        let options = dir.join("options.txt");
        let source = dir.join("download");
        let wrapper = source.join("Faithful");
        std::fs::create_dir_all(wrapper.join("assets/example")).unwrap();
        std::fs::write(
            wrapper.join("pack.mcmeta"),
            r#"{"pack":{"pack_format":15}}"#,
        )
        .unwrap();
        std::fs::write(wrapper.join("assets/example/icon.txt"), "icon").unwrap();
        std::fs::write(source.join("README.txt"), "read me").unwrap();
        std::fs::write(source.join("LICENSE"), "license").unwrap();

        let result = local_test_install(
            &content,
            Some(&options),
            &source,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(result.status, LocalImportStatus::Installed);
        let installed = content.join(&result.filename);
        assert!(installed.join("pack.mcmeta").is_file());
        assert!(installed.join("assets/example/icon.txt").is_file());
        assert!(installed.join("README.txt").is_file());
        assert!(installed.join("LICENSE").is_file());
        assert!(!installed.join("Faithful").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_directory_reimport_is_noop_and_conflict_aware() {
        let dir = test_dir("local-rp-dir-reimport");
        let content = dir.join("resourcepacks");
        let options = dir.join("options.txt");
        let source = dir.join("Faithful");
        std::fs::create_dir_all(source.join("assets")).unwrap();
        std::fs::write(source.join("pack.mcmeta"), r#"{"pack":{"pack_format":15}}"#).unwrap();
        std::fs::write(source.join("assets/icon.txt"), "icon").unwrap();

        let first = local_test_install(
            &content,
            Some(&options),
            &source,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(first.status, LocalImportStatus::Installed);
        let second = local_test_install(
            &content,
            Some(&options),
            &source,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(second.status, LocalImportStatus::AlreadyInstalled);

        std::fs::write(source.join("assets/icon.txt"), "changed").unwrap();
        let conflict = local_test_install(
            &content,
            Some(&options),
            &source,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(conflict.status, LocalImportStatus::Conflict);
        assert_eq!(conflict.conflict_with.as_deref(), Some("Faithful"));

        let expected = conflict.existing_sha256.clone().unwrap();
        let replaced = local_test_install(
            &content,
            Some(&options),
            &source,
            "resourcepack",
            LocalConflictPolicy::Replace,
            Some(&expected),
        )
        .unwrap();
        assert_eq!(replaced.status, LocalImportStatus::Installed);
        assert_eq!(
            std::fs::read_to_string(content.join("Faithful/assets/icon.txt")).unwrap(),
            "changed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_directory_with_several_roots_is_rejected() {
        let dir = test_dir("local-rp-dir-ambiguous");
        let content = dir.join("resourcepacks");
        let options = dir.join("options.txt");
        let source = dir.join("download");
        for name in ["A", "B"] {
            std::fs::create_dir_all(source.join(name)).unwrap();
            std::fs::write(
                source.join(name).join("pack.mcmeta"),
                r#"{"pack":{"pack_format":15}}"#,
            )
            .unwrap();
        }
        let error = local_test_install(
            &content,
            Some(&options),
            &source,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "multipleRoots");
        assert!(!content.join("download").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_duplicate_content_requires_explicit_keep_both() {
        let dir = test_dir("local-rp-duplicate");
        let content = dir.join("resourcepacks");
        let options = dir.join("options.txt");
        let one = dir.join("One.zip");
        let two = dir.join("Two.zip");
        let entries = [
            ("pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
            ("assets/example/icon.txt", "icon"),
        ];
        write_jar(&one, &entries);
        write_jar(&two, &entries);

        local_test_install(
            &content,
            Some(&options),
            &one,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        let duplicate = local_test_install(
            &content,
            Some(&options),
            &two,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(duplicate.status, LocalImportStatus::DuplicateContent);
        assert_eq!(duplicate.duplicate_of.as_deref(), Some("One.zip"));
        assert!(!content.join("Two.zip").exists());

        let installed = local_test_install(
            &content,
            Some(&options),
            &two,
            "resourcepack",
            LocalConflictPolicy::KeepBoth,
            None,
        )
        .unwrap();
        assert_eq!(installed.status, LocalImportStatus::Installed);
        assert!(content.join("Two.zip").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_shader_zip_and_directory_install_without_activation_management() {
        let dir = test_dir("local-shader-install");
        let content = dir.join("shaderpacks");
        let options = dir.join("options.txt");
        let zip = dir.join("MyShader.zip");
        write_jar(
            &zip,
            &[
                ("shaders/lib/shader.glsl", "shader"),
                ("shaderpack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
            ],
        );
        let installed = local_test_install(
            &content,
            Some(&options),
            &zip,
            "shader",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(installed.status, LocalImportStatus::Installed);
        assert!(!installed.activation_supported);
        assert!(!installed.requires_activation);

        let folder = dir.join("FolderShader");
        std::fs::create_dir_all(folder.join("shaders")).unwrap();
        std::fs::write(folder.join("shaders/vertex.glsl"), "shader").unwrap();
        let installed = local_test_install(
            &content,
            Some(&options),
            &folder,
            "shader",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(installed.filename, "FolderShader");
        assert!(content.join("FolderShader/shaders/vertex.glsl").is_file());
        assert!(!content.join("FolderShader/FolderShader").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_shader_wrapper_keeps_readme_and_rejects_corrupt_archive() {
        let dir = test_dir("local-shader-wrapper");
        let content = dir.join("shaderpacks");
        let options = dir.join("options.txt");
        let source = dir.join("CoolShader.zip");
        write_jar(
            &source,
            &[
                ("README.txt", "read me"),
                ("LICENSE", "license"),
                ("CoolShader/shaders/lib.glsl", "shader"),
            ],
        );
        let installed = local_test_install(
            &content,
            Some(&options),
            &source,
            "shader",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        let names = zip_entry_names(&content.join(&installed.filename));
        assert!(names.iter().any(|name| name == "shaders/lib.glsl"));
        assert!(names.iter().any(|name| name == "README.txt"));
        assert!(!names.iter().any(|name| name.starts_with("CoolShader/")));

        let corrupt = dir.join("broken.zip");
        std::fs::write(&corrupt, b"PK\x03\x04broken").unwrap();
        let error = local_test_install(
            &content,
            Some(&options),
            &corrupt,
            "shader",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "invalidArchive");

        let traversal = dir.join("traversal.zip");
        write_jar(
            &traversal,
            &[("../evil.glsl", "evil"), ("shaders/ok.glsl", "ok")],
        );
        let error = local_test_install(
            &content,
            Some(&options),
            &traversal,
            "shader",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "unsafePath");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_keep_both_resolves_a_name_conflict_without_overwrite() {
        let dir = test_dir("local-mod-keep-both");
        let source_a = dir.join("a").join("mod.jar");
        let source_b = dir.join("b").join("mod.jar");
        std::fs::create_dir_all(source_a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source_b.parent().unwrap()).unwrap();
        write_jar(
            &source_a,
            &[(
                "fabric.mod.json",
                r#"{"id":"m","name":"M","version":"1.0"}"#,
            )],
        );
        write_jar(
            &source_b,
            &[(
                "fabric.mod.json",
                r#"{"id":"m","name":"M","version":"2.0"}"#,
            )],
        );
        let content = dir.join("mods");
        local_test_install(
            &content,
            None,
            &source_a,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        let original = std::fs::read(content.join("mod.jar")).unwrap();

        let kept = local_test_install(
            &content,
            None,
            &source_b,
            "mod",
            LocalConflictPolicy::KeepBoth,
            None,
        )
        .unwrap();
        assert_eq!(kept.status, LocalImportStatus::Installed);
        assert_eq!(kept.filename, "mod (2).jar");
        assert_eq!(std::fs::read(content.join("mod.jar")).unwrap(), original);
        assert_ne!(
            std::fs::read(content.join("mod (2).jar")).unwrap(),
            original
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mod_duplicate_content_under_another_name_requires_confirmation() {
        let dir = test_dir("local-mod-duplicate-name");
        let one = dir.join("one.jar");
        let two = dir.join("two.jar");
        let entries = [(
            "fabric.mod.json",
            r#"{"id":"same","name":"Same","version":"1.0.0"}"#,
        )];
        write_jar(&one, &entries);
        write_jar(&two, &entries);
        let content = dir.join("mods");

        local_test_install(
            &content,
            None,
            &one,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        let duplicate = local_test_install(
            &content,
            None,
            &two,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(duplicate.status, LocalImportStatus::DuplicateContent);
        assert_eq!(duplicate.duplicate_of.as_deref(), Some("one.jar"));
        assert!(!content.join("two.jar").exists());

        let installed = local_test_install(
            &content,
            None,
            &two,
            "mod",
            LocalConflictPolicy::KeepBoth,
            None,
        )
        .unwrap();
        assert_eq!(installed.status, LocalImportStatus::Installed);
        assert!(content.join("two.jar").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_batch_partial_success_is_retryable_without_touching_installed_content() {
        let dir = test_dir("local-batch-retry");
        let content = dir.join("mods");
        let good = dir.join("Good.jar");
        let bad = dir.join("Bad.jar");
        write_jar(
            &good,
            &[(
                "fabric.mod.json",
                r#"{"id":"good","name":"Good","version":"1.0"}"#,
            )],
        );
        std::fs::write(&bad, b"PK\x03\x04broken").unwrap();

        let first = local_test_install(
            &content,
            None,
            &good,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(first.status, LocalImportStatus::Installed);
        let good_bytes = std::fs::read(content.join(&first.filename)).unwrap();
        assert!(content
            .join(".index")
            .join("Good.voidlauncher.json")
            .is_file());

        // The second file of the batch fails. The first file, its sidecar and
        // the live content folder must stay untouched, with no staging or
        // parked leftovers.
        let error = local_test_install(
            &content,
            None,
            &bad,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "invalidArchive");
        assert_eq!(
            std::fs::read(content.join(&first.filename)).unwrap(),
            good_bytes
        );
        assert!(!content.join("Bad.jar").exists());
        assert!(!content
            .join(".index")
            .join("Bad.voidlauncher.json")
            .exists());
        for entry in std::fs::read_dir(&content).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with(".voidlauncher-"),
                "staging leftover: {name}"
            );
            assert!(
                !name.ends_with(".voidlauncher-old"),
                "parked leftover: {name}"
            );
        }

        // Repeating the successful file is a safe no-op, not a second copy.
        let retry_good = local_test_install(
            &content,
            None,
            &good,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(retry_good.status, LocalImportStatus::AlreadyInstalled);
        assert_eq!(retry_good.filename, first.filename);
        assert_eq!(
            std::fs::read(content.join(&first.filename)).unwrap(),
            good_bytes
        );

        // Fixing only the failed source and retrying installs just that file.
        write_jar(
            &bad,
            &[(
                "fabric.mod.json",
                r#"{"id":"bad","name":"Bad","version":"1.0"}"#,
            )],
        );
        let retry_bad = local_test_install(
            &content,
            None,
            &bad,
            "mod",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(retry_bad.status, LocalImportStatus::Installed);
        assert!(content.join("Bad.jar").is_file());
        assert!(content
            .join(".index")
            .join("Bad.voidlauncher.json")
            .is_file());
        assert_eq!(
            std::fs::read(content.join(&first.filename)).unwrap(),
            good_bytes,
            "retrying the failed file must not rewrite the installed one"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_preserves_a_disabled_existing_target_on_replace() {
        let dir = test_dir("local-rp-disabled-replace");
        let content = dir.join("resourcepacks");
        let options = dir.join("options.txt");
        let source_a = dir.join("a").join("Pack.zip");
        let source_b = dir.join("b").join("Pack.zip");
        std::fs::create_dir_all(source_a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source_b.parent().unwrap()).unwrap();
        write_jar(
            &source_a,
            &[
                ("pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
                ("assets/example/a.txt", "a"),
            ],
        );
        write_jar(
            &source_b,
            &[
                ("pack.mcmeta", r#"{"pack":{"pack_format":15}}"#),
                ("assets/example/b.txt", "b"),
            ],
        );
        local_test_install(
            &content,
            Some(&options),
            &source_a,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        std::fs::rename(content.join("Pack.zip"), content.join("Pack.zip.disabled")).unwrap();

        let conflict = local_test_install(
            &content,
            Some(&options),
            &source_b,
            "resourcepack",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap();
        assert_eq!(conflict.conflict_with.as_deref(), Some("Pack.zip.disabled"));
        let expected = conflict.existing_sha256.clone().unwrap();
        let replaced = local_test_install(
            &content,
            Some(&options),
            &source_b,
            "resourcepack",
            LocalConflictPolicy::Replace,
            Some(&expected),
        )
        .unwrap();
        assert_eq!(replaced.filename, "Pack.zip.disabled");
        assert!(content.join("Pack.zip.disabled").is_file());
        assert!(!content.join("Pack.zip").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_resourcepack_target_kind_mismatch_leaves_existing_content_intact() {
        let dir = test_dir("local-rp-kind-mismatch");
        let content = dir.join("resourcepacks");
        let options = dir.join("options.txt");
        let existing = content.join("Pack.zip");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("keep.txt"), "keep").unwrap();
        let source = dir.join("Pack.zip");
        write_valid_resourcepack(&source);

        let error = local_test_install(
            &content,
            Some(&options),
            &source,
            "resourcepack",
            LocalConflictPolicy::Replace,
            Some("irrelevant"),
        )
        .unwrap_err();
        assert_eq!(error.code, "ambiguousTarget");
        assert_eq!(
            std::fs::read_to_string(existing.join("keep.txt")).unwrap(),
            "keep"
        );
        assert!(!std::fs::read_dir(&content)
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".voidlauncher-import")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_import_result_and_error_serialize_with_the_frontend_contract() {
        let result = LocalImportResult {
            status: LocalImportStatus::AlreadyInstalled,
            filename: "Pack.zip".into(),
            content_type: "resourcepack".into(),
            enabled: false,
            activation_supported: true,
            requires_activation: true,
            duplicate_of: None,
            conflict_with: None,
            existing_sha256: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["status"], "alreadyInstalled");
        assert_eq!(json["contentType"], "resourcepack");
        assert_eq!(json["activationSupported"], true);
        assert_eq!(json["requiresActivation"], true);
        assert!(json.get("duplicateOf").is_none());

        let error = LocalImportError::new("multipleRoots", "several roots");
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["code"], "multipleRoots");
        assert_eq!(json["message"], "several roots");
    }

    #[test]
    fn local_shader_rejects_path_collisions_after_flattening() {
        let dir = test_dir("local-shader-collision");
        let content = dir.join("shaderpacks");
        let options = dir.join("options.txt");
        let source = dir.join("collision.zip");
        write_jar(
            &source,
            &[
                ("README.txt", "root"),
                ("CoolShader/shaders/lib.glsl", "shader"),
                ("CoolShader/README.txt", "wrapper"),
            ],
        );
        let error = local_test_install(
            &content,
            Some(&options),
            &source,
            "shader",
            LocalConflictPolicy::Reject,
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "pathCollision");
        assert!(!content.join("collision.zip").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

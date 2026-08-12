//! Mod & resource-pack commands: Modrinth / CurseForge API wrappers,
//! mod installation/download, local mod metadata parsing, mod icons.

use crate::commands::instances::validate_instance_name;
use crate::curseforge;
use crate::download;
use crate::instances;
use crate::modrinth;
use crate::AppState;
use crate::is_allowed_download_host;
use std::io::Read as _;
use tauri::State;

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
}

#[tauri::command]
pub async fn cmd_check_mod_updates(
    state: State<'_, AppState>,
    instance_name: String,
    mc_version: Option<String>,
    loader: Option<String>,
    pack_type: String,
) -> Result<Vec<ModUpdateResult>, String> {
    validate_instance_name(&instance_name)?;

    struct ModFile {
        filename: String,
        name: String,
        version: String,
        hash: Option<String>,
    }

    let packs_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        instance.minecraft_dir(&config.instances_dir()).join(&pack_type)
    };

    if !packs_dir.exists() {
        return Ok(Vec::new());
    }

    let is_mods = pack_type == "mods";

    let mut mod_files: Vec<ModFile> = Vec::new();
    let mut hash_to_mod: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut all_hashes: Vec<String> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&packs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if filename.starts_with('.') || filename.ends_with(".voidlauncher.json") { continue; }

            if is_mods {
                // Mods: only .jar files, with loader filtering
                let is_jar = filename.ends_with(".jar") || filename.ends_with(".jar.disabled");
                if !is_jar { continue; }

                let meta = read_mod_meta_from_jar(&path);
                let matches_loader = match loader.as_deref() {
                    Some("Fabric") => meta.provider == "Fabric" || meta.provider == "Local",
                    Some("Forge") => meta.provider == "Forge" || meta.provider == "Local",
                    Some("NeoForge") => meta.provider == "NeoForge" || meta.provider == "Local",
                    Some("Vanilla") => false,
                    _ => true,
                };
                if !matches_loader { continue; }

                let hash = download::hash_file_sha1(&path).ok();

                if let Some(ref h) = hash {
                    let idx = mod_files.len();
                    hash_to_mod.insert(h.clone(), idx);
                    all_hashes.push(h.clone());
                }

                mod_files.push(ModFile {
                    filename,
                    name: meta.name,
                    version: meta.version,
                    hash,
                });
            } else {
                // Resourcepacks / Shaderpacks: only .zip files (skip directories)
                let is_zip = filename.ends_with(".zip") || filename.ends_with(".zip.disabled");
                if !is_zip { continue; }

                let (name, version) = read_pack_name_and_version(&path)
                    .unwrap_or_else(|| (instances::strip_minecraft_color_codes(
                        std::path::Path::new(&filename).file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or(&filename)
                    ), String::new()));

                let hash = download::hash_file_sha1(&path).ok();

                if let Some(ref h) = hash {
                    let idx = mod_files.len();
                    hash_to_mod.insert(h.clone(), idx);
                    all_hashes.push(h.clone());
                }

                mod_files.push(ModFile {
                    filename,
                    name,
                    version,
                    hash,
                });
            }
        }
    }

    if all_hashes.is_empty() {
        return Ok(Vec::new());
    }

    // Query Modrinth update API
    // For mods: pass loader filter; for packs: no loader filter
    let loaders = if is_mods {
        loader.map(|l| vec![l.to_lowercase()])
    } else {
        None
    };
    let game_versions = mc_version.map(|v| vec![v]);

    let response = modrinth::check_version_updates(
        all_hashes,
        "sha1",
        loaders,
        game_versions,
    ).await.map_err(|e| e.to_string())?;

    let mut results = Vec::new();

    for (hash_str, maybe_version) in &response {
        let Some(version) = maybe_version else { continue; };
        let Some(&idx) = hash_to_mod.get(hash_str) else { continue; };
        let mod_file = &mod_files[idx];

        let file = version.files.iter()
            .find(|f| f.primary)
            .or_else(|| version.files.first());
        let Some(primary_file) = file else { continue; };

        let installed_hash = mod_file.hash.as_deref().unwrap_or("");
        let file_hash = primary_file.hashes.get("sha1")
            .or_else(|| primary_file.hashes.get("sha512"))
            .map(|s| s.as_str())
            .unwrap_or("");

        if installed_hash == file_hash {
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
        });
    }

    Ok(results)
}

/// Read pack name and version from sidecar metadata, falling back to pack.mcmeta
fn read_pack_name_and_version(path: &std::path::Path) -> Option<(String, String)> {
    // Try sidecar first
    let filename = path.file_name()?.to_string_lossy().to_string();
    let meta_name = format!("{}.voidlauncher.json", filename);
    let meta_path = path.parent()?.join(&meta_name);
    if let Ok(contents) = std::fs::read_to_string(&meta_path) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents) {
            let name = val["project_name"].as_str().unwrap_or("").to_string();
            let version = val["version_number"].as_str().unwrap_or("").to_string();
            if !name.is_empty() {
                return Some((name, version));
            }
        }
    }

    // Fallback to pack.mcmeta (name only, no version in standard format)
    if let Ok(file) = std::fs::File::open(path) {
        if let Ok(mut archive) = zip::ZipArchive::new(file) {
            if let Ok(mcmeta_file) = archive.by_name("pack.mcmeta") {
                let bytes: Vec<u8> = mcmeta_file.bytes().filter_map(|b| b.ok()).collect();
                if let Ok(content) = String::from_utf8(bytes) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(name) = json["pack"].as_object()
                            .and_then(|p| p.get("description"))
                            .and_then(|d| d.as_str())
                            .or_else(|| json["pack"].as_object()
                                .and_then(|p| p.get("name"))
                                .and_then(|n| n.as_str()))
                        {
                            return Some((instances::strip_minecraft_color_codes(name), String::new()));
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
pub async fn cmd_search_curseforge(
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
    curseforge::search_mods(
        &query,
        mc_version.as_deref(),
        loader.as_deref(),
        offset,
        limit,
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
) -> Result<curseforge::CfFilesResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::get_mod_files(mod_id, mc_version.as_deref(), loader.as_deref(), &api_key)
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

#[derive(Debug, serde::Serialize)]
pub struct ModMetadata {
    pub filename: String,
    pub name: String,
    pub version: String,
    pub provider: String,
    pub enabled: bool,
    pub file_size: u64,
    pub icon: Option<String>,
    pub slug: Option<String>,
    pub project_id: Option<String>,
    /// `true` when the slug came from the `.voidlauncher.json` sidecar
    /// (i.e. the mod was installed via the launcher and the slug is the
    /// verified Modrinth/CurseForge project ID). `false` when the slug is
    /// derived from the jar's internal metadata (e.g. `fabric.mod.json`
    /// `id`), which is the mod's *internal* identifier and may NOT match
    /// the Modrinth project slug вЂ” causing false-positive "incompatible"
    /// warnings in the compatibility check.
    #[serde(default)]
    pub slug_verified: bool,
}

#[tauri::command]
pub async fn cmd_install_mod(
    state: State<'_, AppState>,
    instance_name: String,
    modrinth_version_id: Option<String>,
    #[allow(unused_variables)] curseforge_file_id: Option<i32>,
    file_name: String,
    download_url: String,
    project_id: Option<String>,
    project_name: Option<String>,
    version_number: Option<String>,
    provider: String,
) -> Result<String, String> {
    validate_instance_name(&instance_name)?;
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
    let (mods_dir, safe_name, dest) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        let mods_dir = instance.mods_dir(&config.instances_dir());
        let safe_name = std::path::Path::new(&file_name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid file name")?
            .to_string();
        let dest = mods_dir.join(&safe_name);
        (mods_dir, safe_name, dest)
    };
    download::download_file(&download_url, &dest, "")
        .await
        .map_err(|e| e.to_string())?;
    // No SHA1 is available for mod downloads вЂ” at least verify the result is
    // a real JAR archive and not an error page / bogus response.
    download::verify_zip_magic(&dest).map_err(|e| e.to_string())?;
    let final_name = safe_name.clone();

    // Write sidecar so the installed mod can be tracked back to its source.
    if let Some(pid) = project_id.as_deref() {
        let sidecar = serde_json::json!({
            "provider": provider,
            "project_id": pid,
            "project_name": project_name,
            "version_id": modrinth_version_id,
            "version_number": version_number,
        });
        let sidecar_path = mods_dir.join(format!(
            "{}.voidlauncher.json",
            safe_name.trim_end_matches(".jar")
        ));
        let _ = std::fs::write(sidecar_path, sidecar.to_string());
    }

    Ok(final_name)
}

#[tauri::command]
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
) -> Result<String, String> {
    validate_instance_name(&instance_name)?;
    if !download_url.starts_with("https://") {
        return Err("Download URL must be HTTPS.".to_string());
    }
    if !is_allowed_download_host(&download_url) {
        return Err(format!(
            "Download host is not in the allowlist: {}",
            download_url
        ));
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
    let (dest_dir, safe_name, dest) = {
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
        let dest = dest_dir.join(&safe_name);
        (dest_dir, safe_name, dest)
    };
    // If replacing an old file, delete it first so we don't end up with duplicates.
    // `old_filename` is untrusted frontend input: strip any path components so
    // it can never escape `dest_dir` (no traversal, no absolute paths).
    if let Some(ref raw_old) = old_filename {
        let old = std::path::Path::new(raw_old)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty() && *n != "." && *n != "..")
            .ok_or("Invalid old file name")?;
        let old_stem = std::path::Path::new(old)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(old);
        let old_base = if old_stem.ends_with(".jar") || old_stem.ends_with(".zip") {
            old_stem.to_string()
        } else {
            old.to_string()
        };
        // Remove the main file (try both .jar and .zip extensions)
        for ext in &[".jar", ".zip"] {
            let candidate = dest_dir.join(format!("{}{}", old_base, ext));
            let _ = std::fs::remove_file(&candidate);
        }
    }
    download::download_file(&download_url, &dest, &expected_sha1)
        .await
        .map_err(|e| e.to_string())?;
    // When no SHA1 is available, at least verify the result is a real
    // archive (JAR/ZIP magic) and not an HTML error page / bogus body.
    if expected_sha1.is_empty() && (safe_name.ends_with(".jar") || safe_name.ends_with(".zip")) {
        download::verify_zip_magic(&dest).map_err(|e| e.to_string())?;
    }

    // Write sidecar so the installed file can be tracked back to its source.
    if let Some(pid) = project_id.as_deref() {
        let sidecar = serde_json::json!({
            "provider": provider,
            "project_id": pid,
            "project_name": project_name,
            "version_id": version_id,
            "version_number": version_number,
        });
        let sidecar_path = dest_dir.join(format!(
            "{}.voidlauncher.json",
            safe_name.trim_end_matches(".jar")
        ));
        let _ = std::fs::write(sidecar_path, sidecar.to_string());
    }

    Ok(safe_name)
}

#[tauri::command]
pub fn cmd_list_instance_mods(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<ModMetadata>, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let mods_dir = instance.mods_dir(&config.instances_dir());
    if !mods_dir.exists() {
        return Ok(Vec::new());
    }
    let mut mods = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            // Skip the voidlauncher sidecar files
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
            let sidecar_project_id = read_mod_sidecar_slug(&mods_dir, &filename);
            let (slug, slug_verified) = sidecar_project_id.as_ref()
                .map(|s| (Some(s.clone()), true))
                .unwrap_or_else(|| (meta.slug, false));
            mods.push(ModMetadata {
                filename,
                name: meta.name,
                version: meta.version,
                provider: meta.provider,
                enabled,
                file_size,
                icon: meta.icon,
                slug,
                project_id: sidecar_project_id,
                slug_verified,
            });
        }
    }
    mods.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(mods)
}

#[tauri::command]
pub fn cmd_remove_instance_mod(
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
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let mods_dir = instance.mods_dir(&config.instances_dir());
    let mod_path = mods_dir.join(&safe_name);
    if mod_path.exists() {
        std::fs::remove_file(&mod_path).map_err(|e| e.to_string())?;
    }
    // Also remove any sidecar
    let sidecar = mods_dir.join(format!(
        "{}.voidlauncher.json",
        safe_name
            .trim_end_matches(".jar")
            .trim_end_matches(".disabled")
    ));
    let _ = std::fs::remove_file(sidecar);
    Ok(())
}

#[tauri::command]
pub fn cmd_get_mod_metadata(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<ModMetadata>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let mods_dir = instance.minecraft_dir(&config.instances_dir()).join("mods");
    if !mods_dir.exists() {
        return Ok(Vec::new());
    }
    let mut mods = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let is_jar = filename.ends_with(".jar");
            let is_disabled = filename.ends_with(".jar.disabled");
            if is_jar || is_disabled {
                let enabled = is_jar && !is_disabled;
                let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let meta = read_mod_meta_from_jar(&path);
                // Check sidecar for verified Modrinth/CurseForge project slug.
                let sidecar_project_id = read_mod_sidecar_slug(&mods_dir, &filename);
                let (slug, slug_verified) = sidecar_project_id.as_ref()
                    .map(|s| (Some(s.clone()), true))
                    .unwrap_or_else(|| (meta.slug, false));
                // Prefer the sidecar provider (modrinth/curseforge/local) over the JAR metadata loader name
                let sidecar_provider = read_mod_sidecar_provider(&mods_dir, &filename);
                let provider = match sidecar_provider.as_deref() {
                    Some(s) => match s.to_lowercase().as_str() {
                        "modrinth" => "Modrinth".to_string(),
                        "curseforge" => "CurseForge".to_string(),
                        "local" => "Local".to_string(),
                        other => other.to_string(),
                    },
                    None => meta.provider.clone(),
                };
                mods.push(ModMetadata {
                    filename,
                    name: meta.name,
                    version: meta.version,
                    provider,
                    enabled,
                    file_size,
                    icon: meta.icon,
                    slug,
                    project_id: sidecar_project_id,
                    slug_verified,
                });
            }
        }
    }
    mods.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(mods)
}

/// Read the Modrinth/CurseForge project slug from the `.voidlauncher.json`
/// sidecar file that `cmd_install_mod` writes at download time.
fn read_mod_sidecar_slug(mods_dir: &std::path::Path, filename: &str) -> Option<String> {
    let stem = filename
        .trim_end_matches(".jar")
        .trim_end_matches(".disabled");
    let sidecar_path = mods_dir.join(format!("{}.voidlauncher.json", stem));
    let contents = std::fs::read_to_string(sidecar_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&contents).ok()?;
    json["project_id"].as_str().map(|s| s.to_string())
}

/// Read the provider (modrinth/curseforge/local) from the `.voidlauncher.json` sidecar.
fn read_mod_sidecar_provider(mods_dir: &std::path::Path, filename: &str) -> Option<String> {
    let stem = filename
        .trim_end_matches(".jar")
        .trim_end_matches(".disabled");
    let sidecar_path = mods_dir.join(format!("{}.voidlauncher.json", stem));
    let contents = std::fs::read_to_string(sidecar_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&contents).ok()?;
    json["provider"].as_str().map(|s| s.to_string())
}

struct ModMetaResult {
    name: String,
    version: String,
    provider: String,
    icon: Option<String>,
    slug: Option<String>,
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
            .map_or(false, |c| c.is_ascii_digit())
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
                let name = json["name"]
                    .as_str()
                    .or_else(|| json["id"].as_str())
                    .unwrap_or("Unknown")
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
                    name,
                    version,
                    provider: "Fabric".into(),
                    icon,
                    slug,
                };
            }
        }
    }

    // Try META-INF/mods.toml (Forge/NeoForge)
    for toml_name in &["META-INF/mods.toml", "META-INF/neoforge.mods.toml"] {
        if let Ok(mut file) = archive.by_name(toml_name) {
            let mut contents = String::new();
            if file.read_to_string(&mut contents).is_ok() {
                if let Ok(toml_val) = contents.parse::<toml::Value>() {
                    if let Some(mods_arr) = toml_val.get("mods").and_then(|v| v.as_array()) {
                        if let Some(first_mod) = mods_arr.first() {
                            let mod_id = first_mod
                                .get("modId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let display_name = first_mod
                                .get("displayName")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&mod_id)
                                .to_string();
                            let version = first_mod
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let logo = first_mod
                                .get("logoFile")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let provider = if toml_name.contains("neoforge") {
                                "NeoForge"
                            } else {
                                "Forge"
                            };
                            return ModMetaResult {
                                name: display_name,
                                version,
                                provider: provider.into(),
                                icon: logo,
                                slug: Some(mod_id),
                            };
                        }
                    }
                    // Inline format: mods = [{modId = "x", ...}]
                    if let Some(mods_arr) = toml_val.get("mods").and_then(|v| v.as_array()) {
                        if let Some(first_mod) = mods_arr.first() {
                            let mod_id = first_mod
                                .get("modId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let display_name = first_mod
                                .get("displayName")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&mod_id)
                                .to_string();
                            let version = first_mod
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let logo = first_mod
                                .get("logoFile")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let provider = if toml_name.contains("neoforge") {
                                "NeoForge"
                            } else {
                                "Forge"
                            };
                            return ModMetaResult {
                                name: display_name,
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
                let display_name =
                    extract_toml_field(&contents, "displayName").unwrap_or_else(|| mod_id.clone());
                let version =
                    extract_toml_field(&contents, "version").unwrap_or("Unknown".to_string());
                let logo = extract_toml_field(&contents, "logoFile");
                let provider = if toml_name.contains("neoforge") {
                    "NeoForge"
                } else {
                    "Forge"
                };
                return ModMetaResult {
                    name: display_name,
                    version,
                    provider: provider.into(),
                    icon: logo,
                    slug: Some(mod_id),
                };
            }
        }
    }

    // Fallback: extract name from filename
    fallback_meta_from_filename(path)
}

fn strip_toml_comment(val: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = val.char_indices();
    while let Some((i, c)) = chars.next() {
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
            let raw = trimmed.splitn(2, '=').nth(1)?.trim();
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
    let safe_filename = std::path::Path::new(&filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?
        .to_string();

    let (jar_path, curseforge_api_key) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        let mods_dir = instance.minecraft_dir(&config.instances_dir()).join("mods");
        let jar_path = mods_dir.join(&safe_filename);
        let cf_key = config.curseforge_api_key.clone();
        (jar_path, cf_key)
    };

    if !jar_path.exists() {
        return Ok(None);
    }

    // Try extracting icon from the jar itself
    if let Ok(Some(icon)) = extract_icon_from_jar(&jar_path) {
        return Ok(Some(icon));
    }

    // Fallback: try CurseForge API by project_id from sidecar (only if API key is configured)
    if !curseforge_api_key.is_empty() {
        let sidecar_name = format!(
            "{}.voidlauncher.json",
            safe_filename.trim_end_matches(".jar")
        );
        if let Some(sidecar_path) = jar_path.parent() {
            let sidecar_path = sidecar_path.join(&sidecar_name);
            if let Ok(contents) = std::fs::read_to_string(&sidecar_path) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents) {
                    if val["provider"].as_str() == Some("curseforge") {
                        if let Some(pid_str) = val["project_id"].as_str() {
                            if let Ok(pid) = pid_str.parse::<u64>() {
                                if let Ok(Some(icon)) =
                                    fetch_curseforge_mod_icon(pid, &curseforge_api_key).await
                                {
                                    return Ok(Some(icon));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Fallback: try Modrinth project API for icon_url
    let sidecar_name = format!("{}.voidlauncher.json", safe_filename.trim_end_matches(".jar"));
    if let Some(mods_dir) = jar_path.parent() {
        let sidecar_path = mods_dir.join(&sidecar_name);
        if let Ok(contents) = std::fs::read_to_string(&sidecar_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents) {
                if val["provider"].as_str() == Some("modrinth") {
                    if let Some(pid) = val["project_id"].as_str().map(|s| s.to_string()) {
                        if let Ok(project) = modrinth::get_project(&pid).await {
                            if let Some(icon_url) = project.icon_url {
                                if let Some(icon_data) = fetch_remote_icon(&icon_url).await {
                                    return Ok(Some(icon_data));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Fetch a mod's logo icon from CurseForge API and return as base64 data URL
async fn fetch_curseforge_mod_icon(
    project_id: u64,
    api_key: &str,
) -> crate::error::Result<Option<String>> {
    let detail = crate::curseforge::get_mod(project_id, api_key)
        .await
        .map_err(|e| crate::error::LauncherError::Download(format!("CF API error: {}", e)))?;

    let thumbnail_url = match detail.logo {
        Some(logo) if !logo.thumbnail_url.is_empty() => logo.thumbnail_url,
        _ => return Ok(None),
    };

    let client = crate::download::global_http_client();
    let resp = client.get(&thumbnail_url).send().await.map_err(|e| {
        crate::error::LauncherError::Download(format!("Failed to fetch CF logo: {}", e))
    })?;

    if !resp.status().is_success() {
        return Ok(None);
    }

    let bytes = resp.bytes().await.map_err(|e| {
        crate::error::LauncherError::Download(format!("Failed to read CF logo: {}", e))
    })?;

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
    let client = crate::download::global_http_client();
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() { return None; }
    let bytes = resp.bytes().await.ok()?;
    let b64 = base64_encode(&bytes);
    let mime = if url.ends_with(".jpg") || url.ends_with(".jpeg") { "image/jpeg" } else { "image/png" };
    Some(format!("data:{};base64,{}", mime, b64))
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
                    if let Ok(toml_val) = contents.parse::<toml::Value>() {
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
    for candidate in &["icon.png", "logo.png", "assets/icon.png"] {
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
            if (lower.ends_with("icon.png") || lower.ends_with("logo.png"))
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

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
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

use serde::{Deserialize, Serialize};
use crate::error::{LauncherError, Result};
use crate::instances::{self, Instance, LoaderType};
use crate::modrinth;
use crate::modrinth::ModrinthFile;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::Semaphore;
use futures::future::join_all;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum ModpackFormat {
    Prism,
    Modrinth,
    CurseForge,
    ATLauncher,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModpackMetadata {
    pub format: ModpackFormat,
    pub name: String,
    pub mc_version: Option<String>,
    pub loader: Option<String>,
    pub loader_version: Option<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportProgressPayload {
    pub stage: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
}

fn emit_progress(app: Option<&AppHandle>, stage: &str, current: usize, total: usize, message: &str) {
    if let Some(app) = app {
        let _ = app.emit("import-progress", ImportProgressPayload {
            stage: stage.to_string(),
            current,
            total,
            message: message.to_string(),
        });
    }
}

/// Reject any path component that equals ".." to prevent zip slip.
fn check_safe_relative(relative: &str) -> Result<()> {
    if relative.contains('\0') {
        return Err(LauncherError::Instance("Null byte in zip entry path".into()));
    }
    let normalised = relative.replace('\\', "/");
    for component in normalised.split('/') {
        if component == ".." {
            return Err(LauncherError::Instance(format!(
                "Path traversal detected in zip entry: {}", relative
            )));
        }
    }
    Ok(())
}

/// Extract a zip entry to disk under `base`, verifying no path traversal.
fn extract_entry(base: &Path, entry_name: &str, data: &[u8]) -> Result<()> {
    check_safe_relative(entry_name)?;
    let target = base.join(entry_name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, data)?;
    Ok(())
}

fn normalize_zip_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

/// Client-side import: skip files marked client=unsupported (server-only).
fn mrpack_client_required(file_entry: &serde_json::Value) -> bool {
    match file_entry
        .get("env")
        .and_then(|e| e.get("client"))
        .and_then(|c| c.as_str())
    {
        Some("unsupported") => false,
        _ => true,
    }
}

fn pick_modrinth_file<'a>(
    files: &'a [ModrinthFile],
    filename: &str,
    hashes: &serde_json::Value,
) -> Option<&'a ModrinthFile> {
    if let Some(h) = hashes["sha1"].as_str() {
        if let Some(f) = files
            .iter()
            .find(|f| f.hashes.get("sha1").map(|v| v == h).unwrap_or(false))
        {
            return Some(f);
        }
    }
    if let Some(h) = hashes["sha512"].as_str() {
        if let Some(f) = files
            .iter()
            .find(|f| f.hashes.get("sha512").map(|v| v == h).unwrap_or(false))
        {
            return Some(f);
        }
    }
    files
        .iter()
        .find(|f| f.filename == filename)
        .or_else(|| files.iter().find(|f| f.primary))
        .or_else(|| files.first())
}

async fn resolve_mrpack_urls(file_entry: &serde_json::Value, path_str: &str) -> Vec<String> {
    let mut urls: Vec<String> = file_entry["downloads"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| s.starts_with("https://"))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    if !urls.is_empty() {
        return urls;
    }

    let filename = path_str.rsplit('/').next().unwrap_or(path_str);
    let hashes = &file_entry["hashes"];

    if let Some(vid) = file_entry["version_id"].as_str().or_else(|| {
        file_entry["versions"]
            .as_array()
            .and_then(|v| v.first())
            .and_then(|x| x.as_str())
    }) {
        if let Ok(version) = modrinth::get_version_by_id(vid).await {
            if let Some(file) = pick_modrinth_file(&version.files, filename, hashes) {
                urls.push(file.url.clone());
                return urls;
            }
        }
    }

    for (hash, algo) in [
        hashes["sha512"].as_str().map(|h| (h, "sha512")),
        hashes["sha1"].as_str().map(|h| (h, "sha1")),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(version) = modrinth::get_version_by_hash(hash, algo).await {
            if let Some(file) = pick_modrinth_file(&version.files, filename, hashes) {
                urls.push(file.url.clone());
                return urls;
            }
        }
    }

    if let Some(pid) = file_entry["project_id"].as_str() {
        if let Ok(versions) = modrinth::get_project_versions(pid).await {
            for version in &versions {
                if let Some(file) = pick_modrinth_file(&version.files, filename, hashes) {
                    urls.push(file.url.clone());
                    return urls;
                }
            }
        }
    }

    urls
}

fn write_mrpack_sidecar(dest_path: &Path, file_entry: &serde_json::Value) {
    let filename = dest_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let stem = filename.trim_end_matches(".jar").trim_end_matches(".zip");
    let sidecar = serde_json::json!({
        "provider": "modrinth",
        "project_id": file_entry["project_id"].as_str(),
        "version_id": file_entry["version_id"].as_str().or_else(|| {
            file_entry["versions"].as_array().and_then(|v| v.first()).and_then(|x| x.as_str())
        }),
        "version_number": null,
        "filename": filename,
        "downloaded_from_mrpack": true,
    });
    if let Some(parent) = dest_path.parent() {
        let sidecar_name = format!("{}.voidlauncher.json", stem);
        let _ = std::fs::write(parent.join(sidecar_name), sidecar.to_string());
    }
}

async fn install_mrpack_file(
    path_str: &str,
    file_entry: &serde_json::Value,
    mc_dir: &Path,
    embedded: &HashMap<String, Vec<u8>>,
) -> Result<()> {
    if path_str.is_empty() {
        return Err(LauncherError::Instance("Missing path in mrpack file entry".into()));
    }

    let dest_path = mc_dir.join(path_str);
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let expected_sha1 = file_entry["hashes"]["sha1"].as_str().unwrap_or("");
    let norm_path = normalize_zip_path(path_str);

    if let Some(data) = embedded.get(&norm_path) {
        std::fs::write(&dest_path, data)?;
        if !expected_sha1.is_empty() {
            let mut hasher = sha1::Sha1::new();
            use sha1::Digest;
            hasher.update(data);
            let hash = format!("{:x}", hasher.finalize());
            if hash != expected_sha1 {
                return Err(LauncherError::Instance(format!(
                    "Embedded file hash mismatch for {}",
                    path_str
                )));
            }
        }
        write_mrpack_sidecar(&dest_path, file_entry);
        return Ok(());
    }

    if dest_path.exists() && !expected_sha1.is_empty() {
        let path_buf = dest_path.clone();
        if crate::download::verify_sha1(&path_buf, expected_sha1).unwrap_or(false) {
            write_mrpack_sidecar(&dest_path, file_entry);
            return Ok(());
        }
    }

    let urls = resolve_mrpack_urls(file_entry, path_str).await;
    if urls.is_empty() {
        return Err(LauncherError::Instance(format!(
            "No download URL for {}",
            path_str
        )));
    }

    let dest_buf = dest_path.clone();
    for url in urls {
        if crate::download::download_file(&url, &dest_buf, expected_sha1)
            .await
            .is_ok()
        {
            write_mrpack_sidecar(&dest_path, file_entry);
            return Ok(());
        }
    }

    Err(LauncherError::Instance(format!(
        "Failed to download {}",
        path_str
    )))
}

fn extract_mrpack_override(entry_name: &str) -> Option<&str> {
    for prefix in ["overrides/", "overrides-client/"] {
        if let Some(relative) = entry_name.strip_prefix(prefix) {
            return Some(relative);
        }
    }
    None
}

/// Detect modpack format from a zip file by peeking at known manifest files.
pub fn probe_modpack(path: &str) -> Result<ModpackMetadata> {
    let zip_bytes = std::fs::read(path).map_err(|e| {
        LauncherError::Instance(format!("Cannot read file: {}", e))
    })?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes))
        .map_err(|e| LauncherError::Instance(format!("Invalid archive: {}", e)))?;

    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .collect();

    // Check Prism/MultiMC
    if names.iter().any(|n| n == "instance.cfg") {
        let mut cfg = String::new();
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                if entry.name() == "instance.cfg" {
                    let _ = entry.read_to_string(&mut cfg);
                    break;
                }
            }
        }
        let name = cfg.lines()
            .find_map(|l| l.trim().strip_prefix("name="))
            .unwrap_or("imported")
            .to_string();
        return Ok(ModpackMetadata {
            format: ModpackFormat::Prism,
            name,
            mc_version: None,
            loader: None,
            loader_version: None,
            summary: None,
        });
    }

    // Check Modrinth .mrpack
    if names.iter().any(|n| n == "modrinth.index.json") {
        let mut index_str = String::new();
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                if entry.name() == "modrinth.index.json" {
                    let _ = entry.read_to_string(&mut index_str);
                    break;
                }
            }
        }
        if let Ok(index) = serde_json::from_str::<serde_json::Value>(&index_str) {
            let name = index["name"].as_str().unwrap_or("imported").to_string();
            let summary = index["summary"].as_str().map(|s| s.to_string());
            let mc_version = index["dependencies"]["minecraft"].as_str().map(|s| s.to_string());
            let loader = index["dependencies"].as_object()
                .and_then(|d| d.keys().find(|k| *k != "minecraft"))
                .cloned();
            return Ok(ModpackMetadata {
                format: ModpackFormat::Modrinth,
                name,
                mc_version,
                loader,
                loader_version: None,
                summary,
            });
        }
    }

    // Check CurseForge / FTB (manifest.json with minecraft block)
    if names.iter().any(|n| n == "manifest.json") {
        let mut manifest_str = String::new();
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                if entry.name() == "manifest.json" {
                    let _ = entry.read_to_string(&mut manifest_str);
                    break;
                }
            }
        }
        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&manifest_str) {
            let name = manifest["name"].as_str().unwrap_or("imported").to_string();
            let summary = manifest["overrides"].as_str().map(|s| s.to_string());
            let mc_version = manifest["minecraft"]["version"].as_str().map(|s| s.to_string());
            let loaders = manifest["minecraft"]["modLoaders"].as_array();
            let (loader, loader_version) = loaders
                .and_then(|arr| arr.first())
                .map(|v| {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    let (ldr, ver) = id.split_once('-').unwrap_or((&id, ""));
                    (Some(ldr.to_string()), Some(ver.to_string()))
                })
                .unwrap_or((None, None));
            return Ok(ModpackMetadata {
                format: ModpackFormat::CurseForge,
                name,
                mc_version,
                loader,
                loader_version,
                summary,
            });
        }
    }

    // Check ATLauncher (instance.json with "@library" / components)
    if names.iter().any(|n| n == "instance.json") {
        let mut inst_str = String::new();
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                if entry.name() == "instance.json" {
                    let _ = entry.read_to_string(&mut inst_str);
                    break;
                }
            }
        }
        if let Ok(inst) = serde_json::from_str::<serde_json::Value>(&inst_str) {
            let name = inst["name"].as_str().unwrap_or("imported").to_string();
            let mc = inst["minecraftVersion"].as_str()
                .or_else(|| inst["component"].as_array()
                    .and_then(|c| c.iter().find(|x| x["type"] == "minecraft"))
                    .and_then(|x| x["version"].as_str()));
            return Ok(ModpackMetadata {
                format: ModpackFormat::ATLauncher,
                name,
                mc_version: mc.map(|s| s.to_string()),
                loader: None,
                loader_version: None,
                summary: None,
            });
        }
    }

    Err(LauncherError::Instance(
        "Unrecognized modpack format. Supported: Prism/MultiMC (.zip), Modrinth (.mrpack), CurseForge/FTB (.zip), ATLauncher (.zip)".to_string()
    ))
}

/// Import a modpack into the instances directory
pub async fn import_modpack(
    instances_dir: &PathBuf,
    path: &str,
    instance_name: &str,
    curseforge_api_key: &str,
    libraries_dir: &Path,
    app: Option<&AppHandle>,
) -> Result<Instance> {
    emit_progress(app, "reading", 0, 1, "Reading archive...");

    let zip_bytes = std::fs::read(path)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes))?;

    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .collect();

    let instance = if names.iter().any(|n| n == "instance.cfg") {
        instances::import_prism_pack(instances_dir, path)?
    } else if names.iter().any(|n| n == "modrinth.index.json") {
        import_mrpack(instances_dir, path, instance_name, app).await?
    } else if names.iter().any(|n| n == "manifest.json") {
        import_curseforge_pack(instances_dir, path, instance_name, curseforge_api_key, libraries_dir, app).await?
    } else if names.iter().any(|n| n == "instance.json") {
        import_atlauncher_pack(instances_dir, path, instance_name)?
    } else {
        return Err(LauncherError::Instance("Unrecognized modpack format".to_string()));
    };

    emit_progress(app, "done", 1, 1, "Import complete!");
    Ok(instance)
}

/// Import a Modrinth .mrpack
async fn import_mrpack(
    instances_dir: &PathBuf,
    path: &str,
    instance_name: &str,
    app: Option<&AppHandle>,
) -> Result<Instance> {
    emit_progress(app, "extracting", 0, 1, "Extracting overrides...");

    let zip_bytes = std::fs::read(path)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes))?;

    // Read index
    let mut index_str = String::new();
    let mut found = false;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.name() == "modrinth.index.json" {
            entry.read_to_string(&mut index_str).ok();
            found = true;
            break;
        }
    }
    if !found {
        return Err(LauncherError::Instance("Missing modrinth.index.json".to_string()));
    }
    let index: serde_json::Value = serde_json::from_str(&index_str)?;
    let mc_version = index["dependencies"]["minecraft"].as_str()
        .ok_or_else(|| LauncherError::Instance("Missing minecraft dependency in modrinth.index.json".to_string()))?
        .to_string();

    let target_dir = instances_dir.join(instance_name);
    let mc_dir = target_dir.join(".minecraft");
    std::fs::create_dir_all(&mc_dir)?;
    std::fs::create_dir_all(mc_dir.join("mods"))?;
    std::fs::create_dir_all(mc_dir.join("resourcepacks"))?;
    std::fs::create_dir_all(mc_dir.join("shaderpacks"))?;
    std::fs::create_dir_all(mc_dir.join("config"))?;

    // Extract overrides/ + overrides-client/; collect embedded pack files
    let mut extracted_any = false;
    let mut embedded: HashMap<String, Vec<u8>> = HashMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| LauncherError::Instance(e.to_string()))?;
        let entry_name = entry.name().to_string();

        if entry_name == "modrinth.index.json" || entry_name.starts_with("overrides-server/") {
            continue;
        }

        if let Some(relative) = extract_mrpack_override(&entry_name) {
            if relative.is_empty() {
                continue;
            }
            check_safe_relative(relative)?;
            extracted_any = true;

            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;

            if entry.is_dir() {
                std::fs::create_dir_all(mc_dir.join(relative))?;
            } else {
                extract_entry(&mc_dir, relative, &buf)?;
            }
            continue;
        }

        if entry.is_dir() {
            continue;
        }

        check_safe_relative(&entry_name)?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        embedded.insert(normalize_zip_path(&entry_name), buf);
    }

    if !extracted_any {
        tracing::warn!(target: "launcher", "Modrinth pack has no overrides/ folder");
    }

    // Install files listed in modrinth.index.json (embedded, overrides, or download)
    let files = index["files"].as_array().cloned().unwrap_or_default();
    let client_files: Vec<_> = files
        .iter()
        .filter(|f| mrpack_client_required(f))
        .collect();
    let total = client_files.len();
    let mod_count = client_files
        .iter()
        .filter(|f| {
            f["path"]
                .as_str()
                .map(|p| p.starts_with("mods/"))
                .unwrap_or(false)
        })
        .count();

    if total > 0 {
        emit_progress(
            app,
            "downloading-mods",
            0,
            total,
            &format!("Installing {} files ({} mods)...", total, mod_count),
        );

        let semaphore = Arc::new(Semaphore::new(8));
        let completed = Arc::new(AtomicUsize::new(0));
        let succeeded = Arc::new(AtomicUsize::new(0));
        let failed_paths = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut tasks = Vec::with_capacity(total);

        for file_entry in client_files {
            let path_str = file_entry["path"].as_str().unwrap_or("").to_string();
            let sem = semaphore.clone();
            let mc_dir = mc_dir.clone();
            let embedded = embedded.clone();
            let file_entry = file_entry.clone();
            let app_owned = app.cloned();
            let completed = completed.clone();
            let succeeded = succeeded.clone();
            let failed_paths = failed_paths.clone();

            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.ok();
                let result =
                    install_mrpack_file(&path_str, &file_entry, &mc_dir, &embedded).await;
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                match result {
                    Ok(()) => {
                        succeeded.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::warn!(target: "launcher", "Mrpack file install failed ({}): {}", path_str, e);
                        if let Ok(mut failed) = failed_paths.lock() {
                            failed.push(path_str.clone());
                        }
                    }
                }
                if let Some(ref a) = app_owned {
                    let ok = succeeded.load(Ordering::Relaxed);
                    let _ = a.emit(
                        "import-progress",
                        ImportProgressPayload {
                            stage: "downloading-mods".into(),
                            current: done,
                            total,
                            message: format!("Installed {}/{} files ({} mods in pack)", ok, total, mod_count),
                        },
                    );
                }
            }));
        }

        join_all(tasks).await;

        let ok = succeeded.load(Ordering::Relaxed);
        let failed_list = failed_paths.lock().map(|f| f.clone()).unwrap_or_default();
        tracing::info!(
            target: "launcher",
            "Mrpack import: {}/{} files installed successfully",
            ok,
            total
        );

        if !failed_list.is_empty() {
            let preview: Vec<_> = failed_list.iter().take(5).cloned().collect();
            let more = if failed_list.len() > 5 {
                format!(" and {} more", failed_list.len() - 5)
            } else {
                String::new()
            };
            return Err(LauncherError::Instance(format!(
                "Failed to install {} of {} files: {}{}",
                failed_list.len(),
                total,
                preview.join(", "),
                more
            )));
        }
    }

    // Detect loader from dependencies
    let loader = index["dependencies"].as_object()
        .and_then(|d| d.keys().find(|k| *k != "minecraft"))
        .cloned();
    let loader_version = index["dependencies"].as_object()
        .and_then(|d| {
            loader.as_ref().and_then(|ldr| {
                d.get(ldr)
                    .or_else(|| d.get(&format!("{}-loader", ldr)))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
        });
    let ldr_type = match loader.as_deref() {
        Some("fabric") => LoaderType::Fabric,
        Some("forge") => LoaderType::Forge,
        Some("neoforge") => LoaderType::NeoForge,
        Some("quilt") => LoaderType::Quilt,
        _ => LoaderType::Vanilla,
    };

    let now = chrono::Utc::now().to_rfc3339();
    let instance = Instance {
        name: instance_name.to_string(),
        mc_version,
        loader: ldr_type,
        loader_version,
        loader_profile: None,
        memory_mb: None,
        jvm_args: None,
        gc_preset: None,
        java_path: None,
        resolution: None,
        icon: None,
        banner: None,
        created_at: now.clone(),
        last_played: None,
        play_time_seconds: 0,
        notes: String::new(),
    };

    instances::save_instance(instances_dir, &instance)?;
    tracing::info!(target: "launcher", "Imported Modrinth pack as '{}' ({} client files, {} mods)", instance_name, total, mod_count);
    Ok(instance)
}

/// Import a CurseForge modpack
async fn import_curseforge_pack(
    instances_dir: &PathBuf,
    path: &str,
    instance_name: &str,
    curseforge_api_key: &str,
    _libraries_dir: &Path,
    app: Option<&AppHandle>,
) -> Result<Instance> {
    emit_progress(app, "extracting", 0, 1, "Extracting overrides...");

    let zip_bytes = std::fs::read(path)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes))?;

    // Read manifest
    let mut manifest_str = String::new();
    let mut found = false;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.name() == "manifest.json" {
            entry.read_to_string(&mut manifest_str).ok();
            found = true;
            break;
        }
    }
    if !found {
        return Err(LauncherError::Instance("Missing manifest.json".to_string()));
    }
    let manifest: serde_json::Value = serde_json::from_str(&manifest_str)?;
    let mc_version = manifest["minecraft"]["version"].as_str()
        .ok_or_else(|| LauncherError::Instance("Missing minecraft version in manifest.json".to_string()))?
        .to_string();

    let (loader, loader_version) = manifest["minecraft"]["modLoaders"].as_array()
        .and_then(|arr| arr.first())
        .map(|v| {
            let id = v["id"].as_str().unwrap_or("");
            let parts: Vec<&str> = id.splitn(2, '-').collect();
            (parts.first().copied(), parts.get(1).copied())
        })
        .unwrap_or((None, None));

    let target_dir = instances_dir.join(instance_name);
    let mc_dir = target_dir.join(".minecraft");
    let mods_dir = mc_dir.join("mods");
    let rp_dir = mc_dir.join("resourcepacks");
    let sp_dir = mc_dir.join("shaderpacks");
    let config_dir = mc_dir.join("config");
    std::fs::create_dir_all(&mods_dir)?;
    std::fs::create_dir_all(&rp_dir)?;
    std::fs::create_dir_all(&sp_dir)?;
    std::fs::create_dir_all(&config_dir)?;

    // Extract overrides/ with path traversal protection
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| LauncherError::Instance(e.to_string()))?;
        let entry_name = entry.name().to_string();

        if entry_name == "manifest.json" { continue; }

        if let Some(relative) = entry_name.strip_prefix("overrides/") {
            if relative.is_empty() { continue; }
            check_safe_relative(relative)?;

            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;

            if entry.is_dir() {
                std::fs::create_dir_all(mc_dir.join(relative))?;
            } else {
                extract_entry(&mc_dir, relative, &buf)?;
            }
        }
    }

    // Download mods listed in the files array via CurseForge API (concurrent)
    let files = manifest["files"].as_array().cloned().unwrap_or_default();
    let total = files.len();
    if total > 0 {
        let semaphore = Arc::new(Semaphore::new(4));
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let in_progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let succeeded = Arc::new(AtomicUsize::new(0));
        let failed_ids = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let mut tasks = Vec::with_capacity(total);
        let mut skipped = 0usize;

        // Initial progress
        emit_progress(app, "downloading-mods", 0, total, &format!("Preparing {} mods...", total));

        for file_entry in files {
            let project_id = file_entry["projectID"].as_u64();
            let file_id = file_entry["fileID"].as_u64();

            if let (Some(pid), Some(fid)) = (project_id, file_id) {
                let sem = semaphore.clone();
                let mods_dir = mods_dir.clone();
                let api_key = curseforge_api_key.to_string();
                let app_owned = app.cloned();
                let completed = completed.clone();
                let in_progress = in_progress.clone();
                let succeeded = succeeded.clone();
                let failed_ids = failed_ids.clone();

                tasks.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await.map_err(|e| format!("Semaphore error: {}", e))?;
                    in_progress.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                    // Emit in-progress batch update
                    if let Some(ref a) = app_owned {
                        let done = completed.load(std::sync::atomic::Ordering::Relaxed);
                        let inflight = in_progress.load(std::sync::atomic::Ordering::Relaxed);
                        let _ = a.emit("import-progress", ImportProgressPayload {
                            stage: "downloading-mods".into(),
                            current: done,
                            total,
                            message: format!("Downloading {} mods ({} active)...", total, inflight),
                        });
                    }

                    let mut last_name = String::new();
                    let result: std::result::Result<(), String> = async {
                        let cf_file = crate::curseforge::get_mod_file(pid, fid, &api_key).await
                            .map_err(|e| format!("CF API error: {}", e))?;
                        last_name = cf_file.display_name.clone();

                        let dest_path = mods_dir.join(&cf_file.file_name);
                        crate::curseforge::download_mod_file(pid, fid, &cf_file, &api_key, &dest_path)
                            .await
                            .map_err(|e| e.to_string())?;

                        // Sidecar
                        let sidecar = serde_json::json!({
                            "provider": "curseforge",
                            "project_id": pid.to_string(),
                            "project_name": cf_file.display_name,
                            "version_id": null,
                            "version_number": null,
                        });
                        let sidecar_name = format!("{}.voidlauncher.json",
                            cf_file.file_name.trim_end_matches(".jar"));
                        let _ = std::fs::write(mods_dir.join(sidecar_name), sidecar.to_string());

                        Ok(())
                    }.await;

                    in_progress.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

                    match &result {
                        Ok(()) => {
                            succeeded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        Err(e) => {
                            tracing::warn!(target: "launcher", "CurseForge mod download failed ({}:{}): {}", pid, fid, e);
                            if let Ok(mut failed) = failed_ids.lock() {
                                failed.push((last_name.clone(), format!("{}:{}", pid, fid)));
                            }
                        }
                    }

                    // Emit completion progress
                    if let Some(ref a) = app_owned {
                        let inflight = in_progress.load(std::sync::atomic::Ordering::Relaxed);
                        let ok = succeeded.load(std::sync::atomic::Ordering::Relaxed);
                        let msg = if result.is_ok() {
                            format!("Downloaded: {} ({}/{} installed, {} active)", last_name, ok, total, inflight)
                        } else {
                            format!("Failed: {} ({}/{} installed, {} active)", last_name, ok, total, inflight)
                        };
                        let _ = a.emit("import-progress", ImportProgressPayload {
                            stage: "downloading-mods".into(),
                            current: done,
                            total,
                            message: msg,
                        });
                    }

                    Ok::<_, String>(())
                }));
            } else {
                skipped += 1;
                tracing::warn!(target: "launcher", "Skipping CurseForge file entry: missing projectID or fileID");
            }
        }

        // Wait for all downloads to complete
        join_all(tasks).await;
        let download_failed = failed_ids.lock().map(|f| f.len()).unwrap_or(0);
        let ok = succeeded.load(std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            target: "launcher",
            "CurseForge mod download: {} succeeded, {} failed, {} skipped (no IDs)",
            ok,
            download_failed,
            skipped
        );

        if download_failed > 0 || skipped > 0 {
            let failed_list = failed_ids.lock().map(|f| f.clone()).unwrap_or_default();
            let preview: Vec<String> = failed_list
                .iter()
                .take(5)
                .map(|(name, id)| format!("{} ({})", name, id))
                .collect();
            let more = if failed_list.len() > 5 {
                format!(" and {} more", failed_list.len() - 5)
            } else {
                String::new()
            };
            let mut msg = format!(
                "CurseForge import incomplete: {} installed, {} failed to download",
                ok,
                download_failed,
            );
            if skipped > 0 {
                msg.push_str(&format!(", {} skipped (missing IDs in manifest)", skipped));
            }
            if !preview.is_empty() {
                msg.push_str(&format!(". Failed: {}{}", preview.join(", "), more));
            }
            return Err(LauncherError::Instance(msg));
        }
    }

    let ldr_type = match loader {
        Some("fabric") => LoaderType::Fabric,
        Some("forge") => LoaderType::Forge,
        Some("neoforge") => LoaderType::NeoForge,
        Some("quilt") => LoaderType::Quilt,
        _ => LoaderType::Vanilla,
    };

    let now = chrono::Utc::now().to_rfc3339();
    let instance = Instance {
        name: instance_name.to_string(),
        mc_version: mc_version.clone(),
        loader: ldr_type,
        loader_version: loader_version.map(|s| s.to_string()),
        loader_profile: None,
        memory_mb: None,
        jvm_args: None,
        gc_preset: None,
        java_path: None,
        resolution: None,
        icon: None,
        banner: None,
        created_at: now.clone(),
        last_played: None,
        play_time_seconds: 0,
        notes: String::new(),
    };

    instances::save_instance(instances_dir, &instance)?;

    tracing::info!(target: "launcher", "Imported CurseForge pack as '{}' ({} mods) - loader install pending", instance_name, total);
    Ok(instance)
}

/// Import an ATLauncher instance
fn import_atlauncher_pack(instances_dir: &PathBuf, path: &str, instance_name: &str) -> Result<Instance> {
    let zip_bytes = std::fs::read(path)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes))?;

    // Read instance.json for metadata
    let mut inst_str = String::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.name() == "instance.json" {
            entry.read_to_string(&mut inst_str).ok();
            break;
        }
    }

    let mc_version;
    if let Ok(inst) = serde_json::from_str::<serde_json::Value>(&inst_str) {
        mc_version = inst["minecraftVersion"].as_str()
            .or_else(|| inst["component"].as_array()
                .and_then(|c| c.iter().find(|x| x["type"] == "minecraft"))
                .and_then(|x| x["version"].as_str()))
            .unwrap_or("1.20.1")
            .to_string();
    } else {
        mc_version = "1.20.1".to_string();
    }

    let target_dir = instances_dir.join(instance_name);
    let mc_dir = target_dir.join(".minecraft");
    std::fs::create_dir_all(&mc_dir)?;

    // Extract all files with path traversal protection
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| LauncherError::Instance(e.to_string()))?;
        let entry_name = entry.name().to_string();
        if entry_name == "instance.json" || entry.is_dir() { continue; }
        check_safe_relative(&entry_name)?;

        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        extract_entry(&mc_dir, &entry_name, &buf)?;
    }

    let now = chrono::Utc::now().to_rfc3339();
    let instance = Instance {
        name: instance_name.to_string(),
        mc_version,
        loader: LoaderType::Vanilla,
        loader_version: None,
        loader_profile: None,
        memory_mb: None,
        jvm_args: None,
        gc_preset: None,
        java_path: None,
        resolution: None,
        icon: None,
        banner: None,
        created_at: now.clone(),
        last_played: None,
        play_time_seconds: 0,
        notes: String::new(),
    };

    instances::save_instance(instances_dir, &instance)?;
    tracing::info!(target: "launcher", "Imported ATLauncher pack as '{}'", instance_name);
    Ok(instance)
}

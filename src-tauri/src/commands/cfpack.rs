//! CurseForge pack lifecycle (stage 4): lock probe, update check,
//! transactional update and manifest repair for CF modpack instances.
//!
//! Update safety model: everything new is staged first (downloaded beside
//! the instance, never in it), then applied with park-aside renames — the
//! old file is only parked, and every park is discarded after the new lock
//! is committed. Any failure before the commit restores the parks and the
//! sidecars, so a failed update never deletes the old file nor leaves the
//! instance broken.

use crate::cfpack::{self, CfPackLock, PackDiff, PackFileRef};
use crate::commands::instances::validate_instance_name;
use crate::content::{self, SidecarWrite};
use crate::curseforge::{self, CfFile};
use crate::import::{check_safe_relative, write_extracted, write_override_sidecar, ImportProgressPayload};
use crate::instances;
use crate::AppState;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, State};

// ==================== Shared helpers ====================

struct PackCtx {
    instance: crate::instances::Instance,
    instance_dir: PathBuf,
    mc_dir: PathBuf,
    mods_dir: PathBuf,
    api_key: String,
}

fn pack_ctx(state: &AppState, instance_name: &str) -> Result<PackCtx, String> {
    validate_instance_name(instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance =
        instances::get_instance(&config.instances_dir(), instance_name).map_err(|e| e.to_string())?;
    let instance_dir = instance.dir(&config.instances_dir());
    let mc_dir = instance.minecraft_dir(&config.instances_dir());
    Ok(PackCtx {
        mods_dir: mc_dir.join("mods"),
        instance,
        instance_dir,
        mc_dir,
        api_key: config.curseforge_api_key.clone(),
    })
}

fn emit(app: &AppHandle, stage: &str, current: usize, total: usize, message: &str) {
    let _ = app.emit(
        "import-progress",
        ImportProgressPayload {
            stage: stage.to_string(),
            current,
            total,
            message: message.to_string(),
        },
    );
}

/// Newest available file for a pack under its own MC/loader constraints
/// (the CF listing already comes newest-first with unavailable files gone).
async fn latest_pack_file(lock: &CfPackLock, api_key: &str) -> Result<CfFile, String> {
    let pid: u64 = lock
        .pack_project_id
        .parse()
        .map_err(|_| "Invalid CurseForge pack id".to_string())?;
    let mut files = curseforge::get_mod_files(
        pid,
        Some(&lock.mc_version),
        lock.loader_filter(),
        0,
        api_key,
    )
    .await
    .map_err(|e| e.to_string())?
    .data;
    if files.is_empty() {
        return Err("CurseForge reports no files for this pack.".to_string());
    }
    files.sort_by(|a, b| b.file_date.cmp(&a.file_date));
    let pick = files
        .iter()
        .find(|f| f.release_type == 1)
        .or_else(|| files.first())
        .cloned()
        .ok_or_else(|| "CurseForge reports no files for this pack.".to_string())?;
    Ok(pick)
}

fn parse_id(raw: &str, what: &str) -> Result<u64, String> {
    raw.parse::<u64>()
        .map_err(|_| format!("Invalid CurseForge {} id", what))
}

// ==================== Sidecar snapshots (rollback) ====================

/// Content of both sidecar layouts for one filename, captured before the
/// apply phase mutates anything, so a rollback can put them back exactly.
struct SidecarSnapshot {
    dir: PathBuf,
    filename: String,
    meta: Option<String>,
    legacy: Option<String>,
}

fn take_sidecar_snapshot(dir: &Path, filename: &str) -> SidecarSnapshot {
    SidecarSnapshot {
        dir: dir.to_path_buf(),
        filename: filename.to_string(),
        meta: std::fs::read_to_string(instances::sidecar_meta_path(dir, filename)).ok(),
        legacy: std::fs::read_to_string(instances::legacy_sidecar_path(dir, filename)).ok(),
    }
}

fn restore_sidecar(snap: &SidecarSnapshot) {
    let meta_path = instances::sidecar_meta_path(&snap.dir, &snap.filename);
    match &snap.meta {
        Some(text) => {
            let _ = std::fs::write(&meta_path, text);
        }
        None => {
            let _ = std::fs::remove_file(meta_path);
        }
    }
    let legacy_path = instances::legacy_sidecar_path(&snap.dir, &snap.filename);
    match &snap.legacy {
        Some(text) => {
            let _ = std::fs::write(&legacy_path, text);
        }
        None => {
            let _ = std::fs::remove_file(legacy_path);
        }
    }
}

#[derive(Default)]
struct ApplyRollback {
    /// Files the apply phase moved into the instance (deleted first so the
    /// parked originals can go back to the very same paths).
    moved: Vec<PathBuf>,
    /// Parked originals: (original path, parked path).
    stashes: Vec<(PathBuf, PathBuf)>,
    sidecars: Vec<SidecarSnapshot>,
}

impl ApplyRollback {
    fn snapshot_sidecar(&mut self, dir: &Path, filename: &str) {
        if !self
            .sidecars
            .iter()
            .any(|s| s.dir == dir && s.filename == filename)
        {
            self.sidecars.push(take_sidecar_snapshot(dir, filename));
        }
    }

    fn rollback(&self, staging: &Path, temp_zip: &Path) {
        for dest in &self.moved {
            let _ = std::fs::remove_file(dest);
        }
        instances::restore_stashed_files(&self.stashes);
        for snap in &self.sidecars {
            restore_sidecar(snap);
        }
        let _ = std::fs::remove_dir_all(staging);
        let _ = std::fs::remove_file(temp_zip);
    }
}

fn cleanup_files(staging: &Path, temp_zip: &Path) {
    let _ = std::fs::remove_dir_all(staging);
    let _ = std::fs::remove_file(temp_zip);
}

// ==================== Commands ====================

/// Lock probe for the instance context menu: `Some(lock)` means the CF
/// pack menu (update / repair) should be offered for this instance.
#[tauri::command]
pub async fn cmd_get_curseforge_pack_lock(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Option<CfPackLock>, String> {
    let ctx = pack_ctx(&state, &instance_name)?;
    Ok(CfPackLock::read(&ctx.instance_dir))
}

#[derive(Debug, serde::Serialize)]
pub struct CfPackUpdateInfo {
    pub up_to_date: bool,
    pub current_file_id: String,
    pub current_version: String,
    pub latest_file_id: Option<String>,
    pub latest_version: Option<String>,
    pub latest_file_name: Option<String>,
}

/// Compare the lock's file id against the newest pack release.
#[tauri::command]
pub async fn cmd_check_curseforge_modpack_update(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<CfPackUpdateInfo, String> {
    let ctx = pack_ctx(&state, &instance_name)?;
    let lock = CfPackLock::read(&ctx.instance_dir)
        .ok_or_else(|| "This instance is not a CurseForge pack.".to_string())?;
    let latest = latest_pack_file(&lock, &ctx.api_key).await?;
    Ok(CfPackUpdateInfo {
        up_to_date: latest.id.to_string() == lock.file_id,
        current_file_id: lock.file_id.clone(),
        current_version: lock.file_display_name.clone(),
        latest_file_id: Some(latest.id.to_string()),
        latest_version: Some(latest.display_name.clone()),
        latest_file_name: Some(latest.file_name.clone()),
    })
}

#[derive(Debug, serde::Serialize)]
pub struct CfPackUpdateReport {
    pub diff: PackDiff,
    pub mods_installed: usize,
    pub mods_removed: usize,
    pub overrides_written: usize,
    pub overrides_removed: usize,
    pub new_version: String,
}

/// Transactional pack update: download the new pack zip + every changed mod
/// into a staging folder first, then apply with park-aside renames, commit
/// the new lock and only then discard the parks. Any failure rolls back to
/// the previously installed state.
#[tauri::command]
pub async fn cmd_update_curseforge_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<CfPackUpdateReport, String> {
    let ctx = pack_ctx(&state, &instance_name)?;
    let lock = CfPackLock::read(&ctx.instance_dir)
        .ok_or_else(|| "This instance is not a CurseForge pack.".to_string())?;
    let pack_pid = parse_id(&lock.pack_project_id, "pack")?;

    emit(&app, "updating-pack", 0, 1, "Checking pack version...");
    let latest = latest_pack_file(&lock, &ctx.api_key).await?;
    if latest.id.to_string() == lock.file_id {
        return Err("The pack is already up to date.".to_string());
    }

    emit(&app, "downloading-pack", 0, 1, "Downloading new pack version...");
    let file = curseforge::get_mod_file(pack_pid, latest.id, &ctx.api_key)
        .await
        .map_err(|e| format!("CurseForge file info failed: {}", e))?;
    let safe_instance: String = instance_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let temp_zip = std::env::temp_dir()
        .join(format!("voidlauncher-cfpack-{}-{}.zip", safe_instance, latest.id));
    if let Err(e) =
        curseforge::download_mod_file(pack_pid, latest.id, &file, &ctx.api_key, &temp_zip).await
    {
        let _ = std::fs::remove_file(&temp_zip);
        return Err(format!("Pack download failed: {}", e));
    }

    let staging = ctx.instance_dir.join(".cf-update");
    let (new_manifest, new_overrides) = match cfpack::pack_zip_info(&temp_zip) {
        Ok(v) => v,
        Err(e) => {
            cleanup_files(&staging, &temp_zip);
            return Err(e);
        }
    };

    // The pack's target Minecraft/loader must match this instance — a bump
    // means a new instance, not an in-place migration.
    let new_mc = new_manifest["minecraft"]["version"].as_str().unwrap_or("");
    if new_mc != ctx.instance.mc_version {
        cleanup_files(&staging, &temp_zip);
        return Err(format!(
            "The new pack version targets Minecraft {} (this instance runs {}). Install it as a new instance.",
            new_mc, ctx.instance.mc_version
        ));
    }
    let new_loader = cfpack::manifest_loader_slug(&new_manifest);
    let inst_loader = cfpack::loader_slug(&ctx.instance.loader);
    if new_loader != inst_loader {
        cleanup_files(&staging, &temp_zip);
        return Err(format!(
            "The new pack version requires loader '{}' (this instance uses '{}'). Install it as a new instance.",
            new_loader, inst_loader
        ));
    }

    let diff = cfpack::diff_manifests(&lock.manifest, &new_manifest);
    let overrides_changed = new_overrides != lock.overrides;
    if diff.is_empty() && !overrides_changed {
        cleanup_files(&staging, &temp_zip);
        return Err("The pack is already up to date.".to_string());
    }

    let _ = std::fs::remove_dir_all(&staging);
    let stage_mods = staging.join("mods");
    let stage_ov = staging.join("overrides");
    if let Err(e) = std::fs::create_dir_all(&stage_mods) {
        cleanup_files(&staging, &temp_zip);
        return Err(format!("Cannot create staging folder: {}", e));
    }

    // ---- Phase 1: stage every changed mod beside the instance ----
    let mut staged_mods: Vec<(PackFileRef, CfFile)> = Vec::new();
    let targets: Vec<PackFileRef> = diff.added.iter().chain(diff.updated.iter()).cloned().collect();
    let total = targets.len();
    for (i, r) in targets.iter().enumerate() {
        emit(
            &app,
            "downloading-mods",
            i,
            total,
            &format!("Downloading mod {}/{}...", i + 1, total),
        );
        if i > 0 {
            // Undocumented CF rate limit — pace the metadata calls.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let stage_result: Result<(), String> = async {
            let pid = parse_id(&r.project_id, "project")?;
            let fid = parse_id(&r.file_id, "file")?;
            let cf_file = curseforge::get_mod_file(pid, fid, &ctx.api_key)
                .await
                .map_err(|e| format!("CurseForge info for {} failed: {}", r.project_id, e))?;
            let dest = stage_mods.join(&cf_file.file_name);
            curseforge::download_mod_file(pid, fid, &cf_file, &ctx.api_key, &dest)
                .await
                .map_err(|e| {
                    format!("Download of {} failed: {}", cf_file.file_name, e)
                })?;
            staged_mods.push((r.clone(), cf_file));
            Ok(())
        }
        .await;
        if let Err(e) = stage_result {
            cleanup_files(&staging, &temp_zip);
            return Err(format!(
                "Staging failed, instance untouched: {}",
                e
            ));
        }
    }

    // ---- Phase 2: stage all new overrides (unprotected only) ----
    let stage_result: Result<(), String> = (|| {
        let file = std::fs::File::open(&temp_zip)
            .map_err(|e| format!("Cannot reopen pack archive: {}", e))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("Cannot read pack archive: {}", e))?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("Cannot read pack entry: {}", e))?;
            let name = entry.name().to_string();
            let Some(rel) = name.strip_prefix("overrides/") else {
                continue;
            };
            if rel.is_empty() || entry.is_dir() {
                continue;
            }
            if !new_overrides.iter().any(|o| o == rel) {
                continue;
            }
            if cfpack::is_protected_path(rel) {
                continue;
            }
            check_safe_relative(rel).map_err(|e| e.to_string())?;
            write_extracted(&stage_ov, rel, &mut entry).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(e) = stage_result {
        cleanup_files(&staging, &temp_zip);
        return Err(format!("Staging overrides failed, instance untouched: {}", e));
    }

    // ---- Phase 3: apply (sync — all network work is done) ----
    emit(&app, "applying", 0, 1, "Applying update...");
    let installed = content::list_installed(&ctx.mods_dir, "mod");
    let mut rb = ApplyRollback::default();
    let mut mods_installed = 0usize;
    let mut mods_removed = 0usize;
    let mut overrides_written = 0usize;
    let mut overrides_removed = 0usize;

    let apply: Result<(), String> = (|| {
        // Manifest mods: park the old file (matched by sidecar project id),
        // move the staged replacement in, rewrite the sidecar.
        for (r, cf_file) in &staged_mods {
            let old = installed.iter().find(|i| {
                i.provider == "curseforge"
                    && i.project_id.as_deref() == Some(r.project_id.as_str())
            });
            rb.snapshot_sidecar(&ctx.mods_dir, &cf_file.file_name);
            if let Some(old) = old {
                rb.snapshot_sidecar(&ctx.mods_dir, &old.filename);
                let stashed =
                    instances::stash_old_content_files(&ctx.mods_dir, &old.filename)
                        .map_err(|e| e.to_string())?;
                rb.stashes.extend(stashed);
            }
            let staged_path = stage_mods.join(&cf_file.file_name);
            let dest = ctx.mods_dir.join(&cf_file.file_name);
            if let Err(e) = std::fs::rename(&staged_path, &dest) {
                return Err(format!("Cannot install {}: {}", cf_file.file_name, e));
            }
            rb.moved.push(dest);
            let hash = cf_file.sha1_hash().map(|s| s.to_string());
            let pid = r.project_id.clone();
            let fid = r.file_id.clone();
            let display = cf_file.display_name.clone();
            if let Err(e) = content::write_sidecar(
                &ctx.mods_dir,
                &cf_file.file_name,
                &SidecarWrite {
                    provider: "curseforge",
                    content_type: "mod",
                    project_id: Some(&pid),
                    project_name: Some(&display),
                    version_id: None,
                    version_number: Some(&display),
                    file_id: Some(&fid),
                    hash: hash.as_deref(),
                },
            ) {
                return Err(format!("Cannot write metadata for {}: {}", cf_file.file_name, e));
            }
            if let Some(old) = old {
                instances::cleanup_old_sidecar_after_update(
                    &ctx.mods_dir,
                    &old.filename,
                    &cf_file.file_name,
                );
            }
            mods_installed += 1;
        }

        // Removed mods: park by exact sidecar identity (pid + old fid).
        // Parks are discarded only at commit — a later failure restores them.
        for r in &diff.removed {
            let old = installed.iter().find(|i| {
                i.provider == "curseforge"
                    && i.project_id.as_deref() == Some(r.project_id.as_str())
                    && i.file_id.as_deref() == Some(r.file_id.as_str())
            });
            let Some(old) = old else { continue };
            rb.snapshot_sidecar(&ctx.mods_dir, &old.filename);
            let stashed =
                instances::stash_old_content_files(&ctx.mods_dir, &old.filename)
                    .map_err(|e| e.to_string())?;
            rb.stashes.extend(stashed);
            mods_removed += 1;
        }

        // New/changed overrides: park whatever occupies the path, move the
        // staged file in (pack-owned config is intentionally replaced).
        for rel in &new_overrides {
            if cfpack::is_protected_path(rel) {
                continue;
            }
            let staged_path = stage_ov.join(rel);
            if !staged_path.is_file() {
                continue;
            }
            let dest = ctx.mc_dir.join(rel);
            let Some(parent) = dest.parent() else { continue };
            if let Some(name) = dest.file_name().and_then(|n| n.to_str()) {
                rb.snapshot_sidecar(parent, name);
                if dest.exists() {
                    let stashed = instances::stash_old_content_files(parent, name)
                        .map_err(|e| e.to_string())?;
                    rb.stashes.extend(stashed);
                }
            }
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Err(format!("Cannot create folder for {}: {}", rel, e));
            }
            if let Err(e) = std::fs::rename(&staged_path, &dest) {
                return Err(format!("Cannot write override {}: {}", rel, e));
            }
            rb.moved.push(dest);
            write_override_sidecar(&ctx.mc_dir, rel, "curseforge");
            overrides_written += 1;
        }

        // Overrides the new pack dropped: delete only pack-managed content
        // (sidecar-backed), never user configs, never protected paths, and
        // never a file the manifest just staged above.
        for rel in &lock.overrides {
            if new_overrides.iter().any(|o| o == rel) {
                continue;
            }
            if cfpack::is_protected_path(rel) {
                continue;
            }
            let dest = ctx.mc_dir.join(rel);
            if rb.moved.contains(&dest) {
                continue;
            }
            if !cfpack::override_has_sidecar(&ctx.mc_dir, rel) {
                continue;
            }
            if !dest.exists() {
                continue;
            }
            let Some(parent) = dest.parent() else { continue };
            let Some(name) = dest.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            rb.snapshot_sidecar(parent, name);
            let stashed = instances::stash_old_content_files(parent, name)
                .map_err(|e| e.to_string())?;
            rb.stashes.extend(stashed);
            overrides_removed += 1;
        }
        Ok(())
    })();

    if let Err(e) = apply {
        rb.rollback(&staging, &temp_zip);
        return Err(format!("Update failed and was rolled back: {}", e));
    }

    // ---- Phase 4: commit the lock, then discard the parks ----
    let new_lock = CfPackLock {
        provider: "curseforge".to_string(),
        pack_project_id: lock.pack_project_id.clone(),
        pack_name: lock.pack_name.clone(),
        file_id: latest.id.to_string(),
        file_name: latest.file_name.clone(),
        file_display_name: latest.display_name.clone(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        mc_version: new_mc.to_string(),
        loader: new_loader.clone(),
        manifest: new_manifest,
        overrides: new_overrides,
    };
    if let Err(e) = new_lock.write(&ctx.instance_dir) {
        rb.rollback(&staging, &temp_zip);
        return Err(format!("Cannot save pack metadata, update rolled back: {}", e));
    }

    instances::discard_stashed_files(&rb.stashes);
    cleanup_files(&staging, &temp_zip);
    emit(&app, "done", 1, 1, "Pack updated.");

    Ok(CfPackUpdateReport {
        diff,
        mods_installed,
        mods_removed,
        overrides_written,
        overrides_removed,
        new_version: latest.display_name.clone(),
    })
}

#[derive(Debug, serde::Serialize)]
pub struct CfPackRepairReport {
    pub checked: usize,
    pub restored: usize,
    pub skipped: usize,
    pub failed: Vec<String>,
}

/// Re-install anything the manifest lists but the instance is missing
/// (mods by sidecar identity, overrides only when the file is gone). Never
/// deletes, never touches existing files, never rewrites the lock.
#[tauri::command]
pub async fn cmd_repair_curseforge_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<CfPackRepairReport, String> {
    let ctx = pack_ctx(&state, &instance_name)?;
    let lock = CfPackLock::read(&ctx.instance_dir)
        .ok_or_else(|| "This instance is not a CurseForge pack.".to_string())?;

    let installed = content::list_installed(&ctx.mods_dir, "mod");
    let manifest_files = cfpack::manifest_files(&lock.manifest);
    let total = manifest_files.len();
    let mut report = CfPackRepairReport {
        checked: 0,
        restored: 0,
        skipped: 0,
        failed: Vec::new(),
    };

    // ---- Mods listed by the manifest but missing here ----
    for (i, (pid_str, fid_str)) in manifest_files.iter().enumerate() {
        report.checked += 1;
        if installed.iter().any(|ic| {
            ic.provider == "curseforge"
                && ic.project_id.as_deref() == Some(pid_str.as_str())
                && ic.file_id.as_deref() == Some(fid_str.as_str())
        }) {
            report.skipped += 1;
            continue;
        }
        emit(
            &app,
            "repairing",
            i,
            total,
            &format!("Restoring mod {}/{}...", i + 1, total),
        );
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let (Ok(pid), Ok(fid)) = (pid_str.parse::<u64>(), fid_str.parse::<u64>()) else {
            report.failed.push(format!("{}: invalid id", pid_str));
            continue;
        };
        let restored: Result<(), String> = async {
            let cf_file = curseforge::get_mod_file(pid, fid, &ctx.api_key)
                .await
                .map_err(|e| format!("CurseForge info failed: {}", e))?;
            let dest = ctx.mods_dir.join(&cf_file.file_name);
            curseforge::download_mod_file(pid, fid, &cf_file, &ctx.api_key, &dest)
                .await
                .map_err(|e| e.to_string())?;
            let display = cf_file.display_name.clone();
            let hash = cf_file.sha1_hash().map(|s| s.to_string());
            let pid_s = pid_str.clone();
            let fid_s = fid_str.clone();
            content::write_sidecar(
                &ctx.mods_dir,
                &cf_file.file_name,
                &SidecarWrite {
                    provider: "curseforge",
                    content_type: "mod",
                    project_id: Some(&pid_s),
                    project_name: Some(&display),
                    version_id: None,
                    version_number: Some(&display),
                    file_id: Some(&fid_s),
                    hash: hash.as_deref(),
                },
            )
            .map_err(|e| e.to_string())
        }
        .await;
        match restored {
            Ok(()) => report.restored += 1,
            Err(e) => report.failed.push(format!("{}: {}", cf_display_hint(pid_str), e)),
        }
    }

    // ---- Overrides whose file is gone (protected/user files stay out) ----
    let missing: Vec<String> = lock
        .overrides
        .iter()
        .filter(|rel| !cfpack::is_protected_path(rel) && !ctx.mc_dir.join(rel).exists())
        .cloned()
        .collect();
    if !missing.is_empty() {
        emit(
            &app,
            "repairing",
            total,
            total + 1,
            "Restoring pack overrides...",
        );
        let (Ok(pid), Ok(fid)) = (
            parse_id(&lock.file_id, "file"),
            parse_id(&lock.pack_project_id, "pack"),
        ) else {
            report.failed.push("Invalid pack file id".to_string());
            return Ok(report);
        };
        let (file_id, pack_id) = (fid, pid);
        let file = match curseforge::get_mod_file(pack_id, file_id, &ctx.api_key).await {
            Ok(f) => f,
            Err(e) => {
                report
                    .failed
                    .push(format!("Pack archive info failed: {}", e));
                return Ok(report);
            }
        };
        let temp_zip = std::env::temp_dir().join(format!(
            "voidlauncher-cfpack-repair-{}.zip",
            pack_id
        ));
        if let Err(e) =
            curseforge::download_mod_file(pack_id, file_id, &file, &ctx.api_key, &temp_zip).await
        {
            let _ = std::fs::remove_file(&temp_zip);
            report
                .failed
                .push(format!("Pack archive download failed: {}", e));
            return Ok(report);
        }
        let extract: Result<(), String> = (|| {
            let f = std::fs::File::open(&temp_zip)
                .map_err(|e| format!("Cannot open pack archive: {}", e))?;
            let mut archive = zip::ZipArchive::new(f)
                .map_err(|e| format!("Cannot read pack archive: {}", e))?;
            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| format!("Cannot read pack entry: {}", e))?;
                let name = entry.name().to_string();
                let Some(rel) = name.strip_prefix("overrides/") else {
                    continue;
                };
                if rel.is_empty()
                    || entry.is_dir()
                    || !missing.iter().any(|m| m == rel)
                {
                    continue;
                }
                check_safe_relative(rel).map_err(|e| e.to_string())?;
                write_extracted(&ctx.mc_dir, rel, &mut entry).map_err(|e| e.to_string())?;
                write_override_sidecar(&ctx.mc_dir, rel, "curseforge");
                report.checked += 1;
                report.restored += 1;
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&temp_zip);
        if let Err(e) = extract {
            report.failed.push(format!("Overrides restore failed: {}", e));
        }
    }

    emit(&app, "done", 1, 1, "Repair finished.");
    Ok(report)
}

/// Short label for failure lists when the CF project name is unavailable.
fn cf_display_hint(pid: &str) -> String {
    format!("project {}", pid)
}

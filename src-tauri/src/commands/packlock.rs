//! Modpack lifecycle (roadmap items 25-29): lock probe, update check,
//! transactional update and manifest repair for CurseForge and Modrinth pack
//! instances, driven by the `<instance>/pack-lock.json` lock file.
//!
//! Update safety model: everything new is staged first (downloaded beside
//! the instance, never in it), then applied with park-aside renames — the
//! old file is only parked, and every park is discarded after the new lock
//! is committed. Any failure before the commit restores the parks and the
//! sidecars, so a failed update never deletes the old file nor leaves the
//! instance broken.

use crate::commands::instances::validate_instance_name;
use crate::content::{self, SidecarWrite};
use crate::curseforge::{self, CfFile};
use crate::import::{
    check_safe_file_name, check_safe_relative, write_extracted, write_override_sidecar,
    ImportProgressPayload,
};
use crate::instances;
use crate::modrinth::{self, ModrinthVersion};
use crate::packlock::{self, PackDiff, PackFileRef, PackLock};
use crate::AppState;
use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    let instance = instances::get_instance(&config.instances_dir(), instance_name)
        .map_err(|e| e.to_string())?;
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

/// Newest available CurseForge file for a pack under its own MC/loader
/// constraints (the CF listing already comes newest-first with unavailable
/// files gone).
async fn latest_pack_file(lock: &PackLock, api_key: &str) -> Result<CfFile, String> {
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

/// Newest Modrinth version for a pack under the lock's MC/loader constraints
/// (Modrinth orders versions newest-first; sorting again makes the choice
/// independent of API ordering). Releases are preferred, betas/rcs are a
/// fallback so a pack that only ships them still updates.
async fn latest_mrpack_version(lock: &PackLock) -> Result<ModrinthVersion, String> {
    let mut versions = modrinth::get_versions(
        &lock.pack_project_id,
        Some(&lock.mc_version),
        lock.loader_filter(),
    )
    .await
    .map_err(|e| format!("Modrinth version list failed: {}", e))?;
    if versions.is_empty() {
        return Err("Modrinth reports no versions for this pack.".to_string());
    }
    versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
    let pick = versions
        .iter()
        .find(|v| v.version_type == "release")
        .or_else(|| versions.first())
        .cloned()
        .ok_or_else(|| "Modrinth reports no versions for this pack.".to_string())?;
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

    /// Undo every applied operation. Idempotent: the recorded operations are
    /// cleared, so calling it again (defensive retry) cannot delete files that
    /// the first call already restored.
    fn rollback(&mut self, staging: &Path, temp_zip: &Path) {
        for dest in self.moved.drain(..) {
            let _ = std::fs::remove_file(dest);
        }
        instances::restore_stashed_files(&self.stashes);
        self.stashes.clear();
        for snap in &self.sidecars {
            restore_sidecar(snap);
        }
        self.sidecars.clear();
        let _ = std::fs::remove_dir_all(staging);
        let _ = std::fs::remove_file(temp_zip);
    }
}

fn cleanup_files(staging: &Path, temp_zip: &Path) {
    let _ = std::fs::remove_dir_all(staging);
    let _ = std::fs::remove_file(temp_zip);
}

// ==================== Apply plan (shared by both providers) ====================

/// Owned form of [`SidecarWrite`] for a plan built before the apply phase.
struct SidecarWriteOwned {
    provider: String,
    content_type: String,
    project_id: Option<String>,
    project_name: Option<String>,
    version_id: Option<String>,
    version_number: Option<String>,
    file_id: Option<String>,
    hash: Option<String>,
}

impl SidecarWriteOwned {
    fn as_write(&self) -> SidecarWrite<'_> {
        SidecarWrite {
            provider: &self.provider,
            content_type: &self.content_type,
            project_id: self.project_id.as_deref(),
            project_name: self.project_name.as_deref(),
            version_id: self.version_id.as_deref(),
            version_number: self.version_number.as_deref(),
            file_id: self.file_id.as_deref(),
            hash: self.hash.as_deref(),
        }
    }
}

/// Sidecar to write after a file has been installed.
enum SidecarPlan {
    /// Full metadata for a file listed by the pack manifest.
    Content(SidecarWriteOwned),
    /// Minimal override metadata (only content folders get one; configs are
    /// left without a sidecar by `write_override_sidecar`).
    Override(&'static str),
}

/// One file the apply phase installs.
struct PlannedInstall {
    /// Staged file, already downloaded/extracted beside the instance.
    staged: PathBuf,
    /// Destination in the instance, without a `.disabled` suffix.
    dest: PathBuf,
    /// The replaced file was disabled: the new file must stay disabled.
    preserve_disabled: bool,
    /// Old file name whose sidecar is dropped when the stem changed.
    previous_name: Option<String>,
    sidecar: SidecarPlan,
    /// Manifest content (`true`) vs pack override (`false`), for the report.
    is_mod: bool,
}

/// One file the apply phase parks (removed pack content).
struct PlannedRemoval {
    path: PathBuf,
    /// Manifest content (`true`) vs pack override (`false`).
    is_mod: bool,
    /// `true` when this removal is counted as a mod/override removal in the
    /// report (path changes of updated files are parked but not counted).
    counted: bool,
}

/// Everything the apply phase will do, resolved before any mutation.
#[derive(Default)]
struct ApplyPlan {
    installs: Vec<PlannedInstall>,
    removals: Vec<PlannedRemoval>,
}

impl ApplyPlan {
    fn installs_at(&self, dest: &Path) -> bool {
        self.installs.iter().any(|i| i.dest == dest)
    }

    /// Drop removals that target a path this same update installs.
    ///
    /// Installs run first: the install already parks whatever occupied the
    /// path, so a later removal would park the freshly installed file and the
    /// commit would discard it. The install wins — this is also what the old
    /// "removals before overrides" order produced.
    fn drop_conflicting_removals(&mut self, mc_dir: &Path) {
        if self.removals.is_empty() {
            return;
        }
        let installs: Vec<String> = self
            .installs
            .iter()
            .map(|i| path_key(&i.dest, mc_dir))
            .collect();
        self.removals
            .retain(|removal| !installs.contains(&path_key(&removal.path, mc_dir)));
    }

    /// Keep exactly one install per canonical target, the last one.
    ///
    /// Pack manifests and pack overrides can name the same path (both formats
    /// allow it). Installing it twice would park the first replacement and
    /// overwrite that park with the second one, so a rollback could no longer
    /// restore the original file. Overrides are appended after manifest files,
    /// so the last install is the override: it wins, which matches the pack
    /// format's "overrides are applied on top" semantics.
    fn drop_shadowed_installs(&mut self, mc_dir: &Path) {
        if self.installs.len() < 2 {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        let mut kept = Vec::with_capacity(self.installs.len());
        for install in self.installs.drain(..).rev() {
            if seen.insert(path_key(&install.dest, mc_dir)) {
                kept.push(install);
            }
        }
        kept.reverse();
        self.installs = kept;
    }
}

/// Is `path` a file directly inside one of the instance's content folders?
/// The check is relative to the instance's `.minecraft` directory and compares
/// exact path components, so a parent folder that happens to be named `mods`
/// (e.g. `C:/workspace/mods/instances/...`) does not turn unrelated files into
/// content files.
fn in_content_dir(path: &Path, mc_dir: &Path) -> bool {
    let path_norm = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let root_norm = mc_dir.to_string_lossy().replace('\\', "/").to_lowercase();
    let root_norm = root_norm.trim_end_matches('/');
    let Some(rel) = path_norm.strip_prefix(root_norm) else {
        return false;
    };
    let rel = rel.trim_start_matches('/');
    let parts: Vec<&str> = rel.split('/').collect();
    parts.len() == 2 && matches!(parts[0], "mods" | "resourcepacks" | "shaderpacks")
}

/// Comparison key for an instance path: separators normalized and case-folded
/// (Windows paths are case-insensitive). Only a file directly inside a content
/// folder gets its `.disabled` suffix stripped, so `<name>` and
/// `<name>.disabled` are recognized as the same target; elsewhere the suffix is
/// just part of a name.
fn path_key(path: &Path, mc_dir: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/").to_lowercase();
    if in_content_dir(path, mc_dir) {
        text.strip_suffix(".disabled").unwrap_or(&text).to_string()
    } else {
        text
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ApplyOutcome {
    mods_installed: usize,
    mods_removed: usize,
    overrides_written: usize,
    overrides_removed: usize,
}

/// Is `parent/name` currently installed as a disabled file?
fn is_disabled_at(parent: &Path, name: &str) -> bool {
    parent.join(format!("{}.disabled", name)).is_file()
}

/// Actual on-disk path of a content file: `path` or its `.disabled` twin.
fn existing_content_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let parent = path.parent()?;
    let disabled = parent.join(format!("{}.disabled", name));
    if disabled.is_file() {
        Some(disabled)
    } else if path.is_file() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

/// Park whatever occupies `parent/name` — the plain file and/or its
/// `.disabled` twin — so a staged replacement can move in. Nothing is
/// deleted: parks are discarded only after the new lock commits, and
/// restored on rollback.
///
/// The target path is declared by the pack manifest, so the pack owns it: a
/// fresh install would overwrite that file too.
fn park_occupied(rb: &mut ApplyRollback, parent: &Path, name: &str) -> Result<(), String> {
    let disabled_name = format!("{}.disabled", name);
    let disabled_exists = parent.join(&disabled_name).is_file();
    if !disabled_exists && !parent.join(name).is_file() {
        return Ok(());
    }
    rb.snapshot_sidecar(parent, name);
    // Parking by the disabled name parks the plain twin as well (the stash
    // helper expands the stem family), so both variants leave the path.
    let target = if disabled_exists {
        disabled_name
    } else {
        name.to_string()
    };
    let stashed = instances::stash_old_content_files(parent, &target).map_err(|e| e.to_string())?;
    rb.stashes.extend(stashed);
    Ok(())
}

fn install_one(
    rb: &mut ApplyRollback,
    mc_dir: &Path,
    install: &PlannedInstall,
) -> Result<(), String> {
    let Some(parent) = install.dest.parent() else {
        return Err(format!("Invalid destination {}", install.dest.display()));
    };
    let Some(name) = install.dest.file_name().and_then(|n| n.to_str()) else {
        return Err(format!("Invalid destination {}", install.dest.display()));
    };
    park_occupied(rb, parent, name)?;
    if let Err(e) = std::fs::create_dir_all(parent) {
        return Err(format!("Cannot create folder for {}: {}", name, e));
    }
    let final_dest = if install.preserve_disabled {
        parent.join(format!("{}.disabled", name))
    } else {
        install.dest.clone()
    };
    if let Err(e) = std::fs::rename(&install.staged, &final_dest) {
        return Err(format!("Cannot install {}: {}", name, e));
    }
    rb.moved.push(final_dest);
    match &install.sidecar {
        SidecarPlan::Content(fields) => {
            if let Err(e) = content::write_sidecar(parent, name, &fields.as_write()) {
                return Err(format!("Cannot write metadata for {}: {}", name, e));
            }
        }
        SidecarPlan::Override(provider) => {
            let Ok(rel) = install.dest.strip_prefix(mc_dir) else {
                return Err(format!(
                    "Destination outside the instance: {}",
                    install.dest.display()
                ));
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            // The override replaces whatever pack file occupied this path, so
            // old content metadata (e.g. the removed mod's project id) no
            // longer describes it. The rollback snapshot restores the old
            // sidecar if the update fails.
            instances::remove_sidecar_meta(parent, name);
            write_override_sidecar(mc_dir, &rel, provider);
        }
    }
    if let Some(previous) = &install.previous_name {
        if previous != name {
            instances::cleanup_old_sidecar_after_update(parent, previous, name);
        }
    }
    Ok(())
}

fn remove_one(rb: &mut ApplyRollback, path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Ok(());
    };
    rb.snapshot_sidecar(parent, name);
    let stashed = instances::stash_old_content_files(parent, name).map_err(|e| e.to_string())?;
    rb.stashes.extend(stashed);
    Ok(())
}

/// Apply the whole plan, then commit the new lock. Any failure (apply or lock
/// write) restores the previous state; parks are discarded only after the
/// lock is on disk. This is the single commit point of an update.
fn apply_and_commit(
    mc_dir: &Path,
    instance_dir: &Path,
    plan: &ApplyPlan,
    staging: &Path,
    temp_zip: &Path,
    new_lock: &PackLock,
) -> Result<ApplyOutcome, String> {
    let mut rb = ApplyRollback::default();
    let mut outcome = ApplyOutcome::default();
    let apply: Result<(), String> = (|| {
        for install in &plan.installs {
            install_one(&mut rb, mc_dir, install)?;
            if install.is_mod {
                outcome.mods_installed += 1;
            } else {
                outcome.overrides_written += 1;
            }
        }
        for removal in &plan.removals {
            remove_one(&mut rb, &removal.path)?;
            if removal.counted {
                if removal.is_mod {
                    outcome.mods_removed += 1;
                } else {
                    outcome.overrides_removed += 1;
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = apply {
        rb.rollback(staging, temp_zip);
        return Err(format!("Update failed and was rolled back: {}", e));
    }
    if let Err(e) = new_lock.write(instance_dir) {
        rb.rollback(staging, temp_zip);
        return Err(format!(
            "Cannot save pack metadata, update was rolled back: {}",
            e
        ));
    }
    instances::discard_stashed_files(&rb.stashes);
    cleanup_files(staging, temp_zip);
    Ok(outcome)
}

/// Queue installs for every unprotected override shipped by the new pack.
fn plan_override_installs(
    plan: &mut ApplyPlan,
    staged_overrides_dir: &Path,
    mc_dir: &Path,
    new_overrides: &[String],
    provider: &'static str,
) {
    for rel in new_overrides {
        if packlock::is_protected_path(rel) {
            continue;
        }
        let staged = staged_overrides_dir.join(rel);
        if !staged.is_file() {
            continue;
        }
        let dest = mc_dir.join(rel);
        let preserve_disabled = dest
            .parent()
            .zip(dest.file_name().and_then(|n| n.to_str()))
            .map(|(parent, name)| is_disabled_at(parent, name))
            .unwrap_or(false);
        plan.installs.push(PlannedInstall {
            staged,
            dest,
            preserve_disabled,
            previous_name: None,
            sidecar: SidecarPlan::Override(provider),
            is_mod: false,
        });
    }
}

/// Queue removals for overrides the new pack no longer ships. Only pack-managed
/// (sidecar-backed) content is ever removed, and never a path the plan just
/// installed.
fn plan_override_removals(
    plan: &mut ApplyPlan,
    mc_dir: &Path,
    new_overrides: &[String],
    old_overrides: &[String],
) {
    for rel in old_overrides {
        if new_overrides.iter().any(|o| o == rel) {
            continue;
        }
        if packlock::is_protected_path(rel) {
            continue;
        }
        let dest = mc_dir.join(rel);
        if plan.installs_at(&dest) || !packlock::override_has_sidecar(mc_dir, rel) {
            continue;
        }
        let Some(existing) = existing_content_path(&dest) else {
            continue;
        };
        plan.removals.push(PlannedRemoval {
            path: existing,
            is_mod: false,
            counted: true,
        });
    }
}

struct CfApplyInput<'a> {
    installed: &'a [content::InstalledContent],
    staged_mods: &'a [(PackFileRef, curseforge::CfFile)],
    diff: &'a PackDiff,
    new_overrides: &'a [String],
    old_overrides: &'a [String],
}

/// Resolve the CurseForge apply plan. Manifest mods are matched by sidecar
/// identity; a disabled installed file makes the replacement disabled too.
fn plan_curseforge_update(
    mods_dir: &Path,
    mc_dir: &Path,
    staged_mods_dir: &Path,
    staged_overrides_dir: &Path,
    input: &CfApplyInput<'_>,
) -> ApplyPlan {
    let mut plan = ApplyPlan::default();

    for (r, cf_file) in input.staged_mods {
        let old = input.installed.iter().find(|i| {
            i.provider == "curseforge" && i.project_id.as_deref() == Some(r.project_id.as_str())
        });
        let preserve_disabled = old
            .map(|o| o.filename.ends_with(".disabled"))
            .unwrap_or(false);
        let mut previous_name = None;
        if let Some(old) = old {
            // A disabled file carries the `.disabled` suffix, so compare the
            // canonical target (case-folded, suffix stripped) instead of the
            // raw names: `sodium.jar` and `sodium.jar.disabled` are the same
            // target, and the install already parks the old file.
            let same_target = path_key(&mods_dir.join(&old.filename), mc_dir)
                == path_key(&mods_dir.join(&cf_file.file_name), mc_dir);
            if !same_target {
                // The replacement has a new file name: the old file must be
                // parked as well, or it would stay next to the new one.
                plan.removals.push(PlannedRemoval {
                    path: mods_dir.join(&old.filename),
                    is_mod: true,
                    counted: false,
                });
                previous_name = Some(old.filename.clone());
            }
        }
        plan.installs.push(PlannedInstall {
            staged: staged_mods_dir.join(&cf_file.file_name),
            dest: mods_dir.join(&cf_file.file_name),
            preserve_disabled,
            previous_name,
            sidecar: SidecarPlan::Content(SidecarWriteOwned {
                provider: "curseforge".to_string(),
                content_type: "mod".to_string(),
                project_id: Some(r.project_id.clone()),
                project_name: Some(cf_file.display_name.clone()),
                version_id: None,
                version_number: Some(cf_file.display_name.clone()),
                file_id: Some(r.file_id.clone()),
                hash: cf_file.sha1_hash().map(|s| s.to_string()),
            }),
            is_mod: true,
        });
    }

    for r in &input.diff.removed {
        let old = input.installed.iter().find(|i| {
            i.provider == "curseforge"
                && i.project_id.as_deref() == Some(r.project_id.as_str())
                && i.file_id.as_deref() == Some(r.file_id.as_str())
        });
        let Some(old) = old else { continue };
        plan.removals.push(PlannedRemoval {
            path: mods_dir.join(&old.filename),
            is_mod: true,
            counted: true,
        });
    }

    plan_override_installs(
        &mut plan,
        staged_overrides_dir,
        mc_dir,
        input.new_overrides,
        "curseforge",
    );
    plan_override_removals(&mut plan, mc_dir, input.new_overrides, input.old_overrides);
    plan.drop_shadowed_installs(mc_dir);
    plan.drop_conflicting_removals(mc_dir);
    plan
}

struct MrApplyInput<'a> {
    old_map: &'a std::collections::BTreeMap<String, serde_json::Value>,
    new_map: &'a std::collections::BTreeMap<String, serde_json::Value>,
    diff: &'a PackDiff,
    new_overrides: &'a [String],
    old_overrides: &'a [String],
}

/// Resolve the Modrinth apply plan. A path that changes is parked under its
/// old name; the disabled state of the replaced file is carried over.
fn plan_modrinth_update(mc_dir: &Path, staging: &Path, input: &MrApplyInput<'_>) -> ApplyPlan {
    let mut plan = ApplyPlan::default();

    for r in input.diff.added.iter().chain(input.diff.updated.iter()) {
        let Some(new_entry) = input.new_map.get(&r.project_id) else {
            continue;
        };
        let new_path = packlock::mrpack_entry_path(new_entry);
        if new_path.is_empty() || check_safe_relative(&new_path).is_err() {
            continue;
        }
        let dest = mc_dir.join(&new_path);
        let mut preserve_disabled = dest
            .parent()
            .zip(dest.file_name().and_then(|n| n.to_str()))
            .map(|(parent, name)| is_disabled_at(parent, name))
            .unwrap_or(false);
        let mut previous_name = None;

        if let Some(old_entry) = input.old_map.get(&r.project_id) {
            let old_path = packlock::mrpack_entry_path(old_entry);
            if !old_path.is_empty() && check_safe_relative(&old_path).is_ok() {
                let old_dest = mc_dir.join(&old_path);
                let old_disabled = old_dest
                    .parent()
                    .zip(old_dest.file_name().and_then(|n| n.to_str()))
                    .map(|(parent, name)| is_disabled_at(parent, name))
                    .unwrap_or(false);
                if old_disabled {
                    preserve_disabled = true;
                }
                if old_path != new_path && !packlock::is_protected_path(&old_path) {
                    if let Some(existing) = existing_content_path(&old_dest) {
                        if let (Some(parent), Some(name)) = (
                            existing.parent(),
                            existing.file_name().and_then(|n| n.to_str()),
                        ) {
                            if instances::read_sidecar_meta(parent, name).is_some() {
                                plan.removals.push(PlannedRemoval {
                                    path: existing,
                                    is_mod: true,
                                    counted: false,
                                });
                            }
                        }
                    }
                    previous_name = old_dest
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_string());
                }
            }
        }

        let pid = new_entry["project_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| r.project_id.clone());
        plan.installs.push(PlannedInstall {
            staged: staging.join(&new_path),
            dest,
            preserve_disabled,
            previous_name,
            sidecar: SidecarPlan::Content(SidecarWriteOwned {
                provider: "modrinth".to_string(),
                content_type: packlock::content_type_for_rel(&new_path).to_string(),
                project_id: Some(pid),
                project_name: None,
                version_id: Some(packlock::mrpack_entry_version(new_entry)),
                version_number: None,
                file_id: None,
                hash: packlock::mrpack_entry_sha1(new_entry),
            }),
            is_mod: true,
        });
    }

    for r in &input.diff.removed {
        let Some(old_entry) = input.old_map.get(&r.project_id) else {
            continue;
        };
        let old_path = packlock::mrpack_entry_path(old_entry);
        if old_path.is_empty()
            || packlock::is_protected_path(&old_path)
            || check_safe_relative(&old_path).is_err()
        {
            continue;
        }
        let Some(existing) = existing_content_path(&mc_dir.join(&old_path)) else {
            continue;
        };
        if let (Some(parent), Some(name)) = (
            existing.parent(),
            existing.file_name().and_then(|n| n.to_str()),
        ) {
            if instances::read_sidecar_meta(parent, name).is_none() {
                continue;
            }
        }
        plan.removals.push(PlannedRemoval {
            path: existing,
            is_mod: true,
            counted: true,
        });
    }

    plan_override_installs(
        &mut plan,
        &staging.join("overrides"),
        mc_dir,
        input.new_overrides,
        "modrinth",
    );
    plan_override_removals(&mut plan, mc_dir, input.new_overrides, input.old_overrides);
    plan.drop_shadowed_installs(mc_dir);
    plan.drop_conflicting_removals(mc_dir);
    plan
}

// ==================== Commands ====================

/// Lock probe for the instance context menu: `Some(lock)` means the pack
/// menu (update / repair) should be offered for this instance.
#[tauri::command]
pub async fn cmd_get_pack_lock(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Option<PackLock>, String> {
    let ctx = pack_ctx(&state, &instance_name)?;
    Ok(PackLock::read(&ctx.instance_dir))
}

#[derive(Debug, serde::Serialize)]
pub struct PackUpdateInfo {
    pub up_to_date: bool,
    pub current_file_id: String,
    pub current_version: String,
    pub latest_file_id: Option<String>,
    pub latest_version: Option<String>,
    pub latest_file_name: Option<String>,
}

/// Compare the lock's version identity against the newest pack release.
#[tauri::command]
pub async fn cmd_check_modpack_update(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<PackUpdateInfo, String> {
    let ctx = pack_ctx(&state, &instance_name)?;
    let lock = PackLock::read(&ctx.instance_dir)
        .ok_or_else(|| "This instance is not a tracked modpack.".to_string())?;
    match lock.provider.as_str() {
        "curseforge" => {
            let latest = latest_pack_file(&lock, &ctx.api_key).await?;
            Ok(PackUpdateInfo {
                up_to_date: latest.id.to_string() == lock.file_id,
                current_file_id: lock.file_id.clone(),
                current_version: lock.file_display_name.clone(),
                latest_file_id: Some(latest.id.to_string()),
                latest_version: Some(latest.display_name.clone()),
                latest_file_name: Some(latest.file_name.clone()),
            })
        }
        "modrinth" => {
            let latest = latest_mrpack_version(&lock).await?;
            let primary = latest
                .files
                .iter()
                .find(|f| f.primary)
                .or_else(|| latest.files.first());
            Ok(PackUpdateInfo {
                up_to_date: latest.id == lock.file_id,
                current_file_id: lock.file_id.clone(),
                current_version: lock.file_display_name.clone(),
                latest_file_id: Some(latest.id.clone()),
                latest_version: Some(latest.version_number.clone()),
                latest_file_name: primary.map(|f| f.filename.clone()),
            })
        }
        p => Err(format!("Unsupported pack provider: {}", p)),
    }
}

#[derive(Debug, serde::Serialize)]
pub struct PackUpdateReport {
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
pub async fn cmd_update_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<PackUpdateReport, String> {
    let _pause_scope = crate::download::PausableInstallScope::begin();
    let ctx = pack_ctx(&state, &instance_name)?;
    let lock = PackLock::read(&ctx.instance_dir)
        .ok_or_else(|| "This instance is not a tracked modpack.".to_string())?;
    match lock.provider.as_str() {
        "curseforge" => update_curseforge(&app, &ctx, lock).await,
        "modrinth" => update_modrinth(&app, &ctx, lock).await,
        p => Err(format!("Unsupported pack provider: {}", p)),
    }
}

#[derive(Debug, serde::Serialize)]
pub struct PackRepairReport {
    pub checked: usize,
    pub restored: usize,
    pub skipped: usize,
    pub failed: Vec<String>,
}

/// Re-install anything the manifest lists but the instance is missing
/// (mods by identity, overrides only when the file is gone). Never deletes,
/// never touches existing files, never rewrites the lock.
#[tauri::command]
pub async fn cmd_repair_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<PackRepairReport, String> {
    let _pause_scope = crate::download::PausableInstallScope::begin();
    let ctx = pack_ctx(&state, &instance_name)?;
    let lock = PackLock::read(&ctx.instance_dir)
        .ok_or_else(|| "This instance is not a tracked modpack.".to_string())?;
    match lock.provider.as_str() {
        "curseforge" => repair_curseforge(&app, &ctx, lock).await,
        "modrinth" => repair_modrinth(&app, &ctx, lock).await,
        p => Err(format!("Unsupported pack provider: {}", p)),
    }
}

// ==================== CurseForge ====================

/// CF pack update: staged downloads via the CF file API, then the shared
/// park-aside apply.
async fn update_curseforge(
    app: &AppHandle,
    ctx: &PackCtx,
    lock: PackLock,
) -> Result<PackUpdateReport, String> {
    let pack_pid = parse_id(&lock.pack_project_id, "pack")?;

    emit(app, "updating-pack", 0, 1, "Checking pack version...");
    let latest = latest_pack_file(&lock, &ctx.api_key).await?;
    if latest.id.to_string() == lock.file_id {
        return Err("The pack is already up to date.".to_string());
    }

    emit(
        app,
        "downloading-pack",
        0,
        1,
        "Downloading new pack version...",
    );
    let file = curseforge::get_mod_file(pack_pid, latest.id, &ctx.api_key)
        .await
        .map_err(|e| format!("CurseForge file info failed: {}", e))?;
    let safe_instance: String = ctx
        .instance
        .name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let temp_zip = std::env::temp_dir().join(format!(
        "voidlauncher-cfpack-{}-{}.zip",
        safe_instance, latest.id
    ));
    if let Err(e) =
        curseforge::download_mod_file(pack_pid, latest.id, &file, &ctx.api_key, &temp_zip).await
    {
        let _ = std::fs::remove_file(&temp_zip);
        return Err(format!("Pack download failed: {}", e));
    }

    let staging = ctx.instance_dir.join(".pack-update");
    let (new_manifest, new_overrides) = match packlock::pack_zip_info(&temp_zip) {
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
    let new_loader = packlock::manifest_loader_slug(&new_manifest);
    let inst_loader = packlock::loader_slug(&ctx.instance.loader);
    if new_loader != inst_loader {
        cleanup_files(&staging, &temp_zip);
        return Err(format!(
            "The new pack version requires loader '{}' (this instance uses '{}'). Install it as a new instance.",
            new_loader, inst_loader
        ));
    }

    let diff = packlock::diff_manifests(&lock.manifest, &new_manifest);
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
    let targets: Vec<PackFileRef> = diff
        .added
        .iter()
        .chain(diff.updated.iter())
        .cloned()
        .collect();
    let total = targets.len();
    for (i, r) in targets.iter().enumerate() {
        emit(
            app,
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
            check_safe_file_name(&cf_file.file_name).map_err(|e| e.to_string())?;
            let dest = stage_mods.join(&cf_file.file_name);
            curseforge::download_mod_file(pid, fid, &cf_file, &ctx.api_key, &dest)
                .await
                .map_err(|e| format!("Download of {} failed: {}", cf_file.file_name, e))?;
            staged_mods.push((r.clone(), cf_file));
            Ok(())
        }
        .await;
        if let Err(e) = stage_result {
            cleanup_files(&staging, &temp_zip);
            return Err(format!("Staging failed, instance untouched: {}", e));
        }
    }

    // ---- Phase 2: stage all new overrides (unprotected only) ----
    let stage_result: Result<(), String> = (|| {
        let file = std::fs::File::open(&temp_zip)
            .map_err(|e| format!("Cannot reopen pack archive: {}", e))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;
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
            if packlock::is_protected_path(rel) {
                continue;
            }
            check_safe_relative(rel).map_err(|e| e.to_string())?;
            write_extracted(&stage_ov, rel, &mut entry).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(e) = stage_result {
        cleanup_files(&staging, &temp_zip);
        return Err(format!(
            "Staging overrides failed, instance untouched: {}",
            e
        ));
    }

    // ---- Phase 3: apply (sync — all network work is done) ----
    emit(app, "applying", 0, 1, "Applying update...");
    let installed = content::list_installed(&ctx.mods_dir, "mod");
    let plan = plan_curseforge_update(
        &ctx.mods_dir,
        &ctx.mc_dir,
        &stage_mods,
        &stage_ov,
        &CfApplyInput {
            installed: &installed,
            staged_mods: &staged_mods,
            diff: &diff,
            new_overrides: &new_overrides,
            old_overrides: &lock.overrides,
        },
    );

    let new_lock = PackLock {
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
    let outcome = apply_and_commit(
        &ctx.mc_dir,
        &ctx.instance_dir,
        &plan,
        &staging,
        &temp_zip,
        &new_lock,
    )?;

    emit(app, "done", 1, 1, "Pack updated.");

    Ok(PackUpdateReport {
        diff,
        mods_installed: outcome.mods_installed,
        mods_removed: outcome.mods_removed,
        overrides_written: outcome.overrides_written,
        overrides_removed: outcome.overrides_removed,
        new_version: latest.display_name.clone(),
    })
}

/// CF manifest repair: identity checks against the sidecar index, pack zip
/// re-download for missing overrides.
async fn repair_curseforge(
    app: &AppHandle,
    ctx: &PackCtx,
    lock: PackLock,
) -> Result<PackRepairReport, String> {
    let installed = content::list_installed(&ctx.mods_dir, "mod");
    let manifest_files = packlock::manifest_files(&lock.manifest);
    let total = manifest_files.len();
    let mut report = PackRepairReport {
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
            app,
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
            check_safe_file_name(&cf_file.file_name).map_err(|e| e.to_string())?;
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
            Err(e) => report
                .failed
                .push(format!("{}: {}", cf_display_hint(pid_str), e)),
        }
    }

    // ---- Overrides whose file is gone (protected/user files stay out) ----
    let missing: Vec<String> = lock
        .overrides
        .iter()
        .filter(|rel| !packlock::is_protected_path(rel) && !ctx.mc_dir.join(rel).exists())
        .cloned()
        .collect();
    if !missing.is_empty() {
        emit(
            app,
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
        let temp_zip =
            std::env::temp_dir().join(format!("voidlauncher-cfpack-repair-{}.zip", pack_id));
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
            let mut archive =
                zip::ZipArchive::new(f).map_err(|e| format!("Cannot read pack archive: {}", e))?;
            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| format!("Cannot read pack entry: {}", e))?;
                let name = entry.name().to_string();
                let Some(rel) = name.strip_prefix("overrides/") else {
                    continue;
                };
                if rel.is_empty() || entry.is_dir() || !missing.iter().any(|m| m == rel) {
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
            report
                .failed
                .push(format!("Overrides restore failed: {}", e));
        }
    }

    emit(app, "done", 1, 1, "Repair finished.");
    Ok(report)
}

/// Short label for failure lists when the CF project name is unavailable.
fn cf_display_hint(pid: &str) -> String {
    format!("project {}", pid)
}

// ==================== Modrinth (mrpack) ====================

/// Root-level (non-override) files of an mrpack zip, keyed by normalized
/// path — the same embedded payload the importer holds in memory during a
/// fresh install. Staging reuses it so embedded files need no network.
fn read_embedded_files(zip_path: &Path) -> Result<HashMap<String, Vec<u8>>, String> {
    let file =
        std::fs::File::open(zip_path).map_err(|e| format!("Cannot open pack archive: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;
    let mut out: HashMap<String, Vec<u8>> = HashMap::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Cannot read pack entry: {}", e))?;
        let name = entry.name().to_string();
        if name == "modrinth.index.json" || name.starts_with("overrides-server/") {
            continue;
        }
        if crate::import::extract_mrpack_override(&name).is_some() {
            continue;
        }
        if entry.is_dir() {
            continue;
        }
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| format!("Cannot read pack entry {}: {}", name, e))?;
        out.insert(crate::import::normalize_zip_path(&name), buf);
    }
    Ok(out)
}

/// mrpack update: download the new pack version, diff the two
/// `modrinth.index.json` manifests, stage every changed file beside the
/// instance and apply with the same park-aside/rollback model as CF.
async fn update_modrinth(
    app: &AppHandle,
    ctx: &PackCtx,
    lock: PackLock,
) -> Result<PackUpdateReport, String> {
    emit(app, "updating-pack", 0, 1, "Checking pack version...");
    let latest = latest_mrpack_version(&lock).await?;
    if latest.id == lock.file_id {
        return Err("The pack is already up to date.".to_string());
    }
    let file = latest
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| latest.files.first())
        .ok_or_else(|| "Modrinth version has no files".to_string())?;
    if !file.url.starts_with("https://") {
        return Err("Download URL must be HTTPS.".to_string());
    }
    if !crate::is_allowed_download_host(&file.url) {
        return Err(format!(
            "Download host is not in the allowlist: {}",
            file.url
        ));
    }

    emit(
        app,
        "downloading-pack",
        0,
        1,
        "Downloading new pack version...",
    );
    let safe_instance: String = ctx
        .instance
        .name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let temp_zip = std::env::temp_dir().join(format!(
        "voidlauncher-mrpack-{}-{}.mrpack",
        safe_instance, latest.id
    ));
    let sha1 = file.hashes.get("sha1").map(|s| s.as_str()).unwrap_or("");
    if let Err(e) =
        crate::download::download_file_sized(&file.url, &temp_zip, sha1, file.size).await
    {
        let _ = std::fs::remove_file(&temp_zip);
        return Err(format!("Pack download failed: {}", e));
    }

    let staging = ctx.instance_dir.join(".pack-update");
    let (new_manifest, new_overrides) = match packlock::mrpack_zip_info(&temp_zip) {
        Ok(v) => v,
        Err(e) => {
            cleanup_files(&staging, &temp_zip);
            return Err(e);
        }
    };

    // The pack's target Minecraft/loader must match this instance — a bump
    // means a new instance, not an in-place migration (same rule as CF).
    let new_mc = new_manifest["dependencies"]["minecraft"]
        .as_str()
        .unwrap_or("");
    if new_mc != ctx.instance.mc_version {
        cleanup_files(&staging, &temp_zip);
        return Err(format!(
            "The new pack version targets Minecraft {} (this instance runs {}). Install it as a new instance.",
            new_mc, ctx.instance.mc_version
        ));
    }
    let deps = new_manifest["dependencies"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let (loader_key, _) = crate::import::detect_mrpack_loader(&deps);
    let new_loader_type =
        crate::import::mrpack_loader_type(loader_key.as_deref()).map_err(|e| e.to_string())?;
    let new_loader = packlock::loader_slug(&new_loader_type);
    let inst_loader = packlock::loader_slug(&ctx.instance.loader);
    if new_loader != inst_loader {
        cleanup_files(&staging, &temp_zip);
        return Err(format!(
            "The new pack version requires loader '{}' (this instance uses '{}'). Install it as a new instance.",
            new_loader, inst_loader
        ));
    }

    let old_map = packlock::mrpack_manifest_map(&lock.manifest);
    let new_map = packlock::mrpack_manifest_map(&new_manifest);
    let diff = packlock::diff_mrpack_manifests(&lock.manifest, &new_manifest);
    let overrides_changed = new_overrides != lock.overrides;
    if diff.is_empty() && !overrides_changed {
        cleanup_files(&staging, &temp_zip);
        return Err("The pack is already up to date.".to_string());
    }

    let _ = std::fs::remove_dir_all(&staging);
    let stage_ov = staging.join("overrides");
    if let Err(e) = std::fs::create_dir_all(&staging) {
        cleanup_files(&staging, &temp_zip);
        return Err(format!("Cannot create staging folder: {}", e));
    }

    // ---- Phase 1: stage every changed file beside the instance ----
    let embedded = match read_embedded_files(&temp_zip) {
        Ok(v) => Arc::new(v),
        Err(e) => {
            cleanup_files(&staging, &temp_zip);
            return Err(e);
        }
    };
    let staged_entries: Vec<serde_json::Value> = diff
        .added
        .iter()
        .chain(diff.updated.iter())
        .filter_map(|r| new_map.get(&r.project_id).cloned())
        .collect();
    let total = staged_entries.len();
    for (i, entry) in staged_entries.iter().enumerate() {
        emit(
            app,
            "downloading-mods",
            i,
            total,
            &format!("Downloading file {}/{}...", i + 1, total),
        );
        let path = packlock::mrpack_entry_path(entry);
        if path.is_empty() {
            continue;
        }
        if let Err(e) =
            crate::import::install_mrpack_file(&path, entry, &staging, &embedded, false).await
        {
            cleanup_files(&staging, &temp_zip);
            return Err(format!("Staging failed, instance untouched: {}", e));
        }
    }

    // ---- Phase 2: stage all new overrides (unprotected only) ----
    let stage_result: Result<(), String> = (|| {
        let file = std::fs::File::open(&temp_zip)
            .map_err(|e| format!("Cannot reopen pack archive: {}", e))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("Cannot read pack entry: {}", e))?;
            let name = entry.name().to_string();
            let Some(rel) = crate::import::extract_mrpack_override(&name) else {
                continue;
            };
            if rel.is_empty() || entry.is_dir() {
                continue;
            }
            if !new_overrides.iter().any(|o| o == rel) {
                continue;
            }
            if packlock::is_protected_path(rel) {
                continue;
            }
            check_safe_relative(rel).map_err(|e| e.to_string())?;
            write_extracted(&stage_ov, rel, &mut entry).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(e) = stage_result {
        cleanup_files(&staging, &temp_zip);
        return Err(format!(
            "Staging overrides failed, instance untouched: {}",
            e
        ));
    }

    // ---- Phase 3: apply (sync — all network work is done) ----
    emit(app, "applying", 0, 1, "Applying update...");
    let plan = plan_modrinth_update(
        &ctx.mc_dir,
        &staging,
        &MrApplyInput {
            old_map: &old_map,
            new_map: &new_map,
            diff: &diff,
            new_overrides: &new_overrides,
            old_overrides: &lock.overrides,
        },
    );

    let new_lock = PackLock {
        provider: "modrinth".to_string(),
        pack_project_id: lock.pack_project_id.clone(),
        pack_name: lock.pack_name.clone(),
        file_id: latest.id.clone(),
        file_name: file.filename.clone(),
        file_display_name: latest.version_number.clone(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        mc_version: new_mc.to_string(),
        loader: new_loader.to_string(),
        manifest: new_manifest,
        overrides: new_overrides,
    };
    let outcome = apply_and_commit(
        &ctx.mc_dir,
        &ctx.instance_dir,
        &plan,
        &staging,
        &temp_zip,
        &new_lock,
    )?;

    emit(app, "done", 1, 1, "Pack updated.");

    Ok(PackUpdateReport {
        diff,
        mods_installed: outcome.mods_installed,
        mods_removed: outcome.mods_removed,
        overrides_written: outcome.overrides_written,
        overrides_removed: outcome.overrides_removed,
        new_version: latest.version_number.clone(),
    })
}

/// mrpack repair: every client file the index lists but the instance is
/// missing gets re-downloaded (embedded lookups are skipped — the pack zip
/// is not held; the index's own URLs / version ids resolve the files).
/// Missing overrides are restored from a fresh pack zip download.
async fn repair_modrinth(
    app: &AppHandle,
    ctx: &PackCtx,
    lock: PackLock,
) -> Result<PackRepairReport, String> {
    let map = packlock::mrpack_manifest_map(&lock.manifest);
    let total = map.len();
    let mut report = PackRepairReport {
        checked: 0,
        restored: 0,
        skipped: 0,
        failed: Vec::new(),
    };
    let no_embedded: Arc<HashMap<String, Vec<u8>>> = Arc::new(HashMap::new());

    for (i, (key, entry)) in map.iter().enumerate() {
        report.checked += 1;
        let path = packlock::mrpack_entry_path(entry);
        if path.is_empty() || check_safe_relative(&path).is_err() {
            report.skipped += 1;
            continue;
        }
        let dest = ctx.mc_dir.join(&path);
        let disabled = dest
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| format!("{}.disabled", n))
            .and_then(|n| dest.parent().map(|p| p.join(n)));
        let present = dest.is_file() || disabled.map(|d| d.is_file()).unwrap_or(false);
        if present {
            report.skipped += 1;
            continue;
        }
        emit(
            app,
            "repairing",
            i,
            total,
            &format!("Restoring file {}/{}...", i + 1, total),
        );
        match crate::import::install_mrpack_file(&path, entry, &ctx.mc_dir, &no_embedded, true)
            .await
        {
            Ok(()) => report.restored += 1,
            Err(e) => report.failed.push(format!("{}: {}", key, e)),
        }
    }

    // ---- Overrides whose file is gone (protected/user files stay out) ----
    let missing: Vec<String> = lock
        .overrides
        .iter()
        .filter(|rel| !packlock::is_protected_path(rel) && !ctx.mc_dir.join(rel).exists())
        .cloned()
        .collect();
    if !missing.is_empty() {
        emit(
            app,
            "repairing",
            total,
            total + 1,
            "Restoring pack overrides...",
        );
        let version = match modrinth::get_version_by_id(&lock.file_id).await {
            Ok(v) => v,
            Err(e) => {
                report
                    .failed
                    .push(format!("Pack version info failed: {}", e));
                return Ok(report);
            }
        };
        let file = version
            .files
            .iter()
            .find(|f| f.primary)
            .or_else(|| version.files.first());
        let Some(file) = file else {
            report.failed.push("Pack version has no files".to_string());
            return Ok(report);
        };
        if !file.url.starts_with("https://") || !crate::is_allowed_download_host(&file.url) {
            report
                .failed
                .push("Pack file URL is not allowed".to_string());
            return Ok(report);
        }
        let temp_zip = std::env::temp_dir().join(format!(
            "voidlauncher-mrpack-repair-{}.mrpack",
            lock.file_id
        ));
        let sha1 = file.hashes.get("sha1").map(|s| s.as_str()).unwrap_or("");
        if let Err(e) =
            crate::download::download_file_sized(&file.url, &temp_zip, sha1, file.size).await
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
            let mut archive =
                zip::ZipArchive::new(f).map_err(|e| format!("Cannot read pack archive: {}", e))?;
            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| format!("Cannot read pack entry: {}", e))?;
                let name = entry.name().to_string();
                let Some(rel) = crate::import::extract_mrpack_override(&name) else {
                    continue;
                };
                if rel.is_empty() || entry.is_dir() || !missing.iter().any(|m| m == rel) {
                    continue;
                }
                check_safe_relative(rel).map_err(|e| e.to_string())?;
                write_extracted(&ctx.mc_dir, rel, &mut entry).map_err(|e| e.to_string())?;
                write_override_sidecar(&ctx.mc_dir, rel, "modrinth");
                report.checked += 1;
                report.restored += 1;
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&temp_zip);
        if let Err(e) = extract {
            report
                .failed
                .push(format!("Overrides restore failed: {}", e));
        }
    }

    emit(app, "done", 1, 1, "Repair finished.");
    Ok(report)
}

#[cfg(test)]
mod update_integrity_tests {
    use super::*;
    use crate::content::SidecarWrite;
    use std::path::{Path, PathBuf};

    /// Temp instance with the folders a pack update touches.
    struct TestPack {
        root: PathBuf,
        instance: PathBuf,
        mc: PathBuf,
        staging: PathBuf,
        temp_zip: PathBuf,
    }

    impl TestPack {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "vl_packlock_{}_{}_{}",
                name,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            let _ = std::fs::remove_dir_all(&root);
            let instance = root.join("inst");
            let mc = instance.join(".minecraft");
            let staging = instance.join(".pack-update");
            std::fs::create_dir_all(mc.join("mods")).unwrap();
            std::fs::create_dir_all(mc.join("config")).unwrap();
            std::fs::create_dir_all(mc.join("saves/world")).unwrap();
            std::fs::create_dir_all(&staging).unwrap();
            let temp_zip = root.join("pack.zip");
            Self {
                root,
                instance,
                mc,
                staging,
                temp_zip,
            }
        }

        fn mods(&self) -> PathBuf {
            self.mc.join("mods")
        }

        fn index(&self) -> PathBuf {
            self.mods().join(".index")
        }
    }

    impl Drop for TestPack {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// Put a staged file where the apply phase expects it.
    fn staged(pack: &TestPack, rel: &str, bytes: &[u8]) -> PathBuf {
        let path = pack.staging.join(rel);
        write(&path, bytes);
        path
    }

    fn installed_mod(name: &str, pid: &str, fid: &str) -> content::InstalledContent {
        content::InstalledContent {
            filename: name.to_string(),
            content_type: "mod".to_string(),
            provider: "curseforge".to_string(),
            project_id: Some(pid.to_string()),
            version_id: None,
            file_id: Some(fid.to_string()),
            project_name: None,
            version_number: None,
            hash: None,
            enabled: !name.ends_with(".disabled"),
            installed_at: None,
        }
    }

    fn cf_file(name: &str, id: u64) -> curseforge::CfFile {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "displayName": format!("Sodium {}", id),
            "fileName": name,
            "downloadUrl": "https://example.invalid/file",
            "fileLength": 10,
            "fileDate": "2026-01-01T00:00:00Z",
            "gameVersions": ["1.20.1"],
            "dependencies": [],
            "releaseType": 1,
            "hashes": [{"value": "0123456789abcdef", "algo": 1}],
            "isAvailable": true
        }))
        .expect("CfFile fixture")
    }

    fn sample_lock(file_id: &str) -> PackLock {
        PackLock {
            provider: "curseforge".to_string(),
            pack_project_id: "10".to_string(),
            pack_name: "Test Pack".to_string(),
            file_id: file_id.to_string(),
            file_name: "pack.zip".to_string(),
            file_display_name: format!("v{}", file_id),
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            mc_version: "1.20.1".to_string(),
            loader: "fabric".to_string(),
            manifest: serde_json::json!({"files": []}),
            overrides: vec![],
        }
    }

    fn write_sidecar(pack: &TestPack, name: &str, provider: &str, file_id: &str) {
        content::write_sidecar(
            &pack.mods(),
            name,
            &SidecarWrite {
                provider,
                content_type: "mod",
                project_id: Some("10"),
                project_name: Some("Sodium"),
                version_id: None,
                version_number: Some("old"),
                file_id: Some(file_id),
                hash: None,
            },
        )
        .unwrap();
    }

    fn content_install(staged: PathBuf, dest: PathBuf, is_mod: bool) -> PlannedInstall {
        PlannedInstall {
            staged,
            dest,
            preserve_disabled: false,
            previous_name: None,
            sidecar: SidecarPlan::Content(SidecarWriteOwned {
                provider: "curseforge".to_string(),
                content_type: "mod".to_string(),
                project_id: Some("10".to_string()),
                project_name: Some("Sodium".to_string()),
                version_id: None,
                version_number: Some("new".to_string()),
                file_id: Some("6".to_string()),
                hash: None,
            }),
            is_mod,
        }
    }

    fn cf_plan(
        pack: &TestPack,
        installed: &[content::InstalledContent],
        staged_mods: &[(PackFileRef, curseforge::CfFile)],
        new_overrides: &[String],
        old_overrides: &[String],
    ) -> ApplyPlan {
        plan_curseforge_update(
            &pack.mods(),
            &pack.mc,
            &pack.staging.join("mods"),
            &pack.staging.join("overrides"),
            &CfApplyInput {
                installed,
                staged_mods,
                diff: &PackDiff::default(),
                new_overrides,
                old_overrides,
            },
        )
    }

    // ---- .disabled preservation -----------------------------------------

    #[test]
    fn curseforge_disabled_mod_is_replaced_as_disabled() {
        let pack = TestPack::new("cf_disabled");
        write(&pack.mods().join("sodium-0.5.jar.disabled"), b"old");
        write_sidecar(&pack, "sodium-0.5.jar", "curseforge", "5");
        let installed = content::list_installed(&pack.mods(), "mod");
        assert!(installed[0].filename.ends_with(".disabled"));

        staged(&pack, "mods/sodium-0.6.jar", b"new");
        let staged_mods = vec![(
            PackFileRef {
                project_id: "10".to_string(),
                file_id: "6".to_string(),
            },
            cf_file("sodium-0.6.jar", 6),
        )];
        let plan = cf_plan(&pack, &installed, &staged_mods, &[], &[]);
        assert!(plan.installs[0].preserve_disabled, "the mod was disabled");

        let outcome = apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect("update succeeds");

        assert_eq!(outcome.mods_installed, 1);
        assert!(
            pack.mods().join("sodium-0.6.jar.disabled").is_file(),
            "the replacement must stay disabled"
        );
        assert!(!pack.mods().join("sodium-0.6.jar").exists());
        assert!(!pack.mods().join("sodium-0.5.jar.disabled").exists());
        assert!(pack.index().join("sodium-0.6.voidlauncher.json").is_file());
        assert!(!pack.index().join("sodium-0.5.voidlauncher.json").exists());
    }

    #[test]
    fn modrinth_disabled_mod_is_replaced_as_disabled_when_the_path_changes() {
        let pack = TestPack::new("mr_disabled");
        write(&pack.mods().join("sodium-0.5.jar.disabled"), b"old");
        write_sidecar(&pack, "sodium-0.5.jar", "modrinth", "v5");

        let old_manifest = serde_json::json!({
            "files": [{
                "path": "mods/sodium-0.5.jar",
                "project_id": "sodium",
                "version_id": "v5",
                "hashes": {"sha1": "a"}
            }]
        });
        let new_manifest = serde_json::json!({
            "files": [{
                "path": "mods/sodium-0.6.jar",
                "project_id": "sodium",
                "version_id": "v6",
                "hashes": {"sha1": "b"}
            }]
        });
        let old_map = packlock::mrpack_manifest_map(&old_manifest);
        let new_map = packlock::mrpack_manifest_map(&new_manifest);
        let diff = packlock::diff_mrpack_manifests(&old_manifest, &new_manifest);
        staged(&pack, "mods/sodium-0.6.jar", b"new");

        let plan = plan_modrinth_update(
            &pack.mc,
            &pack.staging,
            &MrApplyInput {
                old_map: &old_map,
                new_map: &new_map,
                diff: &diff,
                new_overrides: &[],
                old_overrides: &[],
            },
        );
        assert!(plan.installs[0].preserve_disabled, "the mod was disabled");
        assert!(
            plan.removals
                .iter()
                .any(|r| r.path.ends_with("sodium-0.5.jar.disabled")),
            "the old disabled file must be parked"
        );

        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect("update succeeds");

        assert!(pack.mods().join("sodium-0.6.jar.disabled").is_file());
        assert!(!pack.mods().join("sodium-0.6.jar").exists());
        assert!(!pack.mods().join("sodium-0.5.jar.disabled").exists());
    }

    #[test]
    fn enabled_mod_stays_enabled_and_a_new_mod_is_installed_enabled() {
        let pack = TestPack::new("enabled");
        write(&pack.mods().join("sodium-0.5.jar"), b"old");
        write_sidecar(&pack, "sodium-0.5.jar", "curseforge", "5");
        let installed = content::list_installed(&pack.mods(), "mod");

        staged(&pack, "mods/sodium-0.6.jar", b"new");
        let staged_mods = vec![(
            PackFileRef {
                project_id: "10".to_string(),
                file_id: "6".to_string(),
            },
            cf_file("sodium-0.6.jar", 6),
        )];
        let plan = cf_plan(&pack, &installed, &staged_mods, &[], &[]);
        assert!(!plan.installs[0].preserve_disabled);

        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect("update succeeds");
        assert!(pack.mods().join("sodium-0.6.jar").is_file());
        assert!(!pack.mods().join("sodium-0.6.jar.disabled").exists());

        // A brand-new mod has no disabled state to preserve.
        let pack2 = TestPack::new("brand_new");
        staged(&pack2, "mods/lithium.jar", b"lithium");
        let staged_mods = vec![(
            PackFileRef {
                project_id: "11".to_string(),
                file_id: "7".to_string(),
            },
            cf_file("lithium.jar", 7),
        )];
        let plan = cf_plan(&pack2, &[], &staged_mods, &[], &[]);
        assert!(!plan.installs[0].preserve_disabled);
        apply_and_commit(
            &pack2.mc,
            &pack2.instance,
            &plan,
            &pack2.staging,
            &pack2.temp_zip,
            &sample_lock("7"),
        )
        .expect("install succeeds");
        assert!(pack2.mods().join("lithium.jar").is_file());
    }

    #[test]
    fn disabled_override_keeps_its_state() {
        let pack = TestPack::new("override_disabled");
        write(&pack.mods().join("packmod.jar.disabled"), b"old");
        staged(&pack, "overrides/mods/packmod.jar", b"new");
        let plan = cf_plan(&pack, &[], &[], &["mods/packmod.jar".to_string()], &[]);
        assert_eq!(plan.installs.len(), 1);
        assert!(
            plan.installs[0].preserve_disabled,
            "an override replacing a disabled file must stay disabled"
        );
    }

    // ---- rollback --------------------------------------------------------

    #[test]
    fn failed_apply_restores_files_sidecars_and_removes_new_content() {
        let pack = TestPack::new("rollback");
        write(&pack.mods().join("sodium.jar"), b"old-sodium");
        write_sidecar(&pack, "sodium.jar", "curseforge", "5");
        let sidecar_before = std::fs::read(pack.index().join("sodium.voidlauncher.json")).unwrap();
        write(&pack.mc.join("config/user.toml"), b"user-config");
        write(&pack.mc.join("saves/world/level.dat"), b"world-bytes");

        // The first install succeeds; the second one points at a missing
        // staged file and fails after the replacement is already in place.
        let plan = ApplyPlan {
            installs: vec![
                content_install(
                    staged(&pack, "mods/sodium.jar", b"new-sodium"),
                    pack.mods().join("sodium.jar"),
                    true,
                ),
                content_install(
                    pack.staging.join("mods/missing.jar"),
                    pack.mods().join("missing.jar"),
                    true,
                ),
            ],
            removals: vec![],
        };

        let err = apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect_err("the missing staged file must fail the apply");
        assert!(err.contains("rolled back"), "got: {err}");

        assert_eq!(
            std::fs::read(pack.mods().join("sodium.jar")).unwrap(),
            b"old-sodium",
            "the parked original must be restored"
        );
        assert_eq!(
            std::fs::read(pack.index().join("sodium.voidlauncher.json")).unwrap(),
            sidecar_before,
            "the sidecar must be restored"
        );
        assert!(!pack.mods().join("missing.jar").exists());
        assert!(!pack.mods().join("sodium.jar.voidlauncher-old").exists());
        assert!(!pack.staging.exists(), "staging must be cleaned up");
        assert_eq!(
            std::fs::read(pack.mc.join("config/user.toml")).unwrap(),
            b"user-config"
        );
        assert_eq!(
            std::fs::read(pack.mc.join("saves/world/level.dat")).unwrap(),
            b"world-bytes"
        );
        assert!(
            !pack.instance.join("pack-lock.json").exists(),
            "a failed apply must not commit a lock"
        );
    }

    #[test]
    fn rollback_can_run_twice_without_damaging_the_restored_tree() {
        let pack = TestPack::new("rb_twice");
        write(&pack.mods().join("sodium.jar"), b"old");
        write_sidecar(&pack, "sodium.jar", "curseforge", "5");

        let mut rb = ApplyRollback::default();
        rb.snapshot_sidecar(&pack.mods(), "sodium.jar");
        let stashed = instances::stash_old_content_files(&pack.mods(), "sodium.jar").unwrap();
        rb.stashes.extend(stashed);
        write(&pack.mods().join("sodium.jar"), b"new");
        rb.moved.push(pack.mods().join("sodium.jar"));

        rb.rollback(&pack.staging, &pack.temp_zip);
        rb.rollback(&pack.staging, &pack.temp_zip);

        assert_eq!(
            std::fs::read(pack.mods().join("sodium.jar")).unwrap(),
            b"old"
        );
        assert!(!pack.mods().join("sodium.jar.voidlauncher-old").exists());
        assert!(pack.index().join("sodium.voidlauncher.json").is_file());
    }

    // ---- lock commit -----------------------------------------------------

    #[test]
    fn successful_update_commits_a_lock_matching_the_applied_files() {
        let pack = TestPack::new("lock_ok");
        write(&pack.mods().join("sodium-0.5.jar"), b"old");
        write_sidecar(&pack, "sodium-0.5.jar", "curseforge", "5");
        let installed = content::list_installed(&pack.mods(), "mod");
        staged(&pack, "mods/sodium-0.6.jar", b"new");
        let staged_mods = vec![(
            PackFileRef {
                project_id: "10".to_string(),
                file_id: "6".to_string(),
            },
            cf_file("sodium-0.6.jar", 6),
        )];
        let plan = cf_plan(&pack, &installed, &staged_mods, &[], &[]);
        let new_lock = sample_lock("6");

        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &new_lock,
        )
        .expect("update succeeds");

        assert_eq!(
            PackLock::read(&pack.instance),
            Some(new_lock),
            "the committed lock must describe the applied version"
        );
        assert!(pack.mods().join("sodium-0.6.jar").is_file());
        assert!(!pack.mods().join("sodium-0.5.jar").exists());
        assert!(!pack.mods().join("sodium-0.5.jar.voidlauncher-old").exists());
        assert!(!pack.staging.exists());
    }

    #[test]
    fn lock_write_failure_rolls_back_and_keeps_the_previous_lock() {
        let pack = TestPack::new("lock_fail");
        write(&pack.mods().join("sodium.jar"), b"old");
        write_sidecar(&pack, "sodium.jar", "curseforge", "5");
        let sidecar_before = std::fs::read(pack.index().join("sodium.voidlauncher.json")).unwrap();
        let old_lock = sample_lock("5");
        old_lock.write(&pack.instance).unwrap();
        let lock_before = std::fs::read(pack.instance.join("pack-lock.json")).unwrap();

        // Controlled failure point: the atomic writer's temp path is blocked
        // by a directory, so the lock write fails deterministically.
        std::fs::create_dir_all(pack.instance.join(".pack-lock.json.tmp")).unwrap();

        let plan = ApplyPlan {
            installs: vec![content_install(
                staged(&pack, "mods/sodium.jar", b"new"),
                pack.mods().join("sodium.jar"),
                true,
            )],
            removals: vec![],
        };
        let err = apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect_err("the blocked lock write must fail the update");
        assert!(err.contains("rolled back"), "got: {err}");

        assert_eq!(
            std::fs::read(pack.mods().join("sodium.jar")).unwrap(),
            b"old",
            "file changes must be rolled back when the lock cannot be written"
        );
        assert_eq!(
            std::fs::read(pack.index().join("sodium.voidlauncher.json")).unwrap(),
            sidecar_before
        );
        assert_eq!(
            std::fs::read(pack.instance.join("pack-lock.json")).unwrap(),
            lock_before,
            "the previous lock must stay intact"
        );
        assert_eq!(PackLock::read(&pack.instance), Some(old_lock));
        assert!(!pack.staging.exists());
    }

    // ---- ownership -------------------------------------------------------

    #[test]
    fn only_sidecar_backed_overrides_are_removed_and_protected_paths_are_skipped() {
        let pack = TestPack::new("ownership");
        write(&pack.mods().join("pack.jar"), b"pack");
        write_sidecar(&pack, "pack.jar", "curseforge", "7");
        write(&pack.mc.join("config/pack.toml"), b"user-config");
        write(&pack.mc.join("saves/world/level.dat"), b"world");
        staged(&pack, "overrides/options.txt", b"pack-options");
        write(&pack.mc.join("options.txt"), b"user-options");

        let old = vec![
            "mods/pack.jar".to_string(),
            "config/pack.toml".to_string(),
            "saves/world/level.dat".to_string(),
        ];
        let plan = cf_plan(&pack, &[], &[], &["options.txt".to_string()], &old);

        assert!(
            plan.installs.is_empty(),
            "a protected override must never be planned"
        );
        assert_eq!(
            plan.removals.len(),
            1,
            "only the sidecar-backed mod may be removed: {:?}",
            plan.removals.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
        assert!(plan.removals[0].path.ends_with("pack.jar"));

        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("8"),
        )
        .expect("update succeeds");

        assert_eq!(
            std::fs::read(pack.mc.join("config/pack.toml")).unwrap(),
            b"user-config"
        );
        assert_eq!(
            std::fs::read(pack.mc.join("options.txt")).unwrap(),
            b"user-options"
        );
        assert_eq!(
            std::fs::read(pack.mc.join("saves/world/level.dat")).unwrap(),
            b"world"
        );
        assert!(!pack.mods().join("pack.jar").exists());
    }

    #[test]
    fn curseforge_disabled_mod_with_the_same_file_name_stays_disabled() {
        let pack = TestPack::new("cf_disabled_same_name");
        write(&pack.mods().join("sodium.jar.disabled"), b"old-version");
        write_sidecar(&pack, "sodium.jar", "curseforge", "5");
        let installed = content::list_installed(&pack.mods(), "mod");
        staged(&pack, "mods/sodium.jar", b"new-version");
        let staged_mods = vec![(
            PackFileRef {
                project_id: "10".to_string(),
                file_id: "6".to_string(),
            },
            cf_file("sodium.jar", 6),
        )];

        let plan = cf_plan(&pack, &installed, &staged_mods, &[], &[]);
        assert!(
            plan.removals.is_empty(),
            "the disabled file is the same target as the install and must not be queued for removal"
        );

        let new_lock = sample_lock("6");
        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &new_lock,
        )
        .expect("update succeeds");

        assert_eq!(
            std::fs::read(pack.mods().join("sodium.jar.disabled")).unwrap(),
            b"new-version",
            "the updated mod must be present and disabled"
        );
        assert!(!pack.mods().join("sodium.jar").exists(), "no enabled twin");
        assert!(!pack.mods().join("sodium.jar.voidlauncher-old").exists());
        assert!(!pack
            .mods()
            .join("sodium.jar.disabled.voidlauncher-old")
            .exists());

        let sidecar: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(pack.index().join("sodium.voidlauncher.json")).unwrap(),
        )
        .expect("sidecar is valid JSON");
        assert_eq!(sidecar["provider"], "curseforge");
        assert_eq!(sidecar["content_type"], "mod");
        assert_eq!(sidecar["project_id"], "10");
        assert_eq!(sidecar["file_id"], "6");
        assert_eq!(PackLock::read(&pack.instance), Some(new_lock));
    }

    #[test]
    fn curseforge_removed_mod_and_override_at_the_same_path_keep_the_override() {
        let pack = TestPack::new("cf_override_conflict");
        write(&pack.mods().join("pack.jar"), b"old-mod");
        write_sidecar(&pack, "pack.jar", "curseforge", "5");
        write(&pack.mc.join("config/user.toml"), b"user-config");
        staged(&pack, "overrides/mods/pack.jar", b"new-override");

        let old_manifest =
            serde_json::json!({"files": [{"projectID": 10, "fileID": 5, "required": true}]});
        let new_manifest = serde_json::json!({"files": []});
        let diff = packlock::diff_manifests(&old_manifest, &new_manifest);
        assert_eq!(diff.removed.len(), 1, "the mod is dropped by the manifest");

        let installed = content::list_installed(&pack.mods(), "mod");
        let plan = plan_curseforge_update(
            &pack.mods(),
            &pack.mc,
            &pack.staging.join("mods"),
            &pack.staging.join("overrides"),
            &CfApplyInput {
                installed: &installed,
                staged_mods: &[],
                diff: &diff,
                new_overrides: &["mods/pack.jar".to_string()],
                old_overrides: &[],
            },
        );
        assert_eq!(plan.installs.len(), 1, "the override is installed");
        assert!(
            plan.removals.is_empty(),
            "the removed-mod entry must not delete the override installed at the same path"
        );

        let new_lock = sample_lock("6");
        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &new_lock,
        )
        .expect("update succeeds");

        assert_eq!(
            std::fs::read(pack.mods().join("pack.jar")).unwrap(),
            b"new-override",
            "the override wins over the removed manifest entry"
        );
        assert!(!pack.mods().join("pack.jar.voidlauncher-old").exists());
        assert_eq!(
            std::fs::read(pack.mc.join("config/user.toml")).unwrap(),
            b"user-config",
            "user files outside the pack inventory are untouched"
        );
        let sidecar: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(pack.index().join("pack.voidlauncher.json")).unwrap(),
        )
        .expect("sidecar is valid JSON");
        assert_eq!(sidecar["downloaded_from_pack"], true);
        assert!(
            sidecar.get("file_id").is_none(),
            "stale mod metadata must not describe the override: {sidecar}"
        );
        assert_eq!(PackLock::read(&pack.instance), Some(new_lock));
    }

    #[test]
    fn modrinth_removed_file_and_override_at_the_same_path_keep_the_override() {
        let pack = TestPack::new("mr_override_conflict");
        write(&pack.mods().join("pack.jar"), b"old-mod");
        write_sidecar(&pack, "pack.jar", "modrinth", "v5");
        write(&pack.mc.join("config/user.toml"), b"user-config");
        staged(&pack, "overrides/mods/pack.jar", b"new-override");

        let old_manifest = serde_json::json!({
            "files": [{
                "path": "mods/pack.jar",
                "project_id": "pack",
                "version_id": "v5",
                "hashes": {"sha1": "a"}
            }]
        });
        let new_manifest = serde_json::json!({"files": []});
        let old_map = packlock::mrpack_manifest_map(&old_manifest);
        let new_map = packlock::mrpack_manifest_map(&new_manifest);
        let diff = packlock::diff_mrpack_manifests(&old_manifest, &new_manifest);

        let plan = plan_modrinth_update(
            &pack.mc,
            &pack.staging,
            &MrApplyInput {
                old_map: &old_map,
                new_map: &new_map,
                diff: &diff,
                new_overrides: &["mods/pack.jar".to_string()],
                old_overrides: &[],
            },
        );
        assert_eq!(plan.installs.len(), 1);
        assert!(
            plan.removals.is_empty(),
            "the removed pack file must not delete the override installed at the same path"
        );

        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect("update succeeds");

        assert_eq!(
            std::fs::read(pack.mods().join("pack.jar")).unwrap(),
            b"new-override"
        );
        assert_eq!(
            std::fs::read(pack.mc.join("config/user.toml")).unwrap(),
            b"user-config"
        );
        let sidecar: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(pack.index().join("pack.voidlauncher.json")).unwrap(),
        )
        .expect("sidecar is valid JSON");
        assert_eq!(sidecar["downloaded_from_pack"], true);
    }

    #[test]
    fn failed_lock_in_a_conflicting_update_restores_the_removed_mod_and_sidecar() {
        let pack = TestPack::new("cf_conflict_rollback");
        write(&pack.mods().join("pack.jar"), b"old-mod");
        write_sidecar(&pack, "pack.jar", "curseforge", "5");
        let sidecar_before = std::fs::read(pack.index().join("pack.voidlauncher.json")).unwrap();
        staged(&pack, "overrides/mods/pack.jar", b"new-override");

        let old_manifest =
            serde_json::json!({"files": [{"projectID": 10, "fileID": 5, "required": true}]});
        let new_manifest = serde_json::json!({"files": []});
        let diff = packlock::diff_manifests(&old_manifest, &new_manifest);
        let installed = content::list_installed(&pack.mods(), "mod");
        let plan = plan_curseforge_update(
            &pack.mods(),
            &pack.mc,
            &pack.staging.join("mods"),
            &pack.staging.join("overrides"),
            &CfApplyInput {
                installed: &installed,
                staged_mods: &[],
                diff: &diff,
                new_overrides: &["mods/pack.jar".to_string()],
                old_overrides: &[],
            },
        );

        // Controlled lock failure after the override replaced the old mod.
        std::fs::create_dir_all(pack.instance.join(".pack-lock.json.tmp")).unwrap();
        let err = apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect_err("the blocked lock write must fail the update");
        assert!(err.contains("rolled back"), "got: {err}");

        assert_eq!(
            std::fs::read(pack.mods().join("pack.jar")).unwrap(),
            b"old-mod",
            "the removed mod must come back when the update cannot commit"
        );
        assert_eq!(
            std::fs::read(pack.index().join("pack.voidlauncher.json")).unwrap(),
            sidecar_before,
            "the old mod sidecar must be restored"
        );
        assert!(!pack.mods().join("pack.jar.voidlauncher-old").exists());
    }

    #[test]
    fn removals_are_matched_to_installs_case_insensitively() {
        let pack = TestPack::new("case_conflict");
        write(&pack.mods().join("foo.jar"), b"old");
        write_sidecar(&pack, "foo.jar", "curseforge", "5");
        let installed = content::list_installed(&pack.mods(), "mod");
        staged(&pack, "mods/Foo.jar", b"new");
        let staged_mods = vec![(
            PackFileRef {
                project_id: "10".to_string(),
                file_id: "6".to_string(),
            },
            cf_file("Foo.jar", 6),
        )];

        let plan = cf_plan(&pack, &installed, &staged_mods, &[], &[]);
        assert!(
            plan.removals.is_empty(),
            "a case-only difference is the same Windows target: {:?}",
            plan.removals.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn path_key_strips_disabled_only_for_direct_content_files() {
        let mc = Path::new(r"C:\workspace\mods\instances\test\.minecraft");

        // A parent folder named `mods` must not turn unrelated files into
        // content files.
        let config_disabled =
            Path::new(r"C:\workspace\mods\instances\test\config\settings.disabled");
        assert!(!in_content_dir(config_disabled, mc));
        assert!(
            path_key(config_disabled, mc).ends_with("settings.disabled"),
            "a config name keeps its `.disabled` suffix"
        );

        for dir in ["mods", "resourcepacks", "shaderpacks"] {
            let disabled = mc.join(dir).join("example.jar.disabled");
            let plain = mc.join(dir).join("example.jar");
            assert!(in_content_dir(&disabled, mc), "{dir}: {disabled:?}");
            assert!(in_content_dir(&plain, mc));
            assert_eq!(
                path_key(&disabled, mc),
                path_key(&plain, mc),
                "{dir}: the disabled twin is the same target"
            );
        }

        // Nested folders are not direct content files.
        let nested = mc.join("mods").join("nested").join("example.jar.disabled");
        assert!(!in_content_dir(&nested, mc));
        assert!(path_key(&nested, mc).ends_with(".disabled"));
    }

    #[test]
    fn a_config_named_disabled_is_not_confused_with_its_plain_name() {
        // Instance root deliberately contains a `mods` component.
        let root = std::env::temp_dir().join("vl_packlock_key/mods/instances/test");
        let mc = root.join(".minecraft");
        let mut plan = ApplyPlan {
            installs: vec![PlannedInstall {
                staged: mc.join("config/settings.disabled"),
                dest: mc.join("config/settings.disabled"),
                preserve_disabled: false,
                previous_name: None,
                sidecar: SidecarPlan::Override("curseforge"),
                is_mod: false,
            }],
            removals: vec![PlannedRemoval {
                path: mc.join("config/settings"),
                is_mod: false,
                counted: true,
            }],
        };

        plan.drop_conflicting_removals(&mc);
        assert_eq!(
            plan.removals.len(),
            1,
            "a config named `settings.disabled` is not the same target as `settings`"
        );

        // A genuine content twin is still the same target.
        let mut content = ApplyPlan {
            installs: vec![PlannedInstall {
                staged: mc.join("mods/example.jar"),
                dest: mc.join("mods/example.jar"),
                preserve_disabled: false,
                previous_name: None,
                sidecar: SidecarPlan::Override("curseforge"),
                is_mod: true,
            }],
            removals: vec![PlannedRemoval {
                path: mc.join("mods/example.jar.disabled"),
                is_mod: true,
                counted: true,
            }],
        };
        content.drop_conflicting_removals(&mc);
        assert!(content.removals.is_empty());
    }

    #[test]
    fn override_wins_over_a_manifest_file_at_the_same_path() {
        let pack = TestPack::new("cf_manifest_vs_override");
        write(&pack.mods().join("example.jar"), b"old");
        write_sidecar(&pack, "example.jar", "curseforge", "5");
        staged(&pack, "mods/example.jar", b"manifest-bytes");
        staged(&pack, "overrides/mods/example.jar", b"override-bytes");

        let staged_mods = vec![(
            PackFileRef {
                project_id: "10".to_string(),
                file_id: "6".to_string(),
            },
            cf_file("example.jar", 6),
        )];
        let plan = cf_plan(
            &pack,
            &content::list_installed(&pack.mods(), "mod"),
            &staged_mods,
            &["mods/example.jar".to_string()],
            &[],
        );
        assert_eq!(plan.installs.len(), 1, "one file, one install");
        assert!(
            matches!(
                plan.installs[0].sidecar,
                SidecarPlan::Override("curseforge")
            ),
            "the surviving install is the override"
        );

        let new_lock = sample_lock("6");
        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &new_lock,
        )
        .expect("update succeeds");

        assert_eq!(
            std::fs::read(pack.mods().join("example.jar")).unwrap(),
            b"override-bytes",
            "the pack override is applied on top of the manifest file"
        );
        assert!(!pack.mods().join("example.jar.voidlauncher-old").exists());
        let sidecar: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(pack.index().join("example.voidlauncher.json")).unwrap(),
        )
        .expect("sidecar is valid JSON");
        assert_eq!(sidecar["downloaded_from_pack"], true);
        assert!(sidecar.get("file_id").is_none());
        assert_eq!(PackLock::read(&pack.instance), Some(new_lock));
    }

    #[test]
    fn modrinth_override_wins_over_a_manifest_file_at_the_same_path() {
        let pack = TestPack::new("mr_manifest_vs_override");
        write(&pack.mods().join("example.jar"), b"old");
        write_sidecar(&pack, "example.jar", "modrinth", "v5");
        staged(&pack, "mods/example.jar", b"manifest-bytes");
        staged(&pack, "overrides/mods/example.jar", b"override-bytes");

        let old_manifest = serde_json::json!({"files": []});
        let new_manifest = serde_json::json!({
            "files": [{
                "path": "mods/example.jar",
                "project_id": "example",
                "version_id": "v6",
                "hashes": {"sha1": "b"}
            }]
        });
        let old_map = packlock::mrpack_manifest_map(&old_manifest);
        let new_map = packlock::mrpack_manifest_map(&new_manifest);
        let diff = packlock::diff_mrpack_manifests(&old_manifest, &new_manifest);
        let plan = plan_modrinth_update(
            &pack.mc,
            &pack.staging,
            &MrApplyInput {
                old_map: &old_map,
                new_map: &new_map,
                diff: &diff,
                new_overrides: &["mods/example.jar".to_string()],
                old_overrides: &[],
            },
        );
        assert_eq!(plan.installs.len(), 1);
        assert!(matches!(
            plan.installs[0].sidecar,
            SidecarPlan::Override("modrinth")
        ));

        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect("update succeeds");
        assert_eq!(
            std::fs::read(pack.mods().join("example.jar")).unwrap(),
            b"override-bytes"
        );
        assert!(!pack.mods().join("example.jar.voidlauncher-old").exists());
    }

    #[test]
    fn failed_override_install_restores_the_shadowed_mod() {
        let pack = TestPack::new("cf_override_rollback");
        write(&pack.mods().join("example.jar"), b"old");
        write_sidecar(&pack, "example.jar", "curseforge", "5");
        let sidecar_before = std::fs::read(pack.index().join("example.voidlauncher.json")).unwrap();
        staged(&pack, "mods/example.jar", b"manifest-bytes");
        let override_staged = staged(&pack, "overrides/mods/example.jar", b"override-bytes");

        let staged_mods = vec![(
            PackFileRef {
                project_id: "10".to_string(),
                file_id: "6".to_string(),
            },
            cf_file("example.jar", 6),
        )];
        let plan = cf_plan(
            &pack,
            &content::list_installed(&pack.mods(), "mod"),
            &staged_mods,
            &["mods/example.jar".to_string()],
            &[],
        );

        // The installation is planned, then its staged file disappears.
        std::fs::remove_file(&override_staged).unwrap();
        let err = apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect_err("the missing staged override must fail the update");
        assert!(err.contains("rolled back"), "got: {err}");

        assert_eq!(
            std::fs::read(pack.mods().join("example.jar")).unwrap(),
            b"old",
            "the original file is restored"
        );
        assert_eq!(
            std::fs::read(pack.index().join("example.voidlauncher.json")).unwrap(),
            sidecar_before,
            "the original sidecar is restored"
        );
        assert!(!pack.mods().join("example.jar.voidlauncher-old").exists());
        assert!(!pack.instance.join("pack-lock.json").exists());
    }

    #[test]
    fn installed_mod_helper_matches_provider_identity() {
        // Guards the fixture the other tests rely on.
        let rec = installed_mod("sodium.jar.disabled", "10", "5");
        assert_eq!(rec.provider, "curseforge");
        assert!(!rec.enabled);
        assert!(rec.filename.ends_with(".disabled"));
    }
}

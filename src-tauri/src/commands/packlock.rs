//! Modpack lifecycle (roadmap items 25-29): lock probe, update check,
//! transactional update and manifest repair for CurseForge and Modrinth pack
//! instances, driven by the `<instance>/pack-lock.json` lock file.
//!
//! Safety model (update and repair): everything new is staged first
//! (downloaded beside the instance, never in it), then applied with
//! park-aside renames — the old file is only parked, and every park is
//! discarded after the operation commits. Any failure restores the parks and
//! sidecars in reverse order. A failed rollback reports both errors and
//! leaves recovery data in staging instead of claiming success.
//!
//! The rollback is an error-handling mechanism, not crash recovery: a power
//! loss between two applied files still leaves a partially applied instance.

use crate::commands::instances::validate_instance_name;
use crate::content::{self, SidecarWrite};
use crate::curseforge::{self, CfFile};
use crate::import::{
    check_safe_file_name, check_safe_relative, write_extracted_budgeted, write_override_sidecar,
    ImportProgressPayload,
};
use crate::instances;
use crate::modrinth::{self, ModrinthVersion};
use crate::packlock::{self, PackDiff, PackFileRef, PackLock};
use crate::AppState;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

// ==================== Shared helpers ====================

struct PackCtx {
    instance: crate::instances::Instance,
    instance_dir: PathBuf,
    mc_dir: PathBuf,
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
        instance,
        instance_dir,
        mc_dir,
        api_key: config.curseforge_api_key.clone(),
    })
}

async fn cf_project_metadata(
    project_ids: &[String],
    api_key: &str,
) -> Result<Vec<curseforge::CfBulkMod>, String> {
    let mut numeric: Vec<u64> = project_ids
        .iter()
        .map(|id| {
            id.parse::<u64>()
                .map_err(|_| format!("Invalid CurseForge project id '{id}'"))
        })
        .collect::<Result<_, _>>()?;
    numeric.sort_unstable();
    numeric.dedup();
    curseforge::get_mods_by_ids_chunked(&numeric, api_key)
        .await
        .map_err(|e| format!("CurseForge project metadata failed: {e}"))
}

/// Resolve the real content class of every CurseForge project in one batch.
/// `None` means the API did not return that project; callers must report it,
/// never fall back to `mods/`.
async fn cf_project_classes(
    project_ids: &[String],
    api_key: &str,
) -> Result<HashMap<String, crate::curseforge::CfContentClass>, String> {
    let projects = cf_project_metadata(project_ids, api_key).await?;
    let mut classes = HashMap::new();
    for id in project_ids {
        let numeric = id
            .parse::<u64>()
            .map_err(|_| format!("Invalid CurseForge project id '{id}'"))?;
        let class = curseforge::content_class_from_bulk(&projects, numeric).ok_or_else(|| {
            format!(
                "CurseForge project {id} has no supported content class \
                 (mod, resource pack or shader pack); the operation was stopped."
            )
        })?;
        classes.insert(id.clone(), class);
    }
    Ok(classes)
}

fn cf_destination_dir(mc_dir: &Path, class: crate::curseforge::CfContentClass) -> PathBuf {
    mc_dir.join(class.folder())
}

/// Staging layout: mods keep the historical root filename (tests and existing
/// staging code rely on it), other classes get their own subfolder so two
/// packs with the same file name cannot overwrite each other.
fn cf_staged_path(
    staging_root: &Path,
    class: crate::curseforge::CfContentClass,
    file_name: &str,
) -> PathBuf {
    if class == crate::curseforge::CfContentClass::Mod {
        staging_root.join(file_name)
    } else {
        staging_root.join(class.folder()).join(file_name)
    }
}

fn cf_content_class_for_type(content_type: &str) -> crate::curseforge::CfContentClass {
    match content_type {
        "resourcepack" => crate::curseforge::CfContentClass::ResourcePack,
        "shader" => crate::curseforge::CfContentClass::ShaderPack,
        _ => crate::curseforge::CfContentClass::Mod,
    }
}

fn list_cf_installed(mc_dir: &Path) -> Vec<content::InstalledContent> {
    let mut installed = Vec::new();
    for (dir, content_type) in [
        ("mods", "mod"),
        ("resourcepacks", "resourcepack"),
        ("shaderpacks", "shader"),
    ] {
        installed.extend(content::list_installed(&mc_dir.join(dir), content_type));
    }
    installed
}

fn emit(app: &AppHandle, stage: &str, current: usize, total: usize, message: &str) {
    let _ = app.emit(
        "import-progress",
        ImportProgressPayload {
            stage: stage.to_string(),
            current,
            total,
            message: message.to_string(),
            bytes_done: 0,
            bytes_total: 0,
        },
    );
}

/// Outcome of looking for an update inside the installed release channel.
#[derive(Debug)]
enum PackCandidate<T> {
    Newer(T),
    UpToDate,
    /// The provider did not tell us the installed file's channel, or the
    /// stored lock value conflicts with the provider. Automatic updates are
    /// unsafe in that state: stay put instead of guessing.
    UnknownChannel,
    /// The provider version strings cannot be compared reliably. Publication
    /// dates are not a version order, so automatic updates stay disabled.
    UnknownVersionOrder,
}

/// Combine the channel stored in the lock with the provider's answer. The
/// lock value keeps old locks working and catches provider metadata changes;
/// a conflict is treated as unknown rather than silently switching channels.
fn resolve_installed_channel(
    stored: &str,
    detected: Option<packlock::PackChannel>,
) -> Option<packlock::PackChannel> {
    match (packlock::PackChannel::from_lock(stored), detected) {
        (Some(stored), Some(detected)) if stored != detected => None,
        (Some(stored), _) => Some(stored),
        (None, detected) => detected,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionOrder {
    Newer,
    NotNewer,
    Unknown,
}

/// A provider version string that passed a *total* ordering grammar.
///
/// Two families are accepted, and values from different families are never
/// compared with each other:
///
/// - complete SemVer (`1.2.3`, `1.2.3-beta.1`, `1.2.3+build.4`), with an
///   optional leading `v` (`v1.2.3`);
/// - one to four dot-separated decimal components (`40`, `5.3`, `1.1.8.2`),
///   also with an optional leading `v` (`v40`).
///
/// Everything else — loader/Minecraft suffixes, a single-letter revision,
/// human-readable labels, empty strings — is intentionally rejected: no
/// unambiguous order can be proven for it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedPackVersion {
    Semver(semver::Version),
    NumericDotted(Vec<u64>),
}

/// Parse only complete, unambiguous version grammars. Provider pack versions
/// are often arbitrary labels ("1.20.1-forge", "build 17"), so a failed parse
/// is not treated as an old version: it means the order cannot be proven.
fn parse_pack_version(value: &str) -> Option<ParsedPackVersion> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // A leading `v` is a spelling of the same number, not a different format.
    let value = value
        .strip_prefix('v')
        .or_else(|| value.strip_prefix('V'))
        .unwrap_or(value);
    if let Ok(version) = semver::Version::parse(value) {
        // `1.20.1-forge` is technically valid SemVer, but in provider data it
        // labels a Minecraft version plus loader, not the pack's own version.
        // Ordering such strings could select a build for another loader/MC
        // version, so treat them as unorderable.
        const LOADER_TAGS: &[&str] = &[
            "forge",
            "fabric",
            "neoforge",
            "quilt",
            "vanilla",
            "liteloader",
        ];
        if version
            .pre
            .as_str()
            .split('.')
            .any(|part| LOADER_TAGS.iter().any(|tag| part.eq_ignore_ascii_case(tag)))
        {
            return None;
        }
        return Some(ParsedPackVersion::Semver(version));
    }
    parse_numeric_dotted(value).map(ParsedPackVersion::NumericDotted)
}

/// `1`–`4` decimal components, nothing else. Four components cover the
/// common `1.1.8.2` revision style; more than four is not a format we can
/// vouch for.
fn parse_numeric_dotted(value: &str) -> Option<Vec<u64>> {
    if value.len() > 64 {
        return None;
    }
    let mut parts = Vec::new();
    for part in value.split('.') {
        if part.is_empty() || part.len() > 20 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        parts.push(part.parse::<u64>().ok()?);
    }
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    Some(parts)
}

/// Compare two parsed provider versions.
///
/// SemVer precedence (which ignores build metadata) is used for SemVer
/// values; numeric dotted values are compared component-wise with implicit
/// zero padding (`1.2` == `1.2.0`). Mixed families return `None`: the caller
/// must treat the order as unknown instead of guessing.
fn compare_pack_versions(
    candidate: &ParsedPackVersion,
    installed: &ParsedPackVersion,
) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (candidate, installed) {
        (ParsedPackVersion::Semver(candidate), ParsedPackVersion::Semver(installed)) => {
            Some(candidate.cmp_precedence(installed))
        }
        (
            ParsedPackVersion::NumericDotted(candidate),
            ParsedPackVersion::NumericDotted(installed),
        ) => {
            let len = candidate.len().max(installed.len());
            for index in 0..len {
                let left = candidate.get(index).copied().unwrap_or(0);
                let right = installed.get(index).copied().unwrap_or(0);
                match left.cmp(&right) {
                    Ordering::Equal => {}
                    other => return Some(other),
                }
            }
            Some(Ordering::Equal)
        }
        _ => None,
    }
}

/// Compare two provider version strings without publication dates. A version
/// is only "newer" when both sides use the same proven grammar; otherwise the
/// caller must refuse an automatic update.
fn candidate_is_newer(candidate: &str, installed: &str) -> VersionOrder {
    match (parse_pack_version(candidate), parse_pack_version(installed)) {
        (Some(candidate), Some(installed)) => match compare_pack_versions(&candidate, &installed) {
            Some(std::cmp::Ordering::Greater) => VersionOrder::Newer,
            Some(_) => VersionOrder::NotNewer,
            None => VersionOrder::Unknown,
        },
        _ => VersionOrder::Unknown,
    }
}

/// Pick the newest CurseForge file in the installed channel that is strictly
/// newer by SemVer. Files from other channels are never candidates, so a
/// beta/alpha install cannot silently become a release (or the reverse), and
/// an older release can never replace a newer beta. Publication dates are
/// deliberately ignored: a later upload of an older version is not an update.
///
/// CurseForge exposes no structured pack-version field. Real `displayName`
/// values look like `RLCraft 1.12.2 - Release v2.9.3.zip`,
/// `All the Mods 9-1.1.1` or `Better MC [FABRIC] 1.19.2 v20`. Extracting "the
/// first number" would often pick the Minecraft version or the pack series
/// number, and comparing those could install a build for another game
/// version. Unknown order therefore stays manual; the UI explains why.
fn curseforge_update_decision(
    installed: &CfFile,
    files: &[CfFile],
    stored_channel: &str,
) -> PackCandidate<CfFile> {
    let detected = packlock::PackChannel::from_curseforge(installed.release_type);
    let Some(channel) = resolve_installed_channel(stored_channel, detected) else {
        return PackCandidate::UnknownChannel;
    };
    let candidates: Vec<&CfFile> = files
        .iter()
        .filter(|file| file.id != installed.id)
        .filter(|file| packlock::PackChannel::from_curseforge(file.release_type) == Some(channel))
        .collect();
    if candidates.is_empty() {
        return PackCandidate::UpToDate;
    }
    if parse_pack_version(&installed.display_name).is_none() {
        return PackCandidate::UnknownVersionOrder;
    }
    let mut best: Option<(&CfFile, ParsedPackVersion)> = None;
    let mut saw_unknown = false;
    for file in candidates {
        match candidate_is_newer(&file.display_name, &installed.display_name) {
            VersionOrder::Newer => {
                let version = parse_pack_version(&file.display_name)
                    .expect("VersionOrder::Newer implies both versions parse");
                if best.as_ref().is_none_or(|(_, best_version)| {
                    compare_pack_versions(&version, best_version)
                        == Some(std::cmp::Ordering::Greater)
                }) {
                    best = Some((file, version));
                }
            }
            VersionOrder::NotNewer => {}
            VersionOrder::Unknown => saw_unknown = true,
        }
    }
    match best {
        Some((file, _)) => PackCandidate::Newer(file.clone()),
        None if saw_unknown => PackCandidate::UnknownVersionOrder,
        None => PackCandidate::UpToDate,
    }
}

/// Modrinth counterpart of [`curseforge_update_decision`].
fn modrinth_update_decision(
    installed: &modrinth::ModrinthVersionResponse,
    versions: &[ModrinthVersion],
    stored_channel: &str,
) -> PackCandidate<ModrinthVersion> {
    let detected = packlock::PackChannel::from_modrinth(&installed.version_type);
    let Some(channel) = resolve_installed_channel(stored_channel, detected) else {
        return PackCandidate::UnknownChannel;
    };
    let candidates: Vec<&ModrinthVersion> = versions
        .iter()
        .filter(|version| version.id != installed.id)
        .filter(|version| {
            packlock::PackChannel::from_modrinth(&version.version_type) == Some(channel)
        })
        .collect();
    if candidates.is_empty() {
        return PackCandidate::UpToDate;
    }
    if parse_pack_version(&installed.version_number).is_none() {
        return PackCandidate::UnknownVersionOrder;
    }
    let mut best: Option<(&ModrinthVersion, ParsedPackVersion)> = None;
    let mut saw_unknown = false;
    for version in candidates {
        match candidate_is_newer(&version.version_number, &installed.version_number) {
            VersionOrder::Newer => {
                let parsed = parse_pack_version(&version.version_number)
                    .expect("VersionOrder::Newer implies both versions parse");
                if best.as_ref().is_none_or(|(_, best_version)| {
                    compare_pack_versions(&parsed, best_version)
                        == Some(std::cmp::Ordering::Greater)
                }) {
                    best = Some((version, parsed));
                }
            }
            VersionOrder::NotNewer => {}
            VersionOrder::Unknown => saw_unknown = true,
        }
    }
    match best {
        Some((version, _)) => PackCandidate::Newer(version.clone()),
        None if saw_unknown => PackCandidate::UnknownVersionOrder,
        None => PackCandidate::UpToDate,
    }
}

async fn curseforge_update_candidate(
    lock: &PackLock,
    api_key: &str,
) -> Result<PackCandidate<CfFile>, String> {
    let pid = parse_id(&lock.pack_project_id, "pack")?;
    let fid = parse_id(&lock.file_id, "file")?;
    let installed = match curseforge::get_mod_file(pid, fid, api_key).await {
        Ok(file) => file,
        // A deleted/hidden installed file leaves the channel unknowable.
        Err(_) => return Ok(PackCandidate::UnknownChannel),
    };
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
    files.sort_by(|a, b| b.file_date.cmp(&a.file_date));
    Ok(curseforge_update_decision(
        &installed,
        &files,
        &lock.channel,
    ))
}

async fn modrinth_update_candidate(
    lock: &PackLock,
) -> Result<PackCandidate<ModrinthVersion>, String> {
    let installed = match modrinth::get_version_by_id(&lock.file_id).await {
        Ok(version) => version,
        Err(_) => return Ok(PackCandidate::UnknownChannel),
    };
    let mut versions = modrinth::get_versions(
        &lock.pack_project_id,
        Some(&lock.mc_version),
        lock.loader_filter(),
    )
    .await
    .map_err(|e| format!("Modrinth version list failed: {}", e))?;
    versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
    Ok(modrinth_update_decision(
        &installed,
        &versions,
        &lock.channel,
    ))
}

fn parse_id(raw: &str, what: &str) -> Result<u64, String> {
    raw.parse::<u64>()
        .map_err(|_| format!("Invalid CurseForge {} id", what))
}

/// Overrides whose pack-shipped content changed since the lock was written.
///
/// Comparing pack hashes (not the files on disk) means a user-edited config
/// survives updates as long as the pack did not change that file. Locks from
/// older builds have no hashes, so they fall back to rewriting every override
/// once; the update then records hashes for subsequent runs.
fn changed_override_paths(
    lock: &PackLock,
    new_overrides: &[String],
    new_hashes: &BTreeMap<String, String>,
) -> Vec<String> {
    if lock.override_hashes.is_empty() {
        return new_overrides.to_vec();
    }
    new_overrides
        .iter()
        .filter(|relative| lock.override_hashes.get(*relative) != new_hashes.get(*relative))
        .cloned()
        .collect()
}

/// Maximum size of an override file we are willing to hash while deciding
/// whether a legacy lock's local copy is a user edit. Larger files are treated
/// as possibly user-owned (never overwritten ambiguously).
const MAX_AMBIGUOUS_OVERRIDE_HASH_BYTES: u64 = 64 * 1024 * 1024;

/// With a lock written before `override_hashes` existed there is no way to
/// tell whether a local file that differs from the new pack is an old pack
/// version or a user edit. Keep the user's file, record that it was skipped,
/// and let a later repair explicitly restore the pack copy. New or absent
/// files are still installed normally.
fn preserve_ambiguous_overrides(
    mc_dir: &Path,
    lock: &PackLock,
    candidates: Vec<String>,
    new_hashes: &BTreeMap<String, String>,
) -> (Vec<String>, usize) {
    if !lock.override_hashes.is_empty() {
        return (candidates, 0);
    }
    let mut install = Vec::new();
    let mut skipped = 0usize;
    for rel in candidates {
        let dest = mc_dir.join(&rel);
        if !dest.exists() {
            install.push(rel);
            continue;
        }
        let small_enough = std::fs::metadata(&dest)
            .map(|m| m.len() <= MAX_AMBIGUOUS_OVERRIDE_HASH_BYTES)
            .unwrap_or(false);
        let matches_new = small_enough
            && new_hashes
                .get(&rel)
                .is_some_and(|hash| crate::download::verify_sha1(&dest, hash).ok() == Some(true));
        if matches_new {
            continue;
        }
        skipped += 1;
        tracing::warn!(
            target: "launcher",
            "Keeping existing override {} instead of overwriting it: the pack lock has no hashes, so a user edit cannot be distinguished from an older pack copy",
            rel
        );
    }
    (install, skipped)
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

fn restore_sidecar(snap: &SidecarSnapshot) -> Result<(), String> {
    let mut errors = Vec::new();
    let base = snap.dir.parent().unwrap_or(&snap.dir);
    let meta_path = instances::sidecar_meta_path(&snap.dir, &snap.filename);
    if let Err(e) = crate::import::ensure_no_reparse_points(base, &meta_path) {
        errors.push(format!(
            "cannot safely restore {}: {}",
            meta_path.display(),
            e
        ));
    } else {
        match &snap.meta {
            Some(text) => {
                if let Err(e) = std::fs::write(&meta_path, text) {
                    errors.push(format!("cannot restore {}: {}", meta_path.display(), e));
                }
            }
            None => match std::fs::remove_file(&meta_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => errors.push(format!("cannot remove {}: {}", meta_path.display(), e)),
            },
        }
    }
    let legacy_path = instances::legacy_sidecar_path(&snap.dir, &snap.filename);
    if let Err(e) = crate::import::ensure_no_reparse_points(base, &legacy_path) {
        errors.push(format!(
            "cannot safely restore {}: {}",
            legacy_path.display(),
            e
        ));
    } else {
        match &snap.legacy {
            Some(text) => {
                if let Err(e) = std::fs::write(&legacy_path, text) {
                    errors.push(format!("cannot restore {}: {}", legacy_path.display(), e));
                }
            }
            None => match std::fs::remove_file(&legacy_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => errors.push(format!("cannot remove {}: {}", legacy_path.display(), e)),
            },
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only fault injection: make the next rollback fail before it
    /// touches anything, so the "rollback failed" path can be exercised.
    static FAIL_NEXT_ROLLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Test-only fault injection: fail the next sidecar write after the
    /// content file has already been moved into place.
    static FAIL_NEXT_SIDECAR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_next_rollback() {
    FAIL_NEXT_ROLLBACK.with(|flag| flag.set(true));
}

#[cfg(test)]
fn fail_next_sidecar() {
    FAIL_NEXT_SIDECAR.with(|flag| flag.set(true));
}

#[derive(Default)]
struct ApplyRollback {
    /// Files the apply phase moved into the instance (deleted first so the
    /// parked originals can go back to the very same paths).
    moved: Vec<PathBuf>,
    /// Parked originals: (original path, parked path).
    stashes: Vec<(PathBuf, PathBuf)>,
    sidecars: Vec<SidecarSnapshot>,
    /// Directories the apply phase created (removed again on rollback when
    /// they are empty).
    created_dirs: Vec<PathBuf>,
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

    /// Undo every applied operation in reverse order.
    ///
    /// Files installed by the operation are removed first, then the parked
    /// originals are put back, then the sidecars are restored. An operation
    /// that fails is kept in the journal (with its recovery data) so a later
    /// retry can finish it; staging is deleted only when every step succeeded.
    /// Returns all errors instead of hiding them: a partial rollback is a
    /// partial instance and must never be reported as a successful rollback.
    fn rollback(&mut self, staging: &Path, temp_zip: &Path) -> Result<(), String> {
        #[cfg(test)]
        if FAIL_NEXT_ROLLBACK.with(|flag| flag.replace(false)) {
            // Simulate a rollback that cannot undo anything: every journal
            // entry and every on-disk recovery file stays intact.
            let _ = self.preserve_recovery(staging);
            return Err("injected rollback failure (test)".to_string());
        }

        let mut errors = Vec::new();

        while let Some(dest) = self.moved.pop() {
            match std::fs::remove_file(&dest) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    errors.push(format!("cannot remove {}: {}", dest.display(), e));
                    self.moved.push(dest);
                }
            }
        }

        while let Some((orig, park)) = self.stashes.pop() {
            if !park.exists() {
                errors.push(format!(
                    "recovery file is missing: {} (needed for {})",
                    park.display(),
                    orig.display()
                ));
                self.stashes.push((orig, park));
                continue;
            }
            if orig.exists() {
                if let Err(e) = std::fs::remove_file(&orig) {
                    errors.push(format!("cannot clear {}: {}", orig.display(), e));
                    self.stashes.push((orig, park));
                    continue;
                }
            }
            if let Err(e) = std::fs::rename(&park, &orig) {
                errors.push(format!(
                    "cannot restore {} from {}: {}",
                    orig.display(),
                    park.display(),
                    e
                ));
                self.stashes.push((orig, park));
            }
        }

        while let Some(snap) = self.sidecars.pop() {
            if let Err(e) = restore_sidecar(&snap) {
                errors.push(e);
                self.sidecars.push(snap);
            }
        }

        // Best effort: a directory the apply created is removed only when it
        // is empty. A non-empty one contains restored/user content and stays.
        while let Some(dir) = self.created_dirs.pop() {
            let _ = std::fs::remove_dir(&dir);
        }

        if !errors.is_empty() {
            // Recovery data must survive both this process and the content
            // listing's stale-park cleanup (which deletes parks whose original
            // name is occupied again). Copy the remaining parks and sidecar
            // snapshots into staging before reporting the failure.
            match self.preserve_recovery(staging) {
                Ok(path) => errors.push(format!("recovery data was saved to '{}'", path.display())),
                Err(e) => errors.push(format!("recovery data could not be saved: {}", e)),
            }
            return Err(errors.join("; "));
        }

        let _ = std::fs::remove_dir_all(staging);
        let _ = std::fs::remove_file(temp_zip);
        Ok(())
    }

    /// Copy everything a manual recovery still needs into the staging folder.
    /// Parks are copied, not moved: the in-place originals stay available for
    /// an immediate retry. The manifest names every original path and every
    /// new file the failed operation left behind.
    fn preserve_recovery(&self, staging: &Path) -> Result<PathBuf, String> {
        let recovery_dir = staging.join("recovery");
        std::fs::create_dir_all(&recovery_dir)
            .map_err(|e| format!("cannot create {}: {}", recovery_dir.display(), e))?;
        let mut lines = Vec::new();
        let mut copy_no = 0usize;

        for (orig, park) in &self.stashes {
            if !park.exists() {
                lines.push(format!(
                    "missing backup: {} (original: {})",
                    park.display(),
                    orig.display()
                ));
                continue;
            }
            copy_no += 1;
            let name = park
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("parked-file");
            let target = recovery_dir.join(format!("{:04}-{}", copy_no, name));
            std::fs::copy(park, &target)
                .map_err(|e| format!("cannot copy recovery file {}: {}", park.display(), e))?;
            lines.push(format!(
                "original: {} | parked: {} | backup: {}",
                orig.display(),
                park.display(),
                target.display()
            ));
        }

        for dest in &self.moved {
            lines.push(format!(
                "installed by the failed operation: {}",
                dest.display()
            ));
        }

        for (index, snap) in self.sidecars.iter().enumerate() {
            let stem = std::path::Path::new(&snap.filename)
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("content");
            for (kind, text) in [("meta", &snap.meta), ("legacy", &snap.legacy)] {
                let Some(text) = text else { continue };
                let target = recovery_dir.join(format!(
                    "{:04}-{:04}-{}-{}.json",
                    copy_no, index, stem, kind
                ));
                std::fs::write(&target, text).map_err(|e| {
                    format!("cannot write recovery sidecar {}: {}", target.display(), e)
                })?;
                lines.push(format!(
                    "sidecar {} | {} | {}: {}",
                    snap.dir.display(),
                    snap.filename,
                    kind,
                    target.display()
                ));
            }
        }

        let manifest = staging.join("recovery.txt");
        std::fs::write(&manifest, lines.join("\n")).map_err(|e| {
            format!(
                "cannot write recovery manifest {}: {}",
                manifest.display(),
                e
            )
        })?;
        Ok(staging.to_path_buf())
    }
}

fn cleanup_files(staging: &Path, temp_zip: &Path) {
    let _ = std::fs::remove_dir_all(staging);
    let _ = std::fs::remove_file(temp_zip);
}

/// Stable per-instance token for temp archive names: two instances of the
/// same pack must not share (and delete) one download, while a retry of the
/// same instance can still resume its `.part` file.
fn instance_temp_token(instance_name: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    instance_name.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Cheap process-local uniqueness for staging and temp names (two instances
/// of the same pack may be updated concurrently).
fn unique_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

static OPERATION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Hidden, per-operation staging directory beside the instance. A unique name
/// means a staging folder kept after a failed rollback (recovery data) is
/// never silently deleted by the next update or repair.
fn operation_staging_dir(instance_dir: &Path, tag: &str) -> PathBuf {
    let counter = OPERATION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    instance_dir.join(format!(
        ".{}-{}-{}-{}",
        tag,
        std::process::id(),
        unique_nonce(),
        counter
    ))
}

// ==================== Apply plan (shared by both providers) ====================

/// Owned form of [`SidecarWrite`] for a plan built before the apply phase.
#[derive(Debug)]
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
#[derive(Debug)]
enum SidecarPlan {
    /// Full metadata for a file listed by the pack manifest.
    Content(SidecarWriteOwned),
    /// Minimal override metadata (only content folders get one; configs are
    /// left without a sidecar by `write_override_sidecar`).
    Override(&'static str),
}

/// One file the apply phase installs.
#[derive(Debug)]
struct PlannedInstall {
    /// Staged file, already downloaded/extracted beside the instance.
    staged: PathBuf,
    /// Destination in the instance, without a `.disabled` suffix.
    dest: PathBuf,
    /// The replaced file was disabled: the new file must stay disabled.
    preserve_disabled: bool,
    /// Repair only installs missing content: if something appeared at the
    /// destination after the prepare phase, fail instead of replacing it.
    expect_absent: bool,
    /// Old file name whose sidecar is dropped when the stem changed.
    previous_name: Option<String>,
    sidecar: SidecarPlan,
    /// Manifest content (`true`) vs pack override (`false`), for the report.
    is_mod: bool,
}

/// One file the apply phase parks (removed pack content).
#[derive(Debug)]
struct PlannedRemoval {
    path: PathBuf,
    /// Manifest content (`true`) vs pack override (`false`).
    is_mod: bool,
    /// `true` when this removal is counted as a mod/override removal in the
    /// report (path changes of updated files are parked but not counted).
    counted: bool,
}

/// Everything the apply phase will do, resolved before any mutation.
#[derive(Debug, Default)]
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

/// Sidecar identity of a content file: two files with the same stem in one
/// content folder (e.g. `Foo.jar` and `Foo.zip`) share one metadata file, so
/// installing both would silently overwrite the first one's metadata.
fn sidecar_key(path: &Path, mc_dir: &Path) -> Option<String> {
    if !in_content_dir(path, mc_dir) {
        return None;
    }
    let parent = path.parent()?;
    let name = path.file_name()?.to_str()?;
    Some(format!(
        "{}/{}",
        parent.to_string_lossy().replace('\\', "/").to_lowercase(),
        instances::content_stem(name).to_lowercase()
    ))
}

/// Reject a plan that would silently lose content: duplicate destinations,
/// a file that is also an ancestor directory of another install, or two files
/// sharing one sidecar. Update plans already resolve intentional
/// shadowing (overrides on top of manifest files) before this check.
fn validate_apply_plan(plan: &ApplyPlan, mc_dir: &Path) -> Result<(), String> {
    let mut destinations: Vec<(String, &Path)> = Vec::with_capacity(plan.installs.len());
    let mut sidecars: HashMap<String, &Path> = HashMap::new();
    for install in &plan.installs {
        let key = path_key(&install.dest, mc_dir);
        for (existing_key, existing) in &destinations {
            if *existing_key == key {
                return Err(format!(
                    "Conflicting destinations in one operation: {} and {}",
                    existing.display(),
                    install.dest.display()
                ));
            }
            if key.starts_with(&format!("{}/", existing_key))
                || existing_key.starts_with(&format!("{}/", key))
            {
                return Err(format!(
                    "A file and a directory collide in one operation: {} and {}",
                    existing.display(),
                    install.dest.display()
                ));
            }
        }
        if let Some(sidecar) = sidecar_key(&install.dest, mc_dir) {
            if let Some(previous) = sidecars.insert(sidecar, &install.dest) {
                return Err(format!(
                    "Two files share one metadata entry: {} and {}",
                    previous.display(),
                    install.dest.display()
                ));
            }
        }
        destinations.push((key, &install.dest));
    }
    Ok(())
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

/// Is any on-disk variant of `path` present, including its `.disabled` twin?
fn content_present(path: &Path) -> bool {
    if path.exists() {
        return true;
    }
    path.parent()
        .zip(path.file_name().and_then(|n| n.to_str()))
        .map(|(parent, name)| is_disabled_at(parent, name))
        .unwrap_or(false)
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
fn park_occupied(rb: &mut ApplyRollback, parent: &Path, name: &str) -> Result<bool, String> {
    let disabled_name = format!("{}.disabled", name);
    let disabled_exists = parent.join(&disabled_name).is_file();
    if !disabled_exists && !parent.join(name).is_file() {
        return Ok(false);
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
    Ok(true)
}

/// Create `dir` and any missing ancestors, remembering which directories did
/// not exist so a rollback can remove them again when they end up empty.
fn ensure_dir_tracked(rb: &mut ApplyRollback, dir: &Path) -> std::io::Result<()> {
    let mut missing = Vec::new();
    let mut current = dir;
    while !current.exists() {
        missing.push(current.to_path_buf());
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    for path in missing.into_iter().rev() {
        match std::fs::create_dir(&path) {
            Ok(()) => rb.created_dirs.push(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
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
    let rel = install
        .dest
        .strip_prefix(mc_dir)
        .map_err(|_| {
            format!(
                "Destination outside the instance: {}",
                install.dest.display()
            )
        })?
        .to_string_lossy()
        .replace('\\', "/");
    // Final boundary check: a plan can be built by several code paths and
    // must never be able to write a user-owned file even if an earlier check
    // is bypassed or a path alias is used.
    if packlock::is_protected_path(&rel) {
        return Err(format!(
            "Refusing to write protected path '{}' during a pack update",
            rel
        ));
    }
    // Lexical containment is not enough: `mods` (or any parent below the
    // instance) can be a junction/symlink to another directory. Reject before
    // parking or renaming anything.
    crate::import::ensure_no_reparse_points(mc_dir, &install.dest)
        .map_err(|e| format!("Unsafe destination {}: {}", install.dest.display(), e))?;
    // Repair only restores files that are known to be missing. Re-check right
    // before the mutation: if another process/user created the file after the
    // prepare phase, replacing it would destroy data that is not ours.
    if install.expect_absent && existing_content_path(&install.dest).is_some() {
        return Err(format!(
            "{} appeared while the repair was preparing; refusing to replace it",
            install.dest.display()
        ));
    }
    // Snapshot metadata before any mutation, including for brand-new files:
    // a rollback must remove the sidecar this operation creates.
    rb.snapshot_sidecar(parent, name);
    if let Some(previous) = &install.previous_name {
        if previous != name {
            rb.snapshot_sidecar(parent, previous);
        }
    }
    park_occupied(rb, parent, name)?;
    if let Err(e) = ensure_dir_tracked(rb, parent) {
        return Err(format!("Cannot create folder for {}: {}", name, e));
    }
    crate::import::ensure_no_reparse_points(mc_dir, &install.dest)
        .map_err(|e| format!("Unsafe destination {}: {}", install.dest.display(), e))?;
    let final_dest = if install.preserve_disabled {
        parent.join(format!("{}.disabled", name))
    } else {
        install.dest.clone()
    };
    if let Err(e) = std::fs::rename(&install.staged, &final_dest) {
        return Err(format!("Cannot install {}: {}", name, e));
    }
    rb.moved.push(final_dest);
    #[cfg(test)]
    if FAIL_NEXT_SIDECAR.with(|flag| flag.replace(false)) {
        return Err("injected sidecar failure (test)".to_string());
    }
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
            let sidecar = instances::sidecar_meta_path(parent, name);
            crate::import::ensure_no_reparse_points(mc_dir, &sidecar)
                .map_err(|e| format!("Unsafe metadata path {}: {}", sidecar.display(), e))?;
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

fn remove_one(rb: &mut ApplyRollback, mc_dir: &Path, path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Ok(());
    };
    let rel = path
        .strip_prefix(mc_dir)
        .map_err(|_| format!("Removal outside the instance: {}", path.display()))?
        .to_string_lossy()
        .replace('\\', "/");
    // Same final boundary as install_one: never delete a user-owned file,
    // regardless of how the removal plan was assembled.
    if packlock::is_protected_path(&rel) {
        return Err(format!(
            "Refusing to remove protected path '{}' during a pack update",
            rel
        ));
    }
    crate::import::ensure_no_reparse_points(mc_dir, path)
        .map_err(|e| format!("Unsafe removal {}: {}", path.display(), e))?;
    rb.snapshot_sidecar(parent, name);
    let stashed = instances::stash_old_content_files(parent, name).map_err(|e| e.to_string())?;
    rb.stashes.extend(stashed);
    Ok(())
}

fn apply_ops(
    rb: &mut ApplyRollback,
    mc_dir: &Path,
    plan: &ApplyPlan,
) -> Result<ApplyOutcome, String> {
    let mut outcome = ApplyOutcome::default();
    for install in &plan.installs {
        install_one(rb, mc_dir, install)?;
        if install.is_mod {
            outcome.mods_installed += 1;
        } else {
            outcome.overrides_written += 1;
        }
    }
    for removal in &plan.removals {
        remove_one(rb, mc_dir, &removal.path)?;
        if removal.counted {
            if removal.is_mod {
                outcome.mods_removed += 1;
            } else {
                outcome.overrides_removed += 1;
            }
        }
    }
    Ok(outcome)
}

/// Turn a failed apply into the error returned to the UI. The original cause is
/// always preserved. If the rollback also failed, both errors are reported and
/// the recovery staging is intentionally left on disk.
fn apply_failure_message(
    what: &str,
    cause: String,
    rb: &mut ApplyRollback,
    staging: &Path,
    temp_zip: &Path,
) -> String {
    match rb.rollback(staging, temp_zip) {
        Ok(()) => format!("{}: {}. The instance was rolled back.", what, cause),
        Err(rollback) => format!(
            "{}: {}. Rollback also failed: {}. The instance may be partially modified \
             and may need manual recovery.",
            what, cause, rollback
        ),
    }
}

/// Apply a prepared plan without writing a pack lock (repair). On success the
/// parks are discarded and staging is removed; any failure rolls the whole
/// plan back.
fn apply_plan(
    mc_dir: &Path,
    plan: &ApplyPlan,
    staging: &Path,
    temp_zip: &Path,
) -> Result<ApplyOutcome, String> {
    if let Err(e) = validate_apply_plan(plan, mc_dir) {
        cleanup_files(staging, temp_zip);
        return Err(format!(
            "Repair plan rejected, the instance was not modified: {}",
            e
        ));
    }
    let mut rb = ApplyRollback::default();
    match apply_ops(&mut rb, mc_dir, plan) {
        Ok(outcome) => {
            instances::discard_stashed_files(&rb.stashes);
            cleanup_files(staging, temp_zip);
            Ok(outcome)
        }
        Err(e) => Err(apply_failure_message(
            "Repair failed before any change could be committed",
            e,
            &mut rb,
            staging,
            temp_zip,
        )),
    }
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
    if let Err(e) = validate_apply_plan(plan, mc_dir) {
        cleanup_files(staging, temp_zip);
        return Err(format!(
            "Update plan rejected, the instance was not modified: {}",
            e
        ));
    }
    let mut rb = ApplyRollback::default();
    let outcome = match apply_ops(&mut rb, mc_dir, plan) {
        Ok(outcome) => outcome,
        Err(e) => {
            return Err(apply_failure_message(
                "Update failed",
                e,
                &mut rb,
                staging,
                temp_zip,
            ))
        }
    };
    if let Err(e) = new_lock.write(instance_dir) {
        return Err(apply_failure_message(
            "Cannot save pack metadata",
            e.to_string(),
            &mut rb,
            staging,
            temp_zip,
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
            expect_absent: false,
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
    classes: &'a HashMap<String, crate::curseforge::CfContentClass>,
    diff: &'a PackDiff,
    new_overrides: &'a [String],
    old_overrides: &'a [String],
}

/// Resolve the CurseForge apply plan. Manifest content is matched by sidecar
/// identity; a disabled installed file makes the replacement disabled too.
/// Project class decides the destination folder and the sidecar type.
fn plan_curseforge_update(
    mc_dir: &Path,
    staged_mods_dir: &Path,
    staged_overrides_dir: &Path,
    input: &CfApplyInput<'_>,
) -> ApplyPlan {
    let mut plan = ApplyPlan::default();

    for (r, cf_file) in input.staged_mods {
        let class = input
            .classes
            .get(&r.project_id)
            .copied()
            .unwrap_or(crate::curseforge::CfContentClass::Mod);
        let dest_dir = cf_destination_dir(mc_dir, class);
        let old = input.installed.iter().find(|i| {
            i.provider == "curseforge" && i.project_id.as_deref() == Some(r.project_id.as_str())
        });
        let preserve_disabled = old
            .map(|o| o.filename.ends_with(".disabled"))
            .unwrap_or(false);
        let mut previous_name = None;
        if let Some(old) = old {
            let old_dir = cf_destination_dir(mc_dir, cf_content_class_for_type(&old.content_type));
            // A disabled file carries the `.disabled` suffix, so compare the
            // canonical target (case-folded, suffix stripped) instead of the
            // raw names: `sodium.jar` and `sodium.jar.disabled` are the same
            // target, and the install already parks the old file.
            let same_target = path_key(&old_dir.join(&old.filename), mc_dir)
                == path_key(&dest_dir.join(&cf_file.file_name), mc_dir);
            if !same_target {
                // The replacement has a new file name (or moved to another
                // content class): park the old file as well.
                plan.removals.push(PlannedRemoval {
                    path: old_dir.join(&old.filename),
                    is_mod: true,
                    counted: false,
                });
                previous_name = Some(old.filename.clone());
            }
        }
        plan.installs.push(PlannedInstall {
            staged: cf_staged_path(staged_mods_dir, class, &cf_file.file_name),
            dest: dest_dir.join(&cf_file.file_name),
            preserve_disabled,
            expect_absent: false,
            previous_name,
            sidecar: SidecarPlan::Content(SidecarWriteOwned {
                provider: "curseforge".to_string(),
                content_type: class.content_type().to_string(),
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
        let old_dir = cf_destination_dir(mc_dir, cf_content_class_for_type(&old.content_type));
        plan.removals.push(PlannedRemoval {
            path: old_dir.join(&old.filename),
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

/// Manifest paths are untrusted and both the update and repair paths must
/// reject user-owned files before anything is staged or written. The check
/// is fatal: silently skipping a protected entry would install only part of
/// the pack while claiming success.
fn check_manifest_entry_path(rel: &str) -> Result<(), String> {
    if rel.is_empty() {
        return Err("The Modrinth pack manifest contains an empty file path.".to_string());
    }
    check_safe_relative(rel)
        .map_err(|e| format!("Unsafe path in the Modrinth pack manifest: {e}"))?;
    if packlock::is_protected_path(rel) {
        return Err(format!(
            "The Modrinth pack manifest targets the protected user file '{}'. \
             The operation was stopped before changing any files.",
            rel
        ));
    }
    Ok(())
}

/// Repair only restores missing files, so it only needs to reject manifest
/// paths that would touch user-owned files. Structurally unsafe paths are
/// skipped by the repair loop (they cannot name an existing destination).
fn check_repair_manifest_paths(map: &BTreeMap<String, serde_json::Value>) -> Result<(), String> {
    for entry in map.values() {
        let path = packlock::mrpack_entry_path(entry);
        if path.is_empty() || check_safe_relative(&path).is_err() {
            continue;
        }
        if packlock::is_protected_path(&path) {
            return Err(format!(
                "The Modrinth pack manifest targets the protected user file '{}'. \
                 Repair was stopped before changing any files.",
                path
            ));
        }
    }
    Ok(())
}

/// Resolve the Modrinth apply plan. A path that changes is parked under its
/// old name; the disabled state of the replaced file is carried over.
fn plan_modrinth_update(
    mc_dir: &Path,
    staging: &Path,
    input: &MrApplyInput<'_>,
) -> Result<ApplyPlan, String> {
    let mut plan = ApplyPlan::default();

    for r in input.diff.added.iter().chain(input.diff.updated.iter()) {
        let Some(new_entry) = input.new_map.get(&r.project_id) else {
            continue;
        };
        let new_path = packlock::mrpack_entry_path(new_entry);
        check_manifest_entry_path(&new_path)?;
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
            expect_absent: false,
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
    Ok(plan)
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
    /// The installed channel could not be established or contradicts the
    /// lock. Automatic updates are intentionally disabled in this state.
    pub channel_unknown: bool,
    /// Provider version strings cannot be ordered reliably. Automatic updates
    /// are intentionally disabled; a later publication date is not a version
    /// order.
    pub version_order_unknown: bool,
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
            let candidate = curseforge_update_candidate(&lock, &ctx.api_key).await?;
            let (
                up_to_date,
                channel_unknown,
                version_order_unknown,
                latest_file_id,
                latest_version,
                latest_file_name,
            ) = match candidate {
                PackCandidate::Newer(file) => (
                    false,
                    false,
                    false,
                    Some(file.id.to_string()),
                    Some(file.display_name),
                    Some(file.file_name),
                ),
                PackCandidate::UpToDate => (
                    true,
                    false,
                    false,
                    Some(lock.file_id.clone()),
                    Some(lock.file_display_name.clone()),
                    Some(lock.file_name.clone()),
                ),
                PackCandidate::UnknownChannel => (true, true, false, None, None, None),
                PackCandidate::UnknownVersionOrder => (true, false, true, None, None, None),
            };
            Ok(PackUpdateInfo {
                up_to_date,
                channel_unknown,
                version_order_unknown,
                current_file_id: lock.file_id.clone(),
                current_version: lock.file_display_name.clone(),
                latest_file_id,
                latest_version,
                latest_file_name,
            })
        }
        "modrinth" => {
            let candidate = modrinth_update_candidate(&lock).await?;
            let (
                up_to_date,
                channel_unknown,
                version_order_unknown,
                latest_file_id,
                latest_version,
                latest_file_name,
            ) = match candidate {
                PackCandidate::Newer(version) => {
                    let primary = version
                        .files
                        .iter()
                        .find(|f| f.primary)
                        .or_else(|| version.files.first());
                    (
                        false,
                        false,
                        false,
                        Some(version.id),
                        Some(version.version_number),
                        primary.map(|file| file.filename.clone()),
                    )
                }
                PackCandidate::UpToDate => (
                    true,
                    false,
                    false,
                    Some(lock.file_id.clone()),
                    Some(lock.file_display_name.clone()),
                    Some(lock.file_name.clone()),
                ),
                PackCandidate::UnknownChannel => (true, true, false, None, None, None),
                PackCandidate::UnknownVersionOrder => (true, false, true, None, None, None),
            };
            Ok(PackUpdateInfo {
                up_to_date,
                channel_unknown,
                version_order_unknown,
                current_file_id: lock.file_id.clone(),
                current_version: lock.file_display_name.clone(),
                latest_file_id,
                latest_version,
                latest_file_name,
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
    /// Existing files kept because a legacy lock has no hashes to prove they
    /// were pack-owned (overwriting could have destroyed user edits).
    #[serde(default)]
    pub overrides_skipped: usize,
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
    validate_instance_name(&instance_name)?;
    let _guard =
        crate::commands::instances::InstanceMutationGuard::acquire(state.inner(), &instance_name)?;
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
/// (mods by identity, overrides only when the file is gone). Everything is
/// downloaded/extracted into staging first; the instance is modified only by
/// the shared apply/rollback path. Never deletes unrelated files and never
/// rewrites the lock.
#[tauri::command]
pub async fn cmd_repair_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<PackRepairReport, String> {
    let _pause_scope = crate::download::PausableInstallScope::begin();
    validate_instance_name(&instance_name)?;
    let _guard =
        crate::commands::instances::InstanceMutationGuard::acquire(state.inner(), &instance_name)?;
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
    let latest = match curseforge_update_candidate(&lock, &ctx.api_key).await? {
        PackCandidate::Newer(file) => file,
        PackCandidate::UpToDate => {
            return Err(
                "No newer pack version is available in the installed release channel.".to_string(),
            )
        }
        PackCandidate::UnknownChannel => {
            return Err(
                "Cannot determine the release channel of the installed pack. \
                 Automatic updates are disabled to avoid a downgrade or a silent channel change; \
                 reinstall the pack to switch channels."
                    .to_string(),
            )
        }
        PackCandidate::UnknownVersionOrder => {
            return Err(
                "Cannot determine whether the available pack version is newer than the installed one. \
                 Automatic updates are disabled to prevent a downgrade; install a chosen version manually."
                    .to_string(),
            )
        }
    };

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
        "voidlauncher-cfpack-{}-{}-{}.zip",
        safe_instance,
        instance_temp_token(&ctx.instance.name),
        latest.id
    ));
    if let Err(e) =
        curseforge::download_mod_file(pack_pid, latest.id, &file, &ctx.api_key, &temp_zip).await
    {
        let _ = std::fs::remove_file(&temp_zip);
        return Err(format!("Pack download failed: {}", e));
    }

    let staging = operation_staging_dir(&ctx.instance_dir, "pack-update");
    let (new_manifest, new_overrides, new_hashes) = match packlock::pack_zip_info(&temp_zip) {
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
    let (changed_overrides, overrides_skipped) = preserve_ambiguous_overrides(
        &ctx.mc_dir,
        &lock,
        changed_override_paths(&lock, &new_overrides, &new_hashes),
        &new_hashes,
    );
    let overrides_changed =
        new_overrides != lock.overrides || !changed_overrides.is_empty() || overrides_skipped > 0;
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
    // Resolve every target's real project class before any download. If a
    // project cannot be classified, stop with the instance untouched rather
    // than silently installing resource/shader packs into mods/.
    let class_ids: Vec<String> = targets.iter().map(|r| r.project_id.clone()).collect();
    let classes = match cf_project_classes(&class_ids, &ctx.api_key).await {
        Ok(classes) => classes,
        Err(e) => {
            cleanup_files(&staging, &temp_zip);
            return Err(format!("Content classification failed: {e}"));
        }
    };
    for (i, r) in targets.iter().enumerate() {
        let class = classes
            .get(&r.project_id)
            .copied()
            .unwrap_or(crate::curseforge::CfContentClass::Mod);
        emit(
            app,
            "downloading-mods",
            i,
            total,
            &format!("Downloading {} {}/{}...", class.label(), i + 1, total),
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
            let dest = cf_staged_path(&stage_mods, class, &cf_file.file_name);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create staging folder: {}", e))?;
            }
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

    // ---- Phase 2: stage overrides whose pack content changed ----
    // Rewriting every override on every pack release would silently reset
    // user edits to configs/scripts the pack did not touch.
    let overrides_prefix =
        crate::import::curseforge_overrides_prefix(&new_manifest).map_err(|e| e.to_string())?;
    let stage_result: Result<(), String> = (|| {
        let file = std::fs::File::open(&temp_zip)
            .map_err(|e| format!("Cannot reopen pack archive: {}", e))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;
        let mut extract_budget = crate::import::ExtractBudget::new();
        crate::import::check_archive_entry_count(archive.len()).map_err(|e| e.to_string())?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("Cannot read pack entry: {}", e))?;
            let name = entry
                .name()
                .map(|name| name.into_owned())
                .map_err(|error| format!("Cannot read pack entry name: {error}"))?;
            let Some(rel) = name.strip_prefix(&overrides_prefix) else {
                continue;
            };
            if rel.is_empty() || entry.is_dir() {
                continue;
            }
            if !changed_overrides.iter().any(|o| o == rel) {
                continue;
            }
            if packlock::is_protected_path(rel) {
                continue;
            }
            check_safe_relative(rel).map_err(|e| e.to_string())?;
            write_extracted_budgeted(&stage_ov, rel, &mut entry, &mut extract_budget)
                .map_err(|e| e.to_string())?;
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
    let installed = list_cf_installed(&ctx.mc_dir);
    let plan = plan_curseforge_update(
        &ctx.mc_dir,
        &stage_mods,
        &stage_ov,
        &CfApplyInput {
            installed: &installed,
            staged_mods: &staged_mods,
            classes: &classes,
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
        channel: packlock::PackChannel::from_curseforge(latest.release_type)
            .map(|channel| channel.as_str().to_string())
            .unwrap_or_default(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        mc_version: new_mc.to_string(),
        loader: new_loader.clone(),
        manifest: new_manifest,
        overrides: new_overrides,
        overrides_prefix,
        override_hashes: new_hashes,
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
        overrides_skipped,
        new_version: latest.display_name.clone(),
    })
}

/// Override source layout inside a pack archive.
#[derive(Clone)]
enum OverridePrefix {
    /// Carries the actual `manifest.json` prefix (default `overrides/`).
    CurseForge(String),
}

impl OverridePrefix {
    fn relative<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        match self {
            Self::CurseForge(prefix) => name.strip_prefix(prefix.as_str()),
        }
    }
}

/// Extract only the requested (missing) overrides into staging and build the
/// install entries for them. The target instance is not touched here.
#[allow(clippy::too_many_arguments)]
fn stage_repair_overrides(
    zip_path: &Path,
    prefix: OverridePrefix,
    missing: &[String],
    staging_overrides: &Path,
    mc_dir: &Path,
    provider: &'static str,
    budget: &mut crate::import::ExtractBudget,
) -> Result<Vec<PlannedInstall>, String> {
    let file =
        std::fs::File::open(zip_path).map_err(|e| format!("Cannot open pack archive: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;
    crate::import::check_archive_entry_count(archive.len()).map_err(|e| e.to_string())?;
    let mut installs = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Cannot read pack entry: {}", e))?;
        let name = entry
            .name()
            .map(|name| name.into_owned())
            .map_err(|error| format!("Cannot read pack entry name: {error}"))?;
        let Some(rel) = prefix.relative(&name) else {
            continue;
        };
        if rel.is_empty() || entry.is_dir() || !missing.iter().any(|m| m == rel) {
            continue;
        }
        check_safe_relative(rel).map_err(|e| e.to_string())?;
        if packlock::is_protected_path(rel) {
            return Err(format!(
                "Refusing to stage protected path '{}' from the pack archive",
                rel
            ));
        }
        write_extracted_budgeted(staging_overrides, rel, &mut entry, budget)
            .map_err(|e| e.to_string())?;
        installs.push(PlannedInstall {
            staged: staging_overrides.join(rel),
            dest: mc_dir.join(rel),
            preserve_disabled: false,
            expect_absent: true,
            previous_name: None,
            sidecar: SidecarPlan::Override(provider),
            is_mod: false,
        });
    }
    Ok(installs)
}

/// Extract missing mrpack overrides with the documented layer precedence:
/// `overrides/` is the base layer and `client-overrides/` (or its
/// `overrides-client/` synonym) wins when both contain the same relative path.
///
/// Each layer gets its own path-claim set, seeded with the operation's
/// non-override claims (the index files), so:
/// - the two layers do not conflict with each other;
/// - both still reject a destination already claimed by an index file;
/// - bytes, files and path metadata share one operation-wide budget.
fn stage_mrpack_repair_overrides(
    zip_path: &Path,
    missing: &[String],
    staging_overrides: &Path,
    mc_dir: &Path,
    provider: &'static str,
    operation_budget: &mut crate::import::ExtractBudget,
) -> Result<Vec<PlannedInstall>, String> {
    let file =
        std::fs::File::open(zip_path).map_err(|e| format!("Cannot open pack archive: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;
    crate::import::check_archive_entry_count(archive.len()).map_err(|e| e.to_string())?;

    // Windows treats paths case-insensitively (and strips trailing dots/spaces),
    // so `config/A.toml` and `config/a.toml` are one destination. Key the
    // selection by the same normalized destination used by the extraction
    // budget: the client layer replaces the base entry, never a duplicate.
    let missing_keys: HashSet<String> = missing
        .iter()
        .filter_map(|rel| crate::import::destination_key(rel))
        .collect();
    let mut chosen: BTreeMap<String, (String, usize, bool)> = BTreeMap::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| format!("Cannot read pack entry: {}", e))?;
        let name = entry
            .name()
            .map(|name| name.into_owned())
            .map_err(|error| format!("Cannot read pack entry name: {error}"))?;
        let Some(rel) = crate::import::extract_mrpack_override(&name) else {
            continue;
        };
        if rel.is_empty() || entry.is_dir() {
            continue;
        }
        let Some(destination) = crate::import::destination_key(rel) else {
            return Err(format!("Invalid archive destination path: {rel}"));
        };
        if !missing_keys.contains(&destination) {
            continue;
        }
        check_safe_relative(rel).map_err(|e| e.to_string())?;
        if packlock::is_protected_path(rel) {
            return Err(format!(
                "Refusing to stage protected path '{}' from the pack archive",
                rel
            ));
        }
        let is_client =
            name.starts_with("client-overrides/") || name.starts_with("overrides-client/");
        match chosen
            .get(&destination)
            .map(|(_, index, client)| (*index, *client))
        {
            Some((_, true)) if is_client => {
                return Err(format!("Duplicate archive destination: {rel}"));
            }
            Some((_, false)) if !is_client => {
                return Err(format!("Duplicate archive destination: {rel}"));
            }
            Some((_, false)) if is_client => {
                chosen.insert(destination, (rel.to_string(), i, true));
            }
            Some(_) => {}
            None => {
                chosen.insert(destination, (rel.to_string(), i, is_client));
            }
        }
    }

    let mut base_budget = operation_budget.layer_with_existing_claims();
    let mut client_budget = operation_budget.layer_with_existing_claims();
    let mut installs = Vec::with_capacity(chosen.len());
    for (_, (rel, entry_index, is_client)) in chosen {
        let mut entry = archive
            .by_index(entry_index)
            .map_err(|e| format!("Cannot read pack entry: {}", e))?;
        let budget = if is_client {
            &mut client_budget
        } else {
            &mut base_budget
        };
        write_extracted_budgeted(staging_overrides, &rel, &mut entry, budget)
            .map_err(|e| e.to_string())?;
        installs.push(PlannedInstall {
            staged: staging_overrides.join(&rel),
            dest: mc_dir.join(&rel),
            preserve_disabled: false,
            expect_absent: true,
            previous_name: None,
            sidecar: SidecarPlan::Override(provider),
            is_mod: false,
        });
    }
    Ok(installs)
}

/// Unique temp archive for a repair. The instance name keeps two instances of
/// the same pack from sharing (and deleting) one download.
fn repair_temp_zip(tag: &str, instance_name: &str, file_id: &str, extension: &str) -> PathBuf {
    let safe_instance: String = instance_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    std::env::temp_dir().join(format!(
        "voidlauncher-{}-repair-{}-{}-{}.{}",
        tag,
        safe_instance,
        instance_temp_token(instance_name),
        file_id,
        extension
    ))
}

/// CF manifest repair: identity checks against the sidecar index, pack zip
/// re-download for missing overrides. Everything is downloaded/extracted into
/// a staging folder first; the instance is modified only by the shared
/// apply/rollback path.
async fn repair_curseforge(
    app: &AppHandle,
    ctx: &PackCtx,
    lock: PackLock,
) -> Result<PackRepairReport, String> {
    let installed = list_cf_installed(&ctx.mc_dir);
    let manifest_files = packlock::manifest_files(&lock.manifest);
    let total = manifest_files.len();
    let mut report = PackRepairReport {
        checked: 0,
        restored: 0,
        skipped: 0,
        failed: Vec::new(),
    };

    let staging = operation_staging_dir(&ctx.instance_dir, "pack-repair");
    let stage_mods = staging.join("mods");
    let stage_overrides = staging.join("overrides");
    let temp_zip = repair_temp_zip("cfpack", &ctx.instance.name, &lock.file_id, "zip");
    let mut plan = ApplyPlan::default();
    let mut extract_budget = crate::import::ExtractBudget::new();
    // Import/update/repair must agree on the pack's actual folder name, not a
    // hard-coded `overrides/`.
    let repair_overrides_prefix = lock.effective_curseforge_overrides_prefix();

    let prepare: Result<(), String> = async {
        std::fs::create_dir_all(&stage_mods)
            .map_err(|e| format!("Cannot create staging folder: {}", e))?;

        // Project class decides the destination folder. One bulk request keeps
        // repair from making a per-file metadata call (and from guessing).
        let project_ids: Vec<String> = manifest_files.keys().cloned().collect();
        let (projects, metadata_error) = match cf_project_metadata(&project_ids, &ctx.api_key).await
        {
            Ok(projects) => (projects, None),
            Err(e) => (Vec::new(), Some(e)),
        };

        // ---- Content listed by the manifest but missing/misclassified here ----
        for (i, (pid_str, fid_str)) in manifest_files.iter().enumerate() {
            report.checked += 1;
            let pid = pid_str
                .parse::<u64>()
                .map_err(|_| format!("Invalid CurseForge project id '{}'", pid_str))?;
            let fid = fid_str
                .parse::<u64>()
                .map_err(|_| format!("Invalid CurseForge file id '{}'", fid_str))?;
            let class = match curseforge::content_class_from_bulk(&projects, pid) {
                Some(class) => class,
                None => {
                    report.failed.push(format!(
                        "{}:{}: {}",
                        pid_str,
                        fid_str,
                        metadata_error.clone().unwrap_or_else(|| {
                            "CurseForge did not identify a supported content \
                                                      class (mod, resource pack or shader pack)"
                                .to_string()
                        })
                    ));
                    continue;
                }
            };
            let old = installed.iter().find(|ic| {
                ic.provider == "curseforge"
                    && ic.project_id.as_deref() == Some(pid_str.as_str())
                    && ic.file_id.as_deref() == Some(fid_str.as_str())
            });
            if let Some(old) = old {
                if old.content_type == class.content_type() {
                    report.skipped += 1;
                    continue;
                }
                // The file exists but in the wrong content folder (for example
                // a resource pack installed by an older build). Restore it to
                // its real class instead of leaving a broken pack.
                let old_dir =
                    cf_destination_dir(&ctx.mc_dir, cf_content_class_for_type(&old.content_type));
                plan.removals.push(PlannedRemoval {
                    path: old_dir.join(&old.filename),
                    is_mod: false,
                    counted: false,
                });
            }
            emit(
                app,
                "repairing",
                i,
                total,
                &format!("Restoring {} {}/{}...", class.label(), i + 1, total),
            );
            if i > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            let cf_file = curseforge::get_mod_file(pid, fid, &ctx.api_key)
                .await
                .map_err(|e| format!("CurseForge info for {} failed: {}", pid_str, e))?;
            check_safe_file_name(&cf_file.file_name).map_err(|e| e.to_string())?;
            let rel = format!("{}/{}", class.folder(), cf_file.file_name);
            extract_budget.claim_path(&rel).map_err(|e| e.to_string())?;
            let staged = cf_staged_path(&stage_mods, class, &cf_file.file_name);
            if let Some(parent) = staged.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create staging folder: {}", e))?;
            }
            curseforge::download_mod_file(pid, fid, &cf_file, &ctx.api_key, &staged)
                .await
                .map_err(|e| format!("Download of {} failed: {}", cf_file.file_name, e))?;
            let preserve_disabled = old
                .map(|o| o.filename.ends_with(".disabled"))
                .unwrap_or(false);
            let display = cf_file.display_name.clone();
            plan.installs.push(PlannedInstall {
                staged,
                dest: cf_destination_dir(&ctx.mc_dir, class).join(&cf_file.file_name),
                preserve_disabled,
                // CF repair is identity-based: a file with the same name but a
                // wrong identity is pack-owned by path and is replaced.
                expect_absent: false,
                previous_name: None,
                sidecar: SidecarPlan::Content(SidecarWriteOwned {
                    provider: "curseforge".to_string(),
                    content_type: class.content_type().to_string(),
                    project_id: Some(pid_str.clone()),
                    project_name: Some(display.clone()),
                    version_id: None,
                    version_number: Some(display),
                    file_id: Some(fid_str.clone()),
                    hash: cf_file.sha1_hash().map(|s| s.to_string()),
                }),
                is_mod: true,
            });
        }

        // ---- Overrides whose file is gone (protected/user files stay out) ----
        let missing: Vec<String> = lock
            .overrides
            .iter()
            .filter(|rel| {
                !packlock::is_protected_path(rel) && !content_present(&ctx.mc_dir.join(rel))
            })
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
            let pack_pid = parse_id(&lock.pack_project_id, "pack")?;
            let file_id = parse_id(&lock.file_id, "file")?;
            let file = curseforge::get_mod_file(pack_pid, file_id, &ctx.api_key)
                .await
                .map_err(|e| format!("Pack archive info failed: {}", e))?;
            curseforge::download_mod_file(pack_pid, file_id, &file, &ctx.api_key, &temp_zip)
                .await
                .map_err(|e| format!("Pack archive download failed: {}", e))?;
            let installs = stage_repair_overrides(
                &temp_zip,
                OverridePrefix::CurseForge(repair_overrides_prefix.clone()),
                &missing,
                &stage_overrides,
                &ctx.mc_dir,
                "curseforge",
                &mut extract_budget,
            )?;
            report.checked += installs.len();
            plan.installs.extend(installs);
        }
        Ok(())
    }
    .await;

    let report = match prepare {
        Ok(()) => report,
        Err(e) => {
            cleanup_files(&staging, &temp_zip);
            return Err(format!(
                "Repair preparation failed, the instance was not modified: {}",
                e
            ));
        }
    };

    emit(app, "applying", 0, 1, "Applying repair...");
    let outcome = apply_plan(&ctx.mc_dir, &plan, &staging, &temp_zip)?;
    emit(app, "done", 1, 1, "Repair finished.");
    Ok(PackRepairReport {
        restored: outcome.mods_installed + outcome.overrides_written,
        ..report
    })
}

/// Stage the changed Modrinth override files. Base and client layers have
/// separate path claims (the client layer may legally replace a base file) but
/// share the caller's operation-wide resource budget, so two layers can never
/// double the byte/file limits.
fn stage_mrpack_overrides(
    zip_path: &Path,
    staging_overrides: &Path,
    changed: &[String],
    operation_budget: &crate::import::ExtractBudget,
) -> Result<(), String> {
    let file =
        std::fs::File::open(zip_path).map_err(|e| format!("Cannot open pack archive: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;
    crate::import::check_archive_entry_count(archive.len()).map_err(|e| e.to_string())?;
    let mut base_budget = operation_budget.layer();
    let mut client_budget = operation_budget.layer();
    for client_layer in [false, true] {
        let budget = if client_layer {
            &mut client_budget
        } else {
            &mut base_budget
        };
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("Cannot read pack entry: {}", e))?;
            let name = entry
                .name()
                .map(|name| name.into_owned())
                .map_err(|error| format!("Cannot read pack entry name: {error}"))?;
            let Some(rel) = crate::import::extract_mrpack_override(&name) else {
                continue;
            };
            let is_client =
                name.starts_with("client-overrides/") || name.starts_with("overrides-client/");
            if is_client != client_layer {
                continue;
            }
            if rel.is_empty() || entry.is_dir() {
                continue;
            }
            if !changed.iter().any(|o| o == rel) {
                continue;
            }
            if packlock::is_protected_path(rel) {
                continue;
            }
            check_safe_relative(rel).map_err(|e| e.to_string())?;
            write_extracted_budgeted(staging_overrides, rel, &mut entry, budget)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Root-level (non-override) files of an mrpack zip, keyed by normalized
/// path тАФ the same embedded payload the importer holds in memory during a
/// fresh install. Staging reuses it so embedded files need no network.
fn read_embedded_files(
    zip_path: &Path,
    budget: &mut crate::import::ExtractBudget,
) -> Result<HashMap<String, Vec<u8>>, String> {
    let file =
        std::fs::File::open(zip_path).map_err(|e| format!("Cannot open pack archive: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;
    crate::import::check_archive_entry_count(archive.len()).map_err(|e| e.to_string())?;
    let mut out: HashMap<String, Vec<u8>> = HashMap::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| format!("Cannot read pack entry: {}", e))?;
        let name = entry
            .name()
            .map(|name| name.into_owned())
            .map_err(|error| format!("Cannot read pack entry name: {error}"))?;
        if name == "modrinth.index.json"
            || name.starts_with("overrides-server/")
            || name.starts_with("server-overrides/")
        {
            continue;
        }
        if crate::import::extract_mrpack_override(&name).is_some() {
            continue;
        }
        if entry.is_dir() {
            continue;
        }
        if entry.size() > crate::import::MAX_EMBEDDED_BYTES {
            return Err("The modpack embeds too much data to install safely".to_string());
        }
        let mut buf = Vec::new();
        entry
            .take(crate::import::MAX_EMBEDDED_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|e| format!("Cannot read pack entry {}: {}", name, e))?;
        budget
            .consume_bytes(buf.len() as u64)
            .map_err(|e| e.to_string())?;
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
    let latest = match modrinth_update_candidate(&lock).await? {
        PackCandidate::Newer(version) => version,
        PackCandidate::UpToDate => {
            return Err(
                "No newer pack version is available in the installed release channel.".to_string(),
            )
        }
        PackCandidate::UnknownChannel => {
            return Err(
                "Cannot determine the release channel of the installed pack. \
                 Automatic updates are disabled to avoid a downgrade or a silent channel change; \
                 reinstall the pack to switch channels."
                    .to_string(),
            )
        }
        PackCandidate::UnknownVersionOrder => {
            return Err(
                "Cannot determine whether the available pack version is newer than the installed one. \
                 Automatic updates are disabled to prevent a downgrade; install a chosen version manually."
                    .to_string(),
            )
        }
    };
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
        "voidlauncher-mrpack-{}-{}-{}.mrpack",
        safe_instance,
        instance_temp_token(&ctx.instance.name),
        latest.id
    ));
    let sha1 = file.hashes.get("sha1").map(|s| s.as_str()).unwrap_or("");
    if let Err(e) =
        crate::download::download_file_sized(&file.url, &temp_zip, sha1, file.size).await
    {
        let _ = std::fs::remove_file(&temp_zip);
        return Err(format!("Pack download failed: {}", e));
    }

    let staging = operation_staging_dir(&ctx.instance_dir, "pack-update");
    let (new_manifest, new_overrides, new_hashes) = match packlock::mrpack_zip_info(&temp_zip) {
        Ok(v) => v,
        Err(e) => {
            cleanup_files(&staging, &temp_zip);
            return Err(e);
        }
    };

    // The pack's target Minecraft/loader must match this instance тАФ a bump
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
    let (changed_overrides, overrides_skipped) = preserve_ambiguous_overrides(
        &ctx.mc_dir,
        &lock,
        changed_override_paths(&lock, &new_overrides, &new_hashes),
        &new_hashes,
    );
    let overrides_changed =
        new_overrides != lock.overrides || !changed_overrides.is_empty() || overrides_skipped > 0;
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
    // One shared resource budget covers embedded payload and every override
    // layer; only the path-claim registries are per layer.
    let mut operation_budget = crate::import::ExtractBudget::new();
    let embedded = match read_embedded_files(&temp_zip, &mut operation_budget) {
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
        if let Err(e) = check_manifest_entry_path(&path) {
            cleanup_files(&staging, &temp_zip);
            return Err(format!("Staging failed, instance untouched: {}", e));
        }
        if let Err(e) =
            crate::import::install_mrpack_file(&path, entry, &staging, &embedded, false).await
        {
            cleanup_files(&staging, &temp_zip);
            return Err(format!("Staging failed, instance untouched: {}", e));
        }
    }

    // ---- Phase 2: stage overrides whose pack content changed ----
    // Extract the base layer first, then client-overrides/, so the client
    // variant wins exactly as it does during a fresh import. Staging in raw
    // zip order could let an earlier client entry be overwritten by a later
    // base entry for the same path.
    if let Err(e) =
        stage_mrpack_overrides(&temp_zip, &stage_ov, &changed_overrides, &operation_budget)
    {
        cleanup_files(&staging, &temp_zip);
        return Err(format!(
            "Staging overrides failed, instance untouched: {}",
            e
        ));
    }

    // ---- Phase 3: apply (sync тАФ all network work is done) ----
    emit(app, "applying", 0, 1, "Applying update...");
    let plan = match plan_modrinth_update(
        &ctx.mc_dir,
        &staging,
        &MrApplyInput {
            old_map: &old_map,
            new_map: &new_map,
            diff: &diff,
            new_overrides: &new_overrides,
            old_overrides: &lock.overrides,
        },
    ) {
        Ok(plan) => plan,
        Err(e) => {
            cleanup_files(&staging, &temp_zip);
            return Err(e);
        }
    };

    let new_lock = PackLock {
        provider: "modrinth".to_string(),
        pack_project_id: lock.pack_project_id.clone(),
        pack_name: lock.pack_name.clone(),
        file_id: latest.id.clone(),
        file_name: file.filename.clone(),
        file_display_name: latest.version_number.clone(),
        channel: packlock::PackChannel::from_modrinth(&latest.version_type)
            .map(|channel| channel.as_str().to_string())
            .unwrap_or_default(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        mc_version: new_mc.to_string(),
        loader: new_loader.to_string(),
        manifest: new_manifest,
        overrides: new_overrides,
        overrides_prefix: String::new(),
        override_hashes: new_hashes,
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
        overrides_skipped,
        new_version: latest.version_number.clone(),
    })
}

/// mrpack repair: every client file the index lists but the instance is
/// missing is re-downloaded into staging; missing overrides are restored from
/// a fresh pack zip. The instance is modified only by the shared
/// apply/rollback path.
async fn repair_modrinth(
    app: &AppHandle,
    ctx: &PackCtx,
    lock: PackLock,
) -> Result<PackRepairReport, String> {
    let map = packlock::mrpack_manifest_map(&lock.manifest);
    // Reject user-file targets before creating staging/temp paths or making
    // any network request. A tampered lock must not be "repaired" into a
    // user's options.txt, servers.dat or saves.
    check_repair_manifest_paths(&map)?;
    let total = map.len();
    let mut report = PackRepairReport {
        checked: 0,
        restored: 0,
        skipped: 0,
        failed: Vec::new(),
    };
    let no_embedded: Arc<HashMap<String, Vec<u8>>> = Arc::new(HashMap::new());

    let staging = operation_staging_dir(&ctx.instance_dir, "pack-repair");
    let stage_overrides = staging.join("overrides");
    let temp_zip = repair_temp_zip("mrpack", &ctx.instance.name, &lock.file_id, "mrpack");
    let mut plan = ApplyPlan::default();
    let mut extract_budget = crate::import::ExtractBudget::new();

    let prepare: Result<(), String> = async {
        std::fs::create_dir_all(&staging)
            .map_err(|e| format!("Cannot create staging folder: {}", e))?;

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
            extract_budget
                .claim_path(&path)
                .map_err(|e| e.to_string())?;
            crate::import::install_mrpack_file(&path, entry, &staging, &no_embedded, false)
                .await
                .map_err(|e| format!("{}: {}", key, e))?;
            plan.installs.push(PlannedInstall {
                staged: staging.join(&path),
                dest,
                preserve_disabled: false,
                expect_absent: true,
                previous_name: None,
                sidecar: SidecarPlan::Content(SidecarWriteOwned {
                    provider: "modrinth".to_string(),
                    content_type: packlock::content_type_for_rel(&path).to_string(),
                    project_id: Some(
                        entry["project_id"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| key.clone()),
                    ),
                    project_name: None,
                    version_id: Some(packlock::mrpack_entry_version(entry)),
                    version_number: None,
                    file_id: None,
                    hash: packlock::mrpack_entry_sha1(entry),
                }),
                is_mod: true,
            });
        }

        // ---- Overrides whose file is gone (protected/user files stay out) ----
        let missing: Vec<String> = lock
            .overrides
            .iter()
            .filter(|rel| {
                !packlock::is_protected_path(rel) && !content_present(&ctx.mc_dir.join(rel))
            })
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
            let version = modrinth::get_version_by_id(&lock.file_id)
                .await
                .map_err(|e| format!("Pack version info failed: {}", e))?;
            let file = version
                .files
                .iter()
                .find(|f| f.primary)
                .or_else(|| version.files.first())
                .ok_or_else(|| "Pack version has no files".to_string())?;
            if !file.url.starts_with("https://") || !crate::is_allowed_download_host(&file.url) {
                return Err("Pack file URL is not allowed".to_string());
            }
            let sha1 = file.hashes.get("sha1").map(|s| s.as_str()).unwrap_or("");
            crate::download::download_file_sized(&file.url, &temp_zip, sha1, file.size)
                .await
                .map_err(|e| format!("Pack archive download failed: {}", e))?;
            let installs = stage_mrpack_repair_overrides(
                &temp_zip,
                &missing,
                &stage_overrides,
                &ctx.mc_dir,
                "modrinth",
                &mut extract_budget,
            )?;
            report.checked += installs.len();
            plan.installs.extend(installs);
        }
        Ok(())
    }
    .await;

    let report = match prepare {
        Ok(()) => report,
        Err(e) => {
            cleanup_files(&staging, &temp_zip);
            return Err(format!(
                "Repair preparation failed, the instance was not modified: {}",
                e
            ));
        }
    };

    emit(app, "applying", 0, 1, "Applying repair...");
    let outcome = apply_plan(&ctx.mc_dir, &plan, &staging, &temp_zip)?;
    emit(app, "done", 1, 1, "Repair finished.");
    Ok(PackRepairReport {
        restored: outcome.mods_installed + outcome.overrides_written,
        ..report
    })
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

    fn cf_file_at(id: u64, release_type: u32, date: &str) -> curseforge::CfFile {
        cf_file_version(id, release_type, date, &format!("{}.0.0", id))
    }

    fn cf_file_version(
        id: u64,
        release_type: u32,
        date: &str,
        version: &str,
    ) -> curseforge::CfFile {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "displayName": version,
            "fileName": format!("pack-{}.zip", id),
            "downloadUrl": "https://example.invalid/file",
            "fileLength": 10,
            "fileDate": date,
            "gameVersions": ["1.20.1"],
            "dependencies": [],
            "releaseType": release_type,
            "hashes": [{"value": "0123456789abcdef", "algo": 1}],
            "isAvailable": true
        }))
        .expect("CfFile fixture")
    }

    fn mr_installed(id: &str, date: &str, version_type: &str) -> modrinth::ModrinthVersionResponse {
        mr_installed_version(id, date, version_type, &format!("{id}.0.0"))
    }

    fn mr_installed_version(
        id: &str,
        date: &str,
        version_type: &str,
        version_number: &str,
    ) -> modrinth::ModrinthVersionResponse {
        modrinth::ModrinthVersionResponse {
            id: id.to_string(),
            project_id: "pack".to_string(),
            name: format!("Pack {id}"),
            version_number: version_number.to_string(),
            game_versions: vec!["1.20.1".to_string()],
            loaders: vec!["fabric".to_string()],
            files: Vec::new(),
            dependencies: Vec::new(),
            date_published: date.to_string(),
            version_type: version_type.to_string(),
        }
    }

    fn mr_candidate(id: &str, date: &str, version_type: &str) -> ModrinthVersion {
        mr_candidate_version(id, date, version_type, &format!("{id}.0.0"))
    }

    fn mr_candidate_version(
        id: &str,
        date: &str,
        version_type: &str,
        version_number: &str,
    ) -> ModrinthVersion {
        ModrinthVersion {
            id: id.to_string(),
            project_id: "pack".to_string(),
            name: format!("Pack {id}"),
            version_number: version_number.to_string(),
            game_versions: vec!["1.20.1".to_string()],
            loaders: vec!["fabric".to_string()],
            files: Vec::new(),
            dependencies: Vec::new(),
            date_published: date.to_string(),
            downloads: 0,
            version_type: version_type.to_string(),
        }
    }

    fn sample_lock(file_id: &str) -> PackLock {
        PackLock {
            provider: "curseforge".to_string(),
            pack_project_id: "10".to_string(),
            pack_name: "Test Pack".to_string(),
            file_id: file_id.to_string(),
            file_name: "pack.zip".to_string(),
            file_display_name: format!("v{}", file_id),
            channel: "release".to_string(),
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            mc_version: "1.20.1".to_string(),
            loader: "fabric".to_string(),
            manifest: serde_json::json!({"files": []}),
            overrides: vec![],
            overrides_prefix: String::new(),
            override_hashes: BTreeMap::new(),
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

    fn assert_user_files_unchanged(pack: &TestPack) {
        assert_eq!(
            std::fs::read(pack.mc.join("options.txt")).unwrap(),
            b"user-options"
        );
        assert_eq!(
            std::fs::read(pack.mc.join("servers.dat")).unwrap(),
            b"user-servers"
        );
        assert_eq!(
            std::fs::read(pack.mc.join("saves/world/level.dat")).unwrap(),
            b"user-world"
        );
    }

    fn user_files_fixture(pack: &TestPack) {
        write(&pack.mc.join("options.txt"), b"user-options");
        write(&pack.mc.join("servers.dat"), b"user-servers");
        write(&pack.mc.join("saves/world/level.dat"), b"user-world");
    }

    fn manifest_with_path(path: &str) -> serde_json::Value {
        serde_json::json!({
            "files": [{
                "path": path,
                "project_id": "attacker",
                "version_id": "v1",
                "hashes": {"sha1": "deadbeef"}
            }]
        })
    }

    #[test]
    fn modrinth_update_rejects_protected_manifest_paths_before_writing() {
        let cases = [
            ("options", "options.txt"),
            ("options_dot", "options.txt."),
            ("options_space", "options.txt "),
            ("options_case", "OPTIONS.TXT"),
            ("servers", "servers.dat"),
            ("saves_plain", "saves/world/level.dat"),
            ("saves_dot", "saves./world/level.dat"),
            ("saves_dot_component", "saves/./world/level.dat"),
            ("saves_backslash", "saves\\world\\level.dat"),
            ("saves_dir", "saves"),
        ];
        for (case, path) in cases {
            let pack = TestPack::new(&format!("protected_{case}"));
            user_files_fixture(&pack);
            let outside = pack.root.join("outside.txt");
            write(&outside, b"outside");

            let old_manifest = serde_json::json!({"files": []});
            let new_manifest = manifest_with_path(path);
            let old_map = packlock::mrpack_manifest_map(&old_manifest);
            let new_map = packlock::mrpack_manifest_map(&new_manifest);
            let diff = packlock::diff_mrpack_manifests(&old_manifest, &new_manifest);

            let err = plan_modrinth_update(
                &pack.mc,
                &pack.staging,
                &MrApplyInput {
                    old_map: &old_map,
                    new_map: &new_map,
                    diff: &diff,
                    new_overrides: &[],
                    old_overrides: &[],
                },
            )
            .expect_err("a protected manifest path must abort the plan");

            assert!(
                err.contains("protected user file"),
                "unexpected error for {path:?}: {err}"
            );
            assert_user_files_unchanged(&pack);
            assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
        }
    }

    #[test]
    fn modrinth_repair_rejects_protected_manifest_paths_without_touching_user_files() {
        let pack = TestPack::new("repair_protected");
        user_files_fixture(&pack);

        for path in [
            "options.txt",
            "options.txt.",
            "options.txt ",
            "servers.dat",
            "saves/world/level.dat",
            "saves./world/level.dat",
            "SAVES\\world\\level.dat",
            "saves",
        ] {
            let map = packlock::mrpack_manifest_map(&manifest_with_path(path));
            let err = check_repair_manifest_paths(&map)
                .expect_err("repair must reject a protected manifest path");
            assert!(
                err.contains("protected user file"),
                "unexpected error for {path:?}: {err}"
            );
            assert_user_files_unchanged(&pack);
        }
    }

    #[test]
    fn modrinth_manifest_check_accepts_pack_owned_paths() {
        let map = packlock::mrpack_manifest_map(&serde_json::json!({
            "files": [
                {"path": "mods/sodium.jar", "project_id": "s", "hashes": {"sha1": "a"}},
                {"path": "config/example.toml", "project_id": "c", "hashes": {"sha1": "b"}}
            ]
        }));
        check_repair_manifest_paths(&map).expect("pack-owned files are allowed");
    }

    #[test]
    fn curseforge_channel_policy_keeps_the_installed_channel() {
        // Release -> newer release.
        let installed_release = cf_file_at(1, 1, "2026-01-01T00:00:00Z");
        let files = vec![
            cf_file_at(2, 1, "2026-02-01T00:00:00Z"),
            cf_file_at(3, 2, "2026-03-01T00:00:00Z"),
        ];
        match curseforge_update_decision(&installed_release, &files, "release") {
            PackCandidate::Newer(file) => assert_eq!(file.id, 2),
            other => panic!("expected the newer release, got {other:?}"),
        }

        // Release -> only a newer beta: no silent channel switch.
        let beta_only = vec![cf_file_at(3, 2, "2026-03-01T00:00:00Z")];
        assert!(matches!(
            curseforge_update_decision(&installed_release, &beta_only, "release"),
            PackCandidate::UpToDate
        ));

        // Beta -> newer beta, even when an older/newer release exists.
        let installed_beta = cf_file_at(10, 2, "2026-01-10T00:00:00Z");
        let beta_files = vec![
            cf_file_at(11, 1, "2026-02-01T00:00:00Z"),
            cf_file_at(12, 2, "2026-01-20T00:00:00Z"),
        ];
        match curseforge_update_decision(&installed_beta, &beta_files, "") {
            PackCandidate::Newer(file) => assert_eq!(file.id, 12),
            other => panic!("expected the newer beta, got {other:?}"),
        }

        // Beta -> a release with a later date is still a channel change.
        assert!(matches!(
            curseforge_update_decision(
                &installed_beta,
                &[cf_file_at(11, 1, "2026-02-01T00:00:00Z")],
                "beta"
            ),
            PackCandidate::UpToDate
        ));

        // Beta -> only an older release: never a downgrade.
        assert!(matches!(
            curseforge_update_decision(
                &installed_beta,
                &[cf_file_at(11, 1, "2025-12-01T00:00:00Z")],
                "beta"
            ),
            PackCandidate::UpToDate
        ));

        // Alpha -> beta / older release: no downgrade or channel switch.
        let installed_alpha = cf_file_at(20, 3, "2026-01-10T00:00:00Z");
        assert!(matches!(
            curseforge_update_decision(
                &installed_alpha,
                &[cf_file_at(21, 2, "2026-02-01T00:00:00Z")],
                "alpha"
            ),
            PackCandidate::UpToDate
        ));
        assert!(matches!(
            curseforge_update_decision(
                &installed_alpha,
                &[cf_file_at(22, 1, "2025-12-01T00:00:00Z")],
                "alpha"
            ),
            PackCandidate::UpToDate
        ));
        match curseforge_update_decision(
            &installed_alpha,
            &[cf_file_at(23, 3, "2026-02-01T00:00:00Z")],
            "",
        ) {
            PackCandidate::Newer(file) => assert_eq!(file.id, 23),
            other => panic!("expected the newer alpha, got {other:?}"),
        }

        // No candidate at all.
        assert!(matches!(
            curseforge_update_decision(&installed_release, &[], "release"),
            PackCandidate::UpToDate
        ));
    }

    #[test]
    fn curseforge_unknown_or_conflicting_channel_is_not_updated() {
        let unknown_provider = cf_file_at(1, 9, "2026-01-01T00:00:00Z");
        assert!(matches!(
            curseforge_update_decision(&unknown_provider, &[], ""),
            PackCandidate::UnknownChannel
        ));
        assert!(matches!(
            curseforge_update_decision(
                &cf_file_at(1, 2, "2026-01-01T00:00:00Z"),
                &[cf_file_at(2, 2, "2026-02-01T00:00:00Z")],
                "release"
            ),
            PackCandidate::UnknownChannel
        ));
    }

    #[test]
    fn modrinth_channel_policy_keeps_the_installed_channel() {
        let installed_release = mr_installed("1", "2026-01-01T00:00:00Z", "release");
        let versions = vec![
            mr_candidate("2", "2026-02-01T00:00:00Z", "release"),
            mr_candidate("3", "2026-03-01T00:00:00Z", "beta"),
        ];
        match modrinth_update_decision(&installed_release, &versions, "release") {
            PackCandidate::Newer(version) => assert_eq!(version.id, "2"),
            other => panic!("expected the newer release, got {other:?}"),
        }
        assert!(matches!(
            modrinth_update_decision(
                &installed_release,
                &[mr_candidate("3", "2026-03-01T00:00:00Z", "beta")],
                "release"
            ),
            PackCandidate::UpToDate
        ));

        let installed_beta = mr_installed("10", "2026-01-10T00:00:00Z", "beta");
        match modrinth_update_decision(
            &installed_beta,
            &[
                mr_candidate("11", "2026-02-01T00:00:00Z", "release"),
                mr_candidate("12", "2026-01-20T00:00:00Z", "beta"),
            ],
            "",
        ) {
            PackCandidate::Newer(version) => assert_eq!(version.id, "12"),
            other => panic!("expected the newer beta, got {other:?}"),
        }
        assert!(matches!(
            modrinth_update_decision(
                &installed_beta,
                &[mr_candidate("11", "2026-02-01T00:00:00Z", "release")],
                "beta"
            ),
            PackCandidate::UpToDate
        ));
        assert!(matches!(
            modrinth_update_decision(
                &installed_beta,
                &[mr_candidate("11", "2025-12-01T00:00:00Z", "release")],
                "beta"
            ),
            PackCandidate::UpToDate
        ));

        let installed_alpha = mr_installed("20", "2026-01-10T00:00:00Z", "alpha");
        assert!(matches!(
            modrinth_update_decision(
                &installed_alpha,
                &[mr_candidate("21", "2026-02-01T00:00:00Z", "beta")],
                "alpha"
            ),
            PackCandidate::UpToDate
        ));
        assert!(matches!(
            modrinth_update_decision(
                &installed_alpha,
                &[mr_candidate("22", "2025-12-01T00:00:00Z", "release")],
                "alpha"
            ),
            PackCandidate::UpToDate
        ));
        match modrinth_update_decision(
            &installed_alpha,
            &[mr_candidate("23", "2026-02-01T00:00:00Z", "alpha")],
            "",
        ) {
            PackCandidate::Newer(version) => assert_eq!(version.id, "23"),
            other => panic!("expected the newer alpha, got {other:?}"),
        }

        assert!(matches!(
            modrinth_update_decision(&installed_release, &[], "release"),
            PackCandidate::UpToDate
        ));
    }

    #[test]
    fn modrinth_unknown_or_conflicting_channel_is_not_updated() {
        let unknown_provider = mr_installed("1", "2026-01-01T00:00:00Z", "");
        assert!(matches!(
            modrinth_update_decision(&unknown_provider, &[], ""),
            PackCandidate::UnknownChannel
        ));
        assert!(matches!(
            modrinth_update_decision(
                &mr_installed("1", "2026-01-01T00:00:00Z", "beta"),
                &[mr_candidate("2", "2026-02-01T00:00:00Z", "beta")],
                "release"
            ),
            PackCandidate::UnknownChannel
        ));
    }

    #[test]
    fn locks_without_a_channel_remain_readable_and_use_provider_metadata() {
        let mut value = serde_json::to_value(sample_lock("1")).expect("lock serializes");
        value
            .as_object_mut()
            .expect("lock is an object")
            .remove("channel");
        let lock: PackLock = serde_json::from_value(value).expect("old lock must stay readable");
        assert_eq!(lock.channel, "");
        assert_eq!(lock.file_id, "1");

        // The provider API fills the gap for an old lock; the same decision
        // still refuses a cross-channel candidate.
        let installed = mr_installed("1", "2026-01-01T00:00:00Z", "beta");
        match modrinth_update_decision(
            &installed,
            &[mr_candidate("2", "2026-02-01T00:00:00Z", "beta")],
            &lock.channel,
        ) {
            PackCandidate::Newer(version) => assert_eq!(version.id, "2"),
            other => panic!("expected the newer beta, got {other:?}"),
        }
    }

    fn content_install(staged: PathBuf, dest: PathBuf, is_mod: bool) -> PlannedInstall {
        PlannedInstall {
            staged,
            dest,
            preserve_disabled: false,
            expect_absent: false,
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
        let classes: HashMap<String, crate::curseforge::CfContentClass> = staged_mods
            .iter()
            .map(|(r, _)| (r.project_id.clone(), crate::curseforge::CfContentClass::Mod))
            .collect();
        plan_curseforge_update(
            &pack.mc,
            &pack.staging.join("mods"),
            &pack.staging.join("overrides"),
            &CfApplyInput {
                installed,
                staged_mods,
                classes: &classes,
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
        )
        .expect("the manifest only names pack content");
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

        rb.rollback(&pack.staging, &pack.temp_zip).unwrap();
        rb.rollback(&pack.staging, &pack.temp_zip).unwrap();

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
            &pack.mc,
            &pack.staging.join("mods"),
            &pack.staging.join("overrides"),
            &CfApplyInput {
                installed: &installed,
                staged_mods: &[],
                classes: &HashMap::new(),
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
        )
        .expect("the manifest only names pack content");
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
            &pack.mc,
            &pack.staging.join("mods"),
            &pack.staging.join("overrides"),
            &CfApplyInput {
                installed: &installed,
                staged_mods: &[],
                classes: &HashMap::new(),
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
                expect_absent: false,
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
                expect_absent: false,
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
        )
        .expect("the manifest only names pack content");
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

    // ---- repair prepare/apply/rollback ----------------------------------

    fn write_test_zip(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write as _;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn mrpack_update_staging_uses_one_budget_across_both_layers() {
        let pack = TestPack::new("mr_stage_budget");
        let zip_path = pack.root.join("pack.mrpack");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/config/base.toml", b"123456"),
                ("client-overrides/config/client.toml", b"654321"),
            ],
        );
        let changed = vec![
            "config/base.toml".to_string(),
            "config/client.toml".to_string(),
        ];
        let user_file = pack.mc.join("saves/world/level.dat");
        write(&user_file, b"user-world");
        let budget = crate::import::ExtractBudget::with_limits(10, 10);
        let err = stage_mrpack_overrides(
            &zip_path,
            &pack.staging.join("overrides"),
            &changed,
            &budget,
        )
        .unwrap_err();
        assert!(err.contains("total limit"), "got: {err}");
        assert_eq!(
            std::fs::read(&user_file).unwrap(),
            b"user-world",
            "a failed staging operation must not touch the instance"
        );
    }

    #[test]
    fn mrpack_update_staging_allows_client_override_of_the_same_path() {
        let pack = TestPack::new("mr_stage_overlap");
        let zip_path = pack.root.join("pack.mrpack");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/config/shared.toml", b"base"),
                ("client-overrides/config/shared.toml", b"client"),
            ],
        );
        let changed = vec!["config/shared.toml".to_string()];
        let budget = crate::import::ExtractBudget::with_limits(100, 100);
        stage_mrpack_overrides(
            &zip_path,
            &pack.staging.join("overrides"),
            &changed,
            &budget,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(pack.staging.join("overrides/config/shared.toml")).unwrap(),
            b"client"
        );
    }

    #[test]
    fn mrpack_update_staging_rejects_duplicate_paths_within_a_layer() {
        let pack = TestPack::new("mr_stage_duplicate");
        let zip_path = pack.root.join("pack.mrpack");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/config/a.toml", b"one"),
                ("overrides/config/A.TOML", b"two"),
            ],
        );
        let changed = vec!["config/a.toml".to_string(), "config/A.TOML".to_string()];
        let budget = crate::import::ExtractBudget::with_limits(100, 100);
        let err = stage_mrpack_overrides(
            &zip_path,
            &pack.staging.join("overrides"),
            &changed,
            &budget,
        )
        .unwrap_err();
        assert!(err.contains("Duplicate or conflicting"), "got: {err}");
    }

    #[test]
    fn mrpack_update_staging_enforces_file_count_and_path_limits() {
        let pack = TestPack::new("mr_stage_limits");
        let zip_path = pack.root.join("pack.mrpack");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/a", b""),
                ("overrides/b", b""),
                ("overrides/c", b""),
            ],
        );
        let changed = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let files = crate::import::ExtractBudget::with_all_limits(1000, 1000, 2, 1000);
        let err =
            stage_mrpack_overrides(&zip_path, &pack.staging.join("overrides"), &changed, &files)
                .unwrap_err();
        assert!(err.contains("file count limit"), "got: {err}");

        let paths = crate::import::ExtractBudget::with_all_limits(1000, 1000, 1000, 4);
        let err =
            stage_mrpack_overrides(&zip_path, &pack.staging.join("overrides"), &changed, &paths)
                .unwrap_err();
        assert!(err.contains("path metadata"), "got: {err}");
    }

    #[test]
    fn curseforge_update_routes_resource_and_shader_packs_to_their_folders() {
        let pack = TestPack::new("cf_class_routing");
        let classes = HashMap::from([
            (
                "10".to_string(),
                crate::curseforge::CfContentClass::ResourcePack,
            ),
            (
                "11".to_string(),
                crate::curseforge::CfContentClass::ShaderPack,
            ),
        ]);
        let staged_mods = vec![
            (
                PackFileRef {
                    project_id: "10".to_string(),
                    file_id: "6".to_string(),
                },
                cf_file("pack.zip", 6),
            ),
            (
                PackFileRef {
                    project_id: "11".to_string(),
                    file_id: "7".to_string(),
                },
                cf_file("shader.zip", 7),
            ),
        ];
        let plan = plan_curseforge_update(
            &pack.mc,
            &pack.staging.join("mods"),
            &pack.staging.join("overrides"),
            &CfApplyInput {
                installed: &[],
                staged_mods: &staged_mods,
                classes: &classes,
                diff: &PackDiff::default(),
                new_overrides: &[],
                old_overrides: &[],
            },
        );
        assert_eq!(plan.installs.len(), 2);
        assert_eq!(
            plan.installs[0].dest,
            pack.mc.join("resourcepacks").join("pack.zip")
        );
        assert_eq!(
            plan.installs[0].staged,
            pack.staging
                .join("mods")
                .join("resourcepacks")
                .join("pack.zip")
        );
        assert_eq!(
            plan.installs[1].dest,
            pack.mc.join("shaderpacks").join("shader.zip")
        );
        assert!(matches!(
            &plan.installs[0].sidecar,
            SidecarPlan::Content(SidecarWriteOwned {
                content_type,
                ..
            }) if content_type == "resourcepack"
        ));
        assert!(matches!(
            &plan.installs[1].sidecar,
            SidecarPlan::Content(SidecarWriteOwned {
                content_type,
                ..
            }) if content_type == "shader"
        ));
        assert_eq!(
            cf_staged_path(
                &pack.staging.join("mods"),
                crate::curseforge::CfContentClass::ShaderPack,
                "shader.zip"
            ),
            pack.staging.join("mods/shaderpacks/shader.zip")
        );
    }

    #[test]
    fn curseforge_repair_reads_the_manifest_custom_overrides_prefix() {
        let pack = TestPack::new("cf_custom_prefix");
        let zip_path = pack.root.join("pack.zip");
        write_test_zip(
            &zip_path,
            &[
                ("custom/path/config/a.toml", b"custom"),
                ("custom/path/nested/deep/b.toml", b"deep"),
                ("custom/path/options.txt", b"fov:90"),
                ("overrides/config/wrong.toml", b"wrong"),
            ],
        );
        let mut budget = crate::import::ExtractBudget::new();
        let staging = pack.staging.join("overrides");
        let installs = stage_repair_overrides(
            &zip_path,
            OverridePrefix::CurseForge("custom/path/".to_string()),
            &[
                "config/a.toml".to_string(),
                "nested/deep/b.toml".to_string(),
            ],
            &staging,
            &pack.mc,
            "curseforge",
            &mut budget,
        )
        .unwrap();
        assert_eq!(installs.len(), 2);
        assert_eq!(
            std::fs::read(staging.join("config/a.toml")).unwrap(),
            b"custom"
        );
        assert_eq!(
            std::fs::read(staging.join("nested/deep/b.toml")).unwrap(),
            b"deep"
        );
        assert!(!staging.join("config/wrong.toml").exists());
        let err = stage_repair_overrides(
            &zip_path,
            OverridePrefix::CurseForge("custom/path/".to_string()),
            &["options.txt".to_string()],
            &staging,
            &pack.mc,
            "curseforge",
            &mut budget,
        )
        .unwrap_err();
        assert!(err.contains("protected"), "got: {err}");
    }

    #[test]
    fn old_pack_locks_fall_back_to_the_manifest_prefix() {
        let mut lock = sample_lock("6");
        lock.overrides_prefix.clear();
        lock.manifest = serde_json::json!({"overrides": "custom\\path"});
        assert_eq!(lock.effective_curseforge_overrides_prefix(), "custom/path/");

        let mut old = sample_lock("6");
        old.overrides_prefix.clear();
        old.manifest = serde_json::json!({"files": []});
        assert_eq!(old.effective_curseforge_overrides_prefix(), "overrides/");

        let mut corrupt = sample_lock("6");
        corrupt.overrides_prefix = "../escape/".to_string();
        corrupt.manifest = serde_json::json!({"overrides": "pack/assets"});
        assert_eq!(
            corrupt.effective_curseforge_overrides_prefix(),
            "pack/assets/"
        );
    }

    #[test]
    fn curseforge_reclassification_parks_the_file_from_the_wrong_folder() {
        let pack = TestPack::new("cf_reclassify");
        write(&pack.mods().join("pack.zip"), b"wrong-folder");
        let installed = vec![content::InstalledContent {
            filename: "pack.zip".to_string(),
            content_type: "mod".to_string(),
            provider: "curseforge".to_string(),
            project_id: Some("10".to_string()),
            version_id: None,
            file_id: Some("6".to_string()),
            project_name: None,
            version_number: None,
            hash: None,
            enabled: true,
            installed_at: None,
        }];
        let classes = HashMap::from([(
            "10".to_string(),
            crate::curseforge::CfContentClass::ResourcePack,
        )]);
        let staged_mods = vec![(
            PackFileRef {
                project_id: "10".to_string(),
                file_id: "6".to_string(),
            },
            cf_file("pack.zip", 6),
        )];
        let plan = plan_curseforge_update(
            &pack.mc,
            &pack.staging.join("mods"),
            &pack.staging.join("overrides"),
            &CfApplyInput {
                installed: &installed,
                staged_mods: &staged_mods,
                classes: &classes,
                diff: &PackDiff::default(),
                new_overrides: &[],
                old_overrides: &[],
            },
        );
        assert_eq!(
            plan.installs[0].dest,
            pack.mc.join("resourcepacks").join("pack.zip")
        );
        assert!(
            plan.removals
                .iter()
                .any(|r| r.path == pack.mods().join("pack.zip")),
            "the wrong-class copy must be parked, not left next to the new one"
        );
    }

    fn repair_install(
        staged: PathBuf,
        dest: PathBuf,
        is_mod: bool,
        expect_absent: bool,
    ) -> PlannedInstall {
        PlannedInstall {
            staged,
            dest,
            preserve_disabled: false,
            expect_absent,
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

    fn repair_override_install(staged: PathBuf, dest: PathBuf) -> PlannedInstall {
        PlannedInstall {
            staged,
            dest,
            preserve_disabled: false,
            expect_absent: true,
            previous_name: None,
            sidecar: SidecarPlan::Override("curseforge"),
            is_mod: false,
        }
    }

    #[test]
    fn repair_override_staging_extracts_only_requested_files() {
        let pack = TestPack::new("repair_stage");
        let zip_path = pack.root.join("repair-pack.zip");
        write_test_zip(
            &zip_path,
            &[
                ("manifest.json", b"{}"),
                ("overrides/mods/a.jar", b"a"),
                ("overrides/config/b.toml", b"b"),
                ("overrides/mods/skip.jar", b"skip"),
            ],
        );

        let mut budget = crate::import::ExtractBudget::new();
        let staging = pack.staging.join("overrides");
        let installs = stage_repair_overrides(
            &zip_path,
            OverridePrefix::CurseForge("overrides/".to_string()),
            &["mods/a.jar".to_string(), "config/b.toml".to_string()],
            &staging,
            &pack.mc,
            "curseforge",
            &mut budget,
        )
        .expect("only the requested files are extracted");

        assert_eq!(installs.len(), 2);
        assert_eq!(std::fs::read(staging.join("mods/a.jar")).unwrap(), b"a");
        assert_eq!(std::fs::read(staging.join("config/b.toml")).unwrap(), b"b");
        assert!(!staging.join("mods/skip.jar").exists());
        assert!(
            !pack.mc.join("mods/a.jar").exists(),
            "prepare must not write into the instance"
        );
        assert!(installs.iter().all(|i| i.expect_absent && !i.is_mod));
    }

    #[test]
    fn repair_override_staging_rejects_a_destination_claimed_by_an_index_file() {
        let pack = TestPack::new("repair_cross_collision");
        let zip_path = pack.root.join("repair-pack.zip");
        write_test_zip(&zip_path, &[("overrides/mods/foo.jar", b"override")]);

        let mut budget = crate::import::ExtractBudget::new();
        budget
            .claim_path("mods/Foo.jar")
            .expect("the staged index file reserves its destination");

        let staging = pack.staging.join("overrides");
        let err = stage_repair_overrides(
            &zip_path,
            OverridePrefix::CurseForge("overrides/".to_string()),
            &["mods/foo.jar".to_string()],
            &staging,
            &pack.mc,
            "curseforge",
            &mut budget,
        )
        .expect_err("the override collides with the index file");
        assert!(err.contains("conflicting"), "got: {err}");
        assert!(!staging.join("mods/foo.jar").exists());
        assert!(!pack.mc.join("mods/foo.jar").exists());
    }

    #[test]
    fn repair_override_staging_rejects_duplicates_and_file_directory_conflicts() {
        let pack = TestPack::new("repair_dup");
        let zip_path = pack.root.join("repair-pack.zip");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/mods/A.jar", b"first"),
                ("overrides/mods/a.jar", b"second"),
            ],
        );
        let mut budget = crate::import::ExtractBudget::new();
        let err = stage_repair_overrides(
            &zip_path,
            OverridePrefix::CurseForge("overrides/".to_string()),
            &["mods/A.jar".to_string(), "mods/a.jar".to_string()],
            &pack.staging.join("overrides"),
            &pack.mc,
            "curseforge",
            &mut budget,
        )
        .expect_err("case-insensitive duplicates are conflicting destinations");
        assert!(err.contains("conflicting"), "got: {err}");
        assert!(!pack.mc.join("mods/A.jar").exists());

        let zip_path = pack.root.join("file-dir.zip");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/mods/A", b"file"),
                ("overrides/mods/A/inner.txt", b"nested"),
            ],
        );
        let mut budget = crate::import::ExtractBudget::new();
        let err = stage_repair_overrides(
            &zip_path,
            OverridePrefix::CurseForge("overrides/".to_string()),
            &["mods/A".to_string(), "mods/A/inner.txt".to_string()],
            &pack.staging.join("overrides2"),
            &pack.mc,
            "curseforge",
            &mut budget,
        )
        .expect_err("a file cannot also be a directory");
        assert!(err.contains("conflicting"), "got: {err}");
    }

    #[test]
    fn mrpack_repair_prefers_client_override_for_overlapping_paths() {
        let pack = TestPack::new("mr_repair_client_wins");
        let zip_path = pack.root.join("repair-pack.mrpack");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/config/shared.toml", b"base"),
                ("client-overrides/config/shared.toml", b"client"),
                ("overrides/config/base-only.toml", b"base-only"),
                ("client-overrides/config/client-only.toml", b"client-only"),
            ],
        );
        let missing = vec![
            "config/shared.toml".to_string(),
            "config/base-only.toml".to_string(),
            "config/client-only.toml".to_string(),
        ];
        let staging = pack.staging.join("overrides");
        let mut budget = crate::import::ExtractBudget::with_limits(100, 100);

        let installs = stage_mrpack_repair_overrides(
            &zip_path,
            &missing,
            &staging,
            &pack.mc,
            "modrinth",
            &mut budget,
        )
        .expect("layered overrides must not conflict");

        assert_eq!(installs.len(), 3, "one install per destination");
        assert_eq!(
            std::fs::read(staging.join("config/shared.toml")).unwrap(),
            b"client",
            "client-overrides/ must win over overrides/"
        );
        assert_eq!(
            std::fs::read(staging.join("config/base-only.toml")).unwrap(),
            b"base-only"
        );
        assert_eq!(
            std::fs::read(staging.join("config/client-only.toml")).unwrap(),
            b"client-only"
        );
        assert!(installs.iter().all(|i| i.expect_absent && !i.is_mod));
        assert!(!pack.mc.join("config/shared.toml").exists());
    }

    #[test]
    fn mrpack_repair_client_wins_for_case_insensitive_paths() {
        let pack = TestPack::new("mr_repair_case_insensitive");
        let zip_path = pack.root.join("repair-pack.mrpack");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/config/A.toml", b"base"),
                ("client-overrides/config/a.toml", b"client"),
            ],
        );
        // The lock produced by mrpack_zip_info contains both spellings; on
        // Windows they are the same destination.
        let missing = vec!["config/A.toml".to_string(), "config/a.toml".to_string()];
        let staging = pack.staging.join("overrides");
        let mut budget = crate::import::ExtractBudget::with_limits(100, 100);

        let installs = stage_mrpack_repair_overrides(
            &zip_path,
            &missing,
            &staging,
            &pack.mc,
            "modrinth",
            &mut budget,
        )
        .expect("case-insensitive layered overrides must not conflict");

        assert_eq!(installs.len(), 1, "one Windows destination, not two");
        assert_eq!(
            installs[0].dest.file_name().and_then(|name| name.to_str()),
            Some("a.toml"),
            "the surviving install is the client spelling"
        );
        assert_eq!(
            std::fs::read(staging.join("config/a.toml")).unwrap(),
            b"client",
            "client-overrides/ must win regardless of path case"
        );

        let plan = ApplyPlan {
            installs,
            removals: vec![],
        };
        validate_apply_plan(&plan, &pack.mc)
            .expect("the case-insensitive overlap is a single destination");
    }

    #[test]
    fn mrpack_repair_rejects_case_insensitive_duplicates_within_one_layer() {
        let pack = TestPack::new("mr_repair_case_duplicate");
        let zip_path = pack.root.join("repair-pack.mrpack");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/config/A.toml", b"first"),
                ("overrides/config/a.toml", b"second"),
            ],
        );
        let missing = vec!["config/A.toml".to_string(), "config/a.toml".to_string()];
        let staging = pack.staging.join("overrides");
        let mut budget = crate::import::ExtractBudget::with_limits(100, 100);

        let err = stage_mrpack_repair_overrides(
            &zip_path,
            &missing,
            &staging,
            &pack.mc,
            "modrinth",
            &mut budget,
        )
        .expect_err("two files in one layer cannot target one Windows path");

        assert!(err.contains("Duplicate"), "got: {err}");
        assert!(!pack.mc.join("config/A.toml").exists());
    }

    #[test]
    fn mrpack_repair_restores_independent_base_and_client_files() {
        let pack = TestPack::new("mr_repair_no_overlap");
        let zip_path = pack.root.join("repair-pack.mrpack");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/config/base.toml", b"base"),
                ("client-overrides/config/client.toml", b"client"),
            ],
        );
        let missing = vec![
            "config/base.toml".to_string(),
            "config/client.toml".to_string(),
        ];
        let staging = pack.staging.join("overrides");
        let mut budget = crate::import::ExtractBudget::with_limits(100, 100);

        let installs = stage_mrpack_repair_overrides(
            &zip_path,
            &missing,
            &staging,
            &pack.mc,
            "modrinth",
            &mut budget,
        )
        .expect("ordinary repair staging must succeed");

        assert_eq!(installs.len(), 2);
        assert_eq!(
            std::fs::read(staging.join("config/base.toml")).unwrap(),
            b"base"
        );
        assert_eq!(
            std::fs::read(staging.join("config/client.toml")).unwrap(),
            b"client"
        );
    }

    #[test]
    fn mrpack_repair_rejects_an_override_claimed_by_an_index_file() {
        let pack = TestPack::new("mr_repair_index_collision");
        let zip_path = pack.root.join("repair-pack.mrpack");
        write_test_zip(&zip_path, &[("overrides/mods/Foo.jar", b"override")]);
        let missing = vec!["mods/Foo.jar".to_string()];
        let staging = pack.staging.join("overrides");
        let mut budget = crate::import::ExtractBudget::new();
        budget
            .claim_path("mods/foo.jar")
            .expect("the index file reserves its destination");

        let err = stage_mrpack_repair_overrides(
            &zip_path,
            &missing,
            &staging,
            &pack.mc,
            "modrinth",
            &mut budget,
        )
        .expect_err("an override must not overwrite an index file");

        assert!(err.contains("conflicting"), "got: {err}");
        assert!(!staging.join("mods/Foo.jar").exists());
        assert!(!pack.mc.join("mods/Foo.jar").exists());
    }

    #[test]
    fn mrpack_repair_uses_one_resource_budget_across_both_layers() {
        let pack = TestPack::new("mr_repair_budget");
        let zip_path = pack.root.join("repair-pack.mrpack");
        write_test_zip(
            &zip_path,
            &[
                ("overrides/config/base.toml", b"123456"),
                ("client-overrides/config/client.toml", b"654321"),
            ],
        );
        let missing = vec![
            "config/base.toml".to_string(),
            "config/client.toml".to_string(),
        ];
        let staging = pack.staging.join("overrides");
        let mut budget = crate::import::ExtractBudget::with_limits(10, 10);

        let err = stage_mrpack_repair_overrides(
            &zip_path,
            &missing,
            &staging,
            &pack.mc,
            "modrinth",
            &mut budget,
        )
        .expect_err("both layers must consume the same total budget");

        assert!(err.contains("total limit"), "got: {err}");
        assert_eq!(budget.total(), 6, "the first layer's bytes are accounted");
    }

    #[test]
    fn repair_override_staging_budget_failure_does_not_touch_the_instance() {
        let pack = TestPack::new("repair_budget");
        write(&pack.mods().join("keep.jar"), b"user-content");
        let zip_path = pack.root.join("repair-pack.zip");
        write_test_zip(&zip_path, &[("overrides/mods/big.jar", b"0123456789")]);

        let mut budget = crate::import::ExtractBudget::with_limits(3, 3);
        let staging = pack.staging.join("overrides");
        let err = stage_repair_overrides(
            &zip_path,
            OverridePrefix::CurseForge("overrides/".to_string()),
            &["mods/big.jar".to_string()],
            &staging,
            &pack.mc,
            "curseforge",
            &mut budget,
        )
        .expect_err("the override exceeds the total budget");
        assert!(err.contains("limit"), "got: {err}");
        assert_eq!(
            std::fs::read(pack.mods().join("keep.jar")).unwrap(),
            b"user-content"
        );
        assert!(!staging.join("mods/big.jar").exists());
        assert!(!pack.mc.join("mods/big.jar").exists());
    }

    #[test]
    fn apply_plan_installs_content_and_overrides_and_cleans_staging() {
        let pack = TestPack::new("repair_apply");
        write(&pack.mc.join("config/user.toml"), b"user-config");

        let plan = ApplyPlan {
            installs: vec![
                repair_install(
                    staged(&pack, "mods/new.jar", b"new-mod"),
                    pack.mods().join("new.jar"),
                    true,
                    true,
                ),
                repair_override_install(
                    staged(&pack, "overrides/mods/pack.jar", b"pack-override"),
                    pack.mods().join("pack.jar"),
                ),
            ],
            removals: vec![],
        };

        let outcome = apply_plan(&pack.mc, &plan, &pack.staging, &pack.temp_zip)
            .expect("a prepared plan applies");
        assert_eq!(outcome.mods_installed, 1);
        assert_eq!(outcome.overrides_written, 1);
        assert_eq!(
            std::fs::read(pack.mods().join("new.jar")).unwrap(),
            b"new-mod"
        );
        assert_eq!(
            std::fs::read(pack.mods().join("pack.jar")).unwrap(),
            b"pack-override"
        );
        assert!(pack.index().join("new.voidlauncher.json").is_file());
        assert!(pack.index().join("pack.voidlauncher.json").is_file());
        assert_eq!(
            std::fs::read(pack.mc.join("config/user.toml")).unwrap(),
            b"user-config"
        );
        assert!(!pack.staging.exists(), "successful apply cleans staging");
    }

    #[test]
    fn apply_plan_rolls_back_a_partial_repair_and_keeps_unrelated_files() {
        let pack = TestPack::new("repair_rb");
        write(&pack.mods().join("unrelated.jar"), b"user-mod");
        write(&pack.mc.join("saves/world/level.dat"), b"world");

        let plan = ApplyPlan {
            installs: vec![
                repair_install(
                    staged(&pack, "mods/new.jar", b"new-mod"),
                    pack.mods().join("new.jar"),
                    true,
                    true,
                ),
                repair_install(
                    pack.staging.join("mods/missing.jar"),
                    pack.mods().join("missing.jar"),
                    true,
                    true,
                ),
            ],
            removals: vec![],
        };

        let err = apply_plan(&pack.mc, &plan, &pack.staging, &pack.temp_zip)
            .expect_err("the missing staged file fails the repair");
        assert!(err.contains("rolled back"), "got: {err}");
        assert!(!pack.mods().join("new.jar").exists());
        assert!(
            !pack.index().join("new.voidlauncher.json").exists(),
            "metadata of a rolled-back new file must be removed"
        );
        assert!(!pack.mods().join("missing.jar").exists());
        assert_eq!(
            std::fs::read(pack.mods().join("unrelated.jar")).unwrap(),
            b"user-mod"
        );
        assert_eq!(
            std::fs::read(pack.mc.join("saves/world/level.dat")).unwrap(),
            b"world"
        );
        assert!(!pack.staging.exists());
    }

    #[test]
    fn content_present_recognizes_the_disabled_twin() {
        let pack = TestPack::new("content_present");
        write(&pack.mods().join("late.jar.disabled"), b"disabled");
        assert!(content_present(&pack.mods().join("late.jar")));
        assert!(!content_present(&pack.mods().join("other.jar")));
    }

    #[test]
    fn rollback_removes_directories_created_by_the_operation() {
        let pack = TestPack::new("repair_dirs");
        let created_dir = pack.mc.join("config/new-section");

        let plan = ApplyPlan {
            installs: vec![
                repair_override_install(
                    staged(&pack, "overrides/config/new-section/a.toml", b"a"),
                    created_dir.join("a.toml"),
                ),
                repair_install(
                    pack.staging.join("mods/missing.jar"),
                    pack.mods().join("missing.jar"),
                    true,
                    true,
                ),
            ],
            removals: vec![],
        };

        apply_plan(&pack.mc, &plan, &pack.staging, &pack.temp_zip)
            .expect_err("the last file is missing");
        assert!(
            !created_dir.exists(),
            "an empty directory created by the failed operation must be removed"
        );
    }

    #[test]
    fn repair_refuses_to_replace_content_that_appeared_after_prepare() {
        let pack = TestPack::new("repair_race");
        write(&pack.mods().join("late.jar"), b"user-created");

        let plan = ApplyPlan {
            installs: vec![repair_install(
                staged(&pack, "mods/late.jar", b"repair-content"),
                pack.mods().join("late.jar"),
                true,
                true,
            )],
            removals: vec![],
        };

        let err = apply_plan(&pack.mc, &plan, &pack.staging, &pack.temp_zip)
            .expect_err("a file that appeared after prepare is not replaced");
        assert!(
            err.contains("appeared while the repair was preparing"),
            "got: {err}"
        );
        assert_eq!(
            std::fs::read(pack.mods().join("late.jar")).unwrap(),
            b"user-created"
        );
        assert!(!pack.staging.exists());
    }

    #[test]
    fn sidecar_failure_rolls_back_the_installed_file() {
        let pack = TestPack::new("repair_sidecar");
        let plan = ApplyPlan {
            installs: vec![repair_install(
                staged(&pack, "mods/new.jar", b"new-mod"),
                pack.mods().join("new.jar"),
                true,
                true,
            )],
            removals: vec![],
        };

        fail_next_sidecar();
        let err = apply_plan(&pack.mc, &plan, &pack.staging, &pack.temp_zip)
            .expect_err("the injected sidecar failure aborts the repair");
        assert!(err.contains("injected sidecar failure"), "got: {err}");
        assert!(err.contains("rolled back"), "got: {err}");
        assert!(!pack.mods().join("new.jar").exists());
        assert!(!pack.index().join("new.voidlauncher.json").exists());
        assert!(!pack.staging.exists());
    }

    #[test]
    fn failed_park_keeps_the_original_and_reports_a_rollback() {
        let pack = TestPack::new("repair_park_fail");
        write(&pack.mods().join("old.jar"), b"old-content");
        // A non-empty directory at the park name makes the rename fail after
        // the operation has started.
        write(
            &pack.mods().join("old.jar.voidlauncher-old/blocker"),
            b"block",
        );

        let plan = ApplyPlan {
            installs: vec![repair_install(
                staged(&pack, "mods/old.jar", b"new-content"),
                pack.mods().join("old.jar"),
                true,
                false,
            )],
            removals: vec![],
        };

        let err = apply_plan(&pack.mc, &plan, &pack.staging, &pack.temp_zip)
            .expect_err("the blocked park fails the apply");
        assert!(err.contains("rolled back"), "got: {err}");
        assert_eq!(
            std::fs::read(pack.mods().join("old.jar")).unwrap(),
            b"old-content"
        );
    }

    #[test]
    fn failed_rollback_reports_both_errors_and_preserves_recovery_data() {
        let pack = TestPack::new("rb_failure");
        write(&pack.mods().join("sodium.jar"), b"old-sodium");
        write_sidecar(&pack, "sodium.jar", "curseforge", "5");

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

        fail_next_rollback();
        let err = apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan,
            &pack.staging,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect_err("the injected rollback failure is reported");

        assert!(err.contains("Rollback also failed"), "got: {err}");
        assert!(err.contains("injected rollback failure"), "got: {err}");
        assert!(err.contains("partially modified"), "got: {err}");
        assert!(
            !err.contains("was rolled back"),
            "a failed rollback must not be reported as success: {err}"
        );

        // The injected rollback did nothing: this is exactly the partial state
        // that must not be hidden from the caller.
        assert_eq!(
            std::fs::read(pack.mods().join("sodium.jar")).unwrap(),
            b"new-sodium"
        );
        assert_eq!(
            std::fs::read(pack.mods().join("sodium.jar.voidlauncher-old")).unwrap(),
            b"old-sodium"
        );
        assert!(
            pack.staging.exists(),
            "staging with recovery data must survive a failed rollback"
        );
        let manifest =
            std::fs::read_to_string(pack.staging.join("recovery.txt")).expect("recovery manifest");
        assert!(manifest.contains("sodium.jar"), "got: {manifest}");
        let copies: Vec<PathBuf> = std::fs::read_dir(pack.staging.join("recovery"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("sodium.jar.voidlauncher-old"))
            })
            .collect();
        assert_eq!(
            copies.len(),
            1,
            "the old content must be copied for recovery"
        );
        assert_eq!(std::fs::read(&copies[0]).unwrap(), b"old-sodium");
    }

    #[test]
    fn validate_apply_plan_rejects_duplicate_destinations_and_sidecar_collisions() {
        let pack = TestPack::new("plan_validate");
        let duplicate = ApplyPlan {
            installs: vec![
                repair_install(
                    staged(&pack, "mods/a.jar", b"a"),
                    pack.mods().join("a.jar"),
                    true,
                    true,
                ),
                repair_install(
                    staged(&pack, "mods/A.jar", b"b"),
                    pack.mods().join("A.jar"),
                    true,
                    true,
                ),
            ],
            removals: vec![],
        };
        let err = validate_apply_plan(&duplicate, &pack.mc).expect_err("case-only duplicate");
        assert!(err.contains("Conflicting destinations"), "got: {err}");

        let file_dir = ApplyPlan {
            installs: vec![
                repair_install(
                    pack.staging.join("mods/A"),
                    pack.mods().join("A"),
                    true,
                    true,
                ),
                repair_install(
                    pack.staging.join("mods/a/inner.jar"),
                    pack.mods().join("a/inner.jar"),
                    true,
                    true,
                ),
            ],
            removals: vec![],
        };
        let err = validate_apply_plan(&file_dir, &pack.mc)
            .expect_err("case-only ancestor paths still collide on Windows");
        assert!(err.contains("file and a directory"), "got: {err}");

        let sidecar_collision = ApplyPlan {
            installs: vec![
                repair_install(
                    staged(&pack, "mods/Foo.jar", b"jar"),
                    pack.mods().join("Foo.jar"),
                    true,
                    true,
                ),
                repair_install(
                    staged(&pack, "mods/Foo.zip", b"zip"),
                    pack.mods().join("Foo.zip"),
                    true,
                    true,
                ),
            ],
            removals: vec![],
        };
        let err = validate_apply_plan(&sidecar_collision, &pack.mc)
            .expect_err("two content files cannot share one sidecar");
        assert!(err.contains("metadata entry"), "got: {err}");

        let independent = ApplyPlan {
            installs: vec![
                repair_install(
                    staged(&pack, "mods/a.jar", b"a"),
                    pack.mods().join("a.jar"),
                    true,
                    true,
                ),
                repair_install(
                    staged(&pack, "config/b.toml", b"b"),
                    pack.mc.join("config/b.toml"),
                    false,
                    true,
                ),
            ],
            removals: vec![],
        };
        validate_apply_plan(&independent, &pack.mc).expect("independent paths are fine");
    }

    #[test]
    fn later_updates_never_delete_a_previous_failed_rollback_recovery() {
        let pack = TestPack::new("recovery_sequence");
        write(&pack.mods().join("sodium.jar"), b"old-sodium");
        write_sidecar(&pack, "sodium.jar", "curseforge", "5");
        write(&pack.mods().join("other.jar"), b"old-other");

        // First operation: fails during apply, then its rollback is injected
        // to fail. The recovery copy must stay in its own staging folder.
        let staging1 = operation_staging_dir(&pack.instance, "pack-update");
        write(&staging1.join("mods/sodium.jar"), b"new-sodium");
        let plan1 = ApplyPlan {
            installs: vec![
                content_install(
                    staging1.join("mods/sodium.jar"),
                    pack.mods().join("sodium.jar"),
                    true,
                ),
                content_install(
                    staging1.join("mods/missing.jar"),
                    pack.mods().join("missing.jar"),
                    true,
                ),
            ],
            removals: vec![],
        };
        fail_next_rollback();
        let err1 = apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan1,
            &staging1,
            &pack.temp_zip,
            &sample_lock("6"),
        )
        .expect_err("the first rollback is injected to fail");
        assert!(err1.contains("Rollback also failed"), "got: {err1}");
        let manifest1 = std::fs::read_to_string(staging1.join("recovery.txt"))
            .expect("first recovery manifest");
        assert!(manifest1.contains("sodium.jar"));

        // Second operation: also fails its rollback. It must use a different
        // staging folder and must not overwrite the first recovery data.
        let staging2 = operation_staging_dir(&pack.instance, "pack-update");
        assert_ne!(staging1, staging2, "staging names must be unique");
        write(&staging2.join("mods/other.jar"), b"new-other");
        let plan2 = ApplyPlan {
            installs: vec![
                content_install(
                    staging2.join("mods/other.jar"),
                    pack.mods().join("other.jar"),
                    true,
                ),
                content_install(
                    staging2.join("mods/missing2.jar"),
                    pack.mods().join("missing2.jar"),
                    true,
                ),
            ],
            removals: vec![],
        };
        fail_next_rollback();
        let err2 = apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan2,
            &staging2,
            &pack.temp_zip,
            &sample_lock("7"),
        )
        .expect_err("the second rollback is injected to fail");
        assert!(err2.contains("Rollback also failed"), "got: {err2}");
        assert_eq!(
            std::fs::read_to_string(staging1.join("recovery.txt")).unwrap(),
            manifest1,
            "the first recovery manifest must not change"
        );
        assert!(staging2.join("recovery.txt").is_file());
        assert!(staging1.join("recovery").is_dir());
        assert!(staging2.join("recovery").is_dir());

        // Third operation succeeds and replaces the same file that the first
        // failed operation left partially updated. This also reclaims the
        // in-place park, so only the preserved recovery copy can still hold
        // the original content.
        let staging3 = operation_staging_dir(&pack.instance, "pack-update");
        write(&staging3.join("mods/sodium.jar"), b"final-sodium");
        let plan3 = ApplyPlan {
            installs: vec![content_install(
                staging3.join("mods/sodium.jar"),
                pack.mods().join("sodium.jar"),
                true,
            )],
            removals: vec![],
        };
        apply_and_commit(
            &pack.mc,
            &pack.instance,
            &plan3,
            &staging3,
            &pack.temp_zip,
            &sample_lock("8"),
        )
        .expect("the third operation succeeds");
        assert_eq!(
            std::fs::read(pack.mods().join("sodium.jar")).unwrap(),
            b"final-sodium"
        );
        assert!(!staging3.exists(), "successful staging is removed");
        assert!(
            staging1.join("recovery.txt").is_file(),
            "first recovery must survive later updates"
        );
        assert!(
            staging2.join("recovery.txt").is_file(),
            "second recovery must survive later updates"
        );
        assert_eq!(
            std::fs::read_to_string(staging1.join("recovery.txt")).unwrap(),
            manifest1,
            "later updates must not rewrite the first recovery manifest"
        );
        let recovery_copies: Vec<PathBuf> = std::fs::read_dir(staging1.join("recovery"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("sodium.jar.voidlauncher-old"))
            })
            .collect();
        assert_eq!(recovery_copies.len(), 1);
        assert_eq!(
            std::fs::read(&recovery_copies[0]).unwrap(),
            b"old-sodium",
            "the recovery copy must keep the original content even after the \
             in-place park was reclaimed"
        );
    }

    #[cfg(windows)]
    fn create_junction(link: &Path, target: &Path) -> bool {
        use std::os::windows::process::CommandExt;
        let mut command = std::process::Command::new("cmd");
        command.arg("/C");
        command.raw_arg(format!(
            "mklink /J \"{}\" \"{}\"",
            link.display(),
            target.display()
        ));
        command
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(windows)]
    #[test]
    fn apply_plan_rejects_a_junction_content_folder() {
        let pack = TestPack::new("junction_apply");
        let outside = std::env::temp_dir().join(format!(
            "vl_packlock_junction_outside_{}_{}",
            std::process::id(),
            unique_nonce()
        ));
        std::fs::create_dir_all(&outside).unwrap();
        let link = pack.mc.join("linked-mods");
        if !create_junction(&link, &outside) {
            eprintln!("skipping junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&outside);
            return;
        }

        let plan = ApplyPlan {
            installs: vec![repair_install(
                staged(&pack, "linked-mods/example.jar", b"payload"),
                link.join("example.jar"),
                true,
                true,
            )],
            removals: vec![],
        };
        let err = apply_plan(&pack.mc, &plan, &pack.staging, &pack.temp_zip)
            .expect_err("a junction content folder must be rejected");
        assert!(err.contains("reparse point"), "got: {err}");
        assert!(
            !outside.join("example.jar").exists(),
            "nothing may be written through the junction"
        );

        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(windows)]
    #[test]
    fn install_and_remove_recheck_protected_paths_at_the_file_boundary() {
        let pack = TestPack::new("protected_boundary");

        for rel in [
            "options.txt",
            "saves /world/level.dat",
            "screenshots./shot.png",
        ] {
            let dest = pack.mc.join(rel);
            let staged_file = staged(&pack, "mods/evil.jar", b"evil");
            let mut rb = ApplyRollback::default();
            let install = content_install(staged_file.clone(), dest.clone(), false);
            let err = install_one(&mut rb, &pack.mc, &install)
                .expect_err("install_one must refuse a protected destination");
            assert!(err.contains("protected"), "got: {err}");
            assert!(!dest.exists(), "no file may be written at {rel}");
            assert!(staged_file.exists(), "the staged file stays in staging");
            assert!(rb.moved.is_empty());
        }

        let protected = pack.mc.join("screenshots/shot.png");
        write(&protected, b"user screenshot");
        let mut rb = ApplyRollback::default();
        let err = remove_one(&mut rb, &pack.mc, &protected)
            .expect_err("remove_one must refuse a protected path");
        assert!(err.contains("protected"), "got: {err}");
        assert_eq!(std::fs::read(&protected).unwrap(), b"user screenshot");
        assert!(rb.stashes.is_empty());
    }

    #[test]
    fn apply_plan_rejects_removing_through_a_junction_content_folder() {
        let pack = TestPack::new("junction_remove");
        let outside = std::env::temp_dir().join(format!(
            "vl_packlock_junction_remove_{}_{}",
            std::process::id(),
            unique_nonce()
        ));
        std::fs::create_dir_all(&outside).unwrap();
        write(&outside.join("old.jar"), b"outside-data");
        let link = pack.mc.join("linked-mods");
        if !create_junction(&link, &outside) {
            eprintln!("skipping junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&outside);
            return;
        }

        let plan = ApplyPlan {
            installs: vec![],
            removals: vec![PlannedRemoval {
                path: link.join("old.jar"),
                is_mod: true,
                counted: true,
            }],
        };
        let err = apply_plan(&pack.mc, &plan, &pack.staging, &pack.temp_zip)
            .expect_err("removal through a junction must be rejected");
        assert!(err.contains("reparse point"), "got: {err}");
        assert_eq!(
            std::fs::read(outside.join("old.jar")).unwrap(),
            b"outside-data"
        );

        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn two_mutations_cannot_reserve_the_same_instance() {
        let dir = std::env::temp_dir().join(format!(
            "vl_packlock_guard_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::AppConfig {
            data_dir: dir.clone(),
            ..Default::default()
        };
        let state = crate::AppState {
            config: std::sync::Mutex::new(config),
            auth_state: std::sync::Mutex::new(crate::auth::AuthState::default()),
            launches: crate::launch_state::LaunchRegistry::new(),
            pack_watcher: std::sync::Mutex::new(None),
            active_sessions: std::sync::Mutex::new(HashMap::new()),
            room_state: std::sync::Arc::new(crate::rooms::room_state::RoomStateManager::new(&dir)),
            tailscale: crate::rooms::tailscale::TailscaleManager::new(dir.clone()),
        };

        let first =
            crate::commands::instances::InstanceMutationGuard::acquire(&state, "Pack").unwrap();
        let second = crate::commands::instances::InstanceMutationGuard::acquire(&state, "Pack");
        assert!(
            second.is_err(),
            "a second content mutation must not enter the apply phase"
        );
        assert_eq!(
            state.launches.phase("Pack"),
            Some(crate::launch_state::LaunchPhase::Editing)
        );
        drop(first);
        assert_eq!(state.launches.phase("Pack"), None);
        crate::commands::instances::InstanceMutationGuard::acquire(&state, "Pack")
            .expect("the reservation is released");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_lock_never_overwrites_existing_overrides_ambiguously() {
        let dir = std::env::temp_dir().join(format!(
            "vl_pack_ambiguous_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config/user.toml"), b"user-edit").unwrap();
        std::fs::write(dir.join("config/pack.toml"), b"new-pack-content").unwrap();
        let pack_hash = crate::download::hash_file_sha1(&dir.join("config/pack.toml")).unwrap();

        let mut lock = sample_lock("1");
        lock.overrides = vec!["config/user.toml".to_string()];
        let candidates = [
            "config/user.toml".to_string(),
            "config/pack.toml".to_string(),
        ];
        let new_hashes = BTreeMap::from([
            (
                "config/user.toml".to_string(),
                "different-from-local".to_string(),
            ),
            ("config/pack.toml".to_string(), pack_hash),
        ]);

        // The lock has no hashes: it cannot prove either file is pack-owned.
        // pack.toml already matches the new pack bytes (nothing to write);
        // user.toml differs and must be preserved.
        let (install, skipped) =
            preserve_ambiguous_overrides(&dir, &lock, candidates.into(), &new_hashes);
        assert_eq!(skipped, 1, "only the differing local file is ambiguous");
        assert!(
            install.is_empty(),
            "the matching pack file needs no write: {install:?}"
        );
        assert_eq!(
            std::fs::read(dir.join("config/user.toml")).unwrap(),
            b"user-edit",
            "a user edit must never be replaced by an ambiguous legacy update"
        );

        // A hash-aware lock trusts its recorded hashes: a changed pack file is
        // installed normally.
        lock.override_hashes
            .insert("config/user.toml".into(), "old".into());
        let (install, skipped) = preserve_ambiguous_overrides(
            &dir,
            &lock,
            vec!["config/user.toml".to_string()],
            &new_hashes,
        );
        assert_eq!(skipped, 0);
        assert_eq!(install, vec!["config/user.toml".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_overrides_with_changed_pack_content_are_rewritten() {
        let mut lock = sample_lock("1");
        lock.override_hashes = BTreeMap::from([
            ("config/a.toml".to_string(), "hash-a".to_string()),
            ("config/b.toml".to_string(), "hash-b".to_string()),
        ]);
        let overrides = vec!["config/a.toml".to_string(), "config/b.toml".to_string()];
        let new_hashes = BTreeMap::from([
            ("config/a.toml".to_string(), "hash-a".to_string()),
            ("config/b.toml".to_string(), "hash-b-changed".to_string()),
        ]);

        assert_eq!(
            changed_override_paths(&lock, &overrides, &new_hashes),
            vec!["config/b.toml".to_string()]
        );

        // A lock from an older build has no hashes; rewrite once and record
        // them so the next update can be incremental.
        lock.override_hashes.clear();
        assert_eq!(
            changed_override_paths(&lock, &overrides, &new_hashes),
            overrides
        );
    }

    #[test]
    fn older_release_is_not_a_newer_pack_candidate() {
        assert_eq!(candidate_is_newer("2.0.0", "1.9.0"), VersionOrder::Newer);
        assert_eq!(candidate_is_newer("1.9.0", "2.0.0"), VersionOrder::NotNewer);
        assert_eq!(
            candidate_is_newer("2.0.0-beta.1", "2.0.0"),
            VersionOrder::NotNewer
        );
        assert_eq!(
            candidate_is_newer("2.0.0", "2.0.0-beta.1"),
            VersionOrder::Newer
        );
        assert_eq!(
            candidate_is_newer("Minecraft 1.20.1", "2.0.0"),
            VersionOrder::Unknown
        );
        assert_eq!(candidate_is_newer("", "2.0.0"), VersionOrder::Unknown);
    }

    #[test]
    fn semver_build_metadata_never_changes_precedence() {
        // SemVer 2.0.0 §10: build metadata is ignored when determining
        // precedence. `Version::Ord` includes it, so this is a regression test
        // for the old `>` comparison.
        assert_eq!(
            candidate_is_newer("1.2.3+build.5", "1.2.3+build.4"),
            VersionOrder::NotNewer
        );
        assert_eq!(
            candidate_is_newer("1.2.3+build.4", "1.2.3+build.5"),
            VersionOrder::NotNewer
        );
        assert_eq!(
            candidate_is_newer("1.2.3+build.4", "1.2.3"),
            VersionOrder::NotNewer
        );
        assert_eq!(
            candidate_is_newer("1.2.3", "1.2.3+build.4"),
            VersionOrder::NotNewer
        );
        // The numeric core still decides.
        assert_eq!(
            candidate_is_newer("1.2.4+build.1", "1.2.3+build.9"),
            VersionOrder::Newer
        );
        assert_eq!(
            candidate_is_newer("1.2.3+build.1", "1.2.4+build.9"),
            VersionOrder::NotNewer
        );
        // Prerelease precedence is preserved with build metadata present.
        assert_eq!(
            candidate_is_newer("1.2.3-beta.2+x", "1.2.3-beta.1+x"),
            VersionOrder::Newer
        );
        assert_eq!(
            candidate_is_newer("1.2.3-beta.1+x", "1.2.3+x"),
            VersionOrder::NotNewer
        );
        // Real Modrinth providers encode MC/loader in build metadata:
        // `0.5.1+1.21.1.fabric` is newer than `0.5.0+1.21.1.fabric` by the
        // core version only (build metadata never changes precedence).
        assert_eq!(
            candidate_is_newer("0.5.1+1.21.1.fabric", "0.5.0+1.21.1.fabric"),
            VersionOrder::Newer
        );
        assert_eq!(
            candidate_is_newer("0.5.0+1.20.1.forge", "0.5.0+1.21.1.fabric"),
            VersionOrder::NotNewer
        );
    }

    #[test]
    fn proven_numeric_and_v_prefixed_versions_are_comparable() {
        // Two- and four-component numeric versions.
        assert_eq!(candidate_is_newer("5.4", "5.3"), VersionOrder::Newer);
        assert_eq!(candidate_is_newer("5.3", "5.4"), VersionOrder::NotNewer);
        assert_eq!(
            candidate_is_newer("1.1.8.2", "1.1.8.1"),
            VersionOrder::Newer
        );
        assert_eq!(
            candidate_is_newer("1.1.8.1", "1.1.8.0"),
            VersionOrder::Newer
        );
        assert_eq!(
            candidate_is_newer("1.1.8.0", "1.1.8.1"),
            VersionOrder::NotNewer
        );
        assert_eq!(
            candidate_is_newer("1.1.8", "1.1.8.0"),
            VersionOrder::Unknown,
            "three-component (SemVer) and four-component values are different families"
        );

        // A `v` prefix is the same number, not another format.
        assert_eq!(candidate_is_newer("v40", "v39"), VersionOrder::Newer);
        assert_eq!(candidate_is_newer("V2.0.0", "v1.9.9"), VersionOrder::Newer);
        assert_eq!(
            candidate_is_newer("v1.2.3", "1.2.3"),
            VersionOrder::NotNewer,
            "v1.2.3 and 1.2.3 are the same version"
        );
        assert_eq!(candidate_is_newer("v1.2", "1.1"), VersionOrder::Newer);

        // Same version is never an update.
        assert_eq!(candidate_is_newer("2.0.0", "2.0.0"), VersionOrder::NotNewer);
        assert_eq!(candidate_is_newer("v2", "2"), VersionOrder::NotNewer);
    }

    #[test]
    fn unproven_formats_and_mixed_families_stay_unknown() {
        for (candidate, installed) in [
            // Minecraft/loader suffixes.
            ("1.20.1-forge", "1.20.1"),
            ("1.20.1-fabric", "1.20.1-forge"),
            // Single-letter revision: could be a hotfix or a beta marker.
            ("5.2.1b", "5.2.1a"),
            ("5.2.1b", "5.2.1"),
            // Arbitrary labels and human-readable names.
            ("Sodium-26.3-2.5", "Sodium-26.3-2.4"),
            ("26.10-main-26.1.2", "26.09-main-26.1.2"),
            ("build 17", "build 16"),
            ("APRIL-FOOLS", "APRIL-FOOLS-2"),
            ("", "1.0.0"),
            ("1.0.0", ""),
            // Mixed families are not comparable even though each side is valid.
            ("1.2.3.4", "1.2.3"),
            ("1.2.3", "1.2.3.4"),
            ("1.2.3-beta.1", "1.2"),
            // More than four numeric components.
            ("1.2.3.4.5", "1.2.3.4"),
        ] {
            assert_eq!(
                candidate_is_newer(candidate, installed),
                VersionOrder::Unknown,
                "{candidate:?} vs {installed:?} must not be ordered automatically"
            );
        }
    }

    #[test]
    fn modrinth_accepts_extended_formats_without_downgrades() {
        let installed = mr_installed_version("1", "2026-01-01T00:00:00Z", "release", "v40");
        match modrinth_update_decision(
            &installed,
            &[
                mr_candidate_version("2", "2026-02-01T00:00:00Z", "release", "v41"),
                mr_candidate_version("3", "2026-03-01T00:00:00Z", "release", "v39"),
            ],
            "release",
        ) {
            PackCandidate::Newer(version) => assert_eq!(version.id, "2"),
            other => panic!("expected v41, got {other:?}"),
        }

        let installed = mr_installed_version("4", "2026-01-01T00:00:00Z", "release", "5.3");
        match modrinth_update_decision(
            &installed,
            &[
                mr_candidate_version("5", "2026-02-01T00:00:00Z", "release", "5.10"),
                mr_candidate_version("6", "2026-03-01T00:00:00Z", "release", "5.9"),
            ],
            "release",
        ) {
            PackCandidate::Newer(version) => assert_eq!(version.id, "5"),
            other => panic!("expected 5.10, got {other:?}"),
        }

        // A later publication of an older numeric version is still not an
        // update, and an unparseable candidate blocks the decision instead of
        // being silently skipped.
        let installed = mr_installed_version("7", "2026-01-01T00:00:00Z", "release", "1.1.8.2");
        assert!(matches!(
            modrinth_update_decision(
                &installed,
                &[
                    mr_candidate_version("8", "2026-03-01T00:00:00Z", "release", "1.1.8.1"),
                    mr_candidate_version("9", "2026-02-01T00:00:00Z", "release", "1.1.8.3"),
                ],
                "release"
            ),
            PackCandidate::Newer(version) if version.id == "9"
        ));
        assert!(matches!(
            modrinth_update_decision(
                &installed,
                &[mr_candidate_version(
                    "10",
                    "2026-02-01T00:00:00Z",
                    "release",
                    "1.1.8.3-hotfix"
                )],
                "release"
            ),
            PackCandidate::UnknownVersionOrder
        ));
    }

    #[test]
    fn curseforge_display_names_remain_manual_only() {
        // Real CurseForge display names include the pack name and Minecraft
        // version. Extracting "the first number" would risk installing a pack
        // for another Minecraft version, so the provider stays manual.
        let installed = cf_file_version(
            1,
            1,
            "2026-01-01T00:00:00Z",
            "RLCraft 1.12.2 - Release v2.9.3.zip",
        );
        assert!(matches!(
            curseforge_update_decision(
                &installed,
                &[cf_file_version(
                    2,
                    1,
                    "2026-02-01T00:00:00Z",
                    "RLCraft 1.12.2 - Release v2.9.4.zip"
                )],
                "release"
            ),
            PackCandidate::UnknownVersionOrder
        ));
        let installed = cf_file_version(3, 1, "2026-01-01T00:00:00Z", "All the Mods 9-1.1.1");
        assert!(matches!(
            curseforge_update_decision(
                &installed,
                &[cf_file_version(
                    4,
                    1,
                    "2026-02-01T00:00:00Z",
                    "All the Mods 9-1.1.2"
                )],
                "release"
            ),
            PackCandidate::UnknownVersionOrder
        ));
    }

    #[test]
    fn curseforge_version_order_ignores_publication_dates() {
        let installed = cf_file_version(1, 1, "2026-01-01T00:00:00Z", "2.0.0");
        assert!(matches!(
            curseforge_update_decision(
                &installed,
                &[cf_file_version(2, 1, "2026-02-01T00:00:00Z", "1.9.0")],
                "release"
            ),
            PackCandidate::UpToDate
        ));
        match curseforge_update_decision(
            &installed,
            &[cf_file_version(3, 1, "2025-12-01T00:00:00Z", "2.1.0")],
            "release",
        ) {
            PackCandidate::Newer(file) => assert_eq!(file.id, 3),
            other => panic!("expected semver-newer candidate, got {other:?}"),
        }
        assert!(matches!(
            curseforge_update_decision(
                &installed,
                &[cf_file_version(4, 1, "2026-03-01T00:00:00Z", "2.0.0")],
                "release"
            ),
            PackCandidate::UpToDate
        ));
    }

    #[test]
    fn curseforge_unknown_version_order_is_not_an_update() {
        let installed = cf_file_version(1, 1, "2026-01-01T00:00:00Z", "Minecraft 1.20.1");
        assert!(matches!(
            curseforge_update_decision(
                &installed,
                &[cf_file_version(2, 1, "2026-02-01T00:00:00Z", "1.9.0")],
                "release"
            ),
            PackCandidate::UnknownVersionOrder
        ));
        let installed = cf_file_version(1, 1, "2026-01-01T00:00:00Z", "2.0.0");
        assert!(matches!(
            curseforge_update_decision(
                &installed,
                &[cf_file_version(2, 1, "2026-02-01T00:00:00Z", "build 17")],
                "release"
            ),
            PackCandidate::UnknownVersionOrder
        ));
        let installed = cf_file_version(3, 1, "2026-01-01T00:00:00Z", "");
        assert!(matches!(
            curseforge_update_decision(
                &installed,
                &[cf_file_version(4, 1, "2026-02-01T00:00:00Z", "2.0.0")],
                "release"
            ),
            PackCandidate::UnknownVersionOrder
        ));
    }

    #[test]
    fn curseforge_beta_and_alpha_never_downgrade_on_a_later_date() {
        let installed_beta = cf_file_version(1, 2, "2026-01-01T00:00:00Z", "2.0.0");
        assert!(matches!(
            curseforge_update_decision(
                &installed_beta,
                &[cf_file_version(2, 2, "2026-02-01T00:00:00Z", "1.9.0")],
                "beta"
            ),
            PackCandidate::UpToDate
        ));
        let installed_alpha = cf_file_version(3, 3, "2026-01-01T00:00:00Z", "3.0.0");
        assert!(matches!(
            curseforge_update_decision(
                &installed_alpha,
                &[cf_file_version(4, 3, "2026-02-01T00:00:00Z", "2.0.0")],
                "alpha"
            ),
            PackCandidate::UpToDate
        ));
        // The same versions are not an update even with a different date.
        assert!(matches!(
            curseforge_update_decision(
                &installed_beta,
                &[cf_file_version(5, 2, "2026-03-01T00:00:00Z", "2.0.0")],
                "beta"
            ),
            PackCandidate::UpToDate
        ));
    }

    #[test]
    fn modrinth_beta_and_alpha_never_downgrade_on_a_later_date() {
        let installed_beta = mr_installed_version("1", "2026-01-01T00:00:00Z", "beta", "2.0.0");
        assert!(matches!(
            modrinth_update_decision(
                &installed_beta,
                &[mr_candidate_version(
                    "2",
                    "2026-02-01T00:00:00Z",
                    "beta",
                    "1.9.0"
                )],
                "beta"
            ),
            PackCandidate::UpToDate
        ));
        let installed_alpha = mr_installed_version("3", "2026-01-01T00:00:00Z", "alpha", "3.0.0");
        assert!(matches!(
            modrinth_update_decision(
                &installed_alpha,
                &[mr_candidate_version(
                    "4",
                    "2026-02-01T00:00:00Z",
                    "alpha",
                    "2.0.0"
                )],
                "alpha"
            ),
            PackCandidate::UpToDate
        ));
        assert!(matches!(
            modrinth_update_decision(
                &installed_beta,
                &[mr_candidate_version(
                    "5",
                    "2026-03-01T00:00:00Z",
                    "beta",
                    "2.0.0"
                )],
                "beta"
            ),
            PackCandidate::UpToDate
        ));
    }

    #[test]
    fn modrinth_version_order_ignores_publication_dates() {
        let installed = mr_installed_version("1", "2026-01-01T00:00:00Z", "release", "2.0.0");
        assert!(matches!(
            modrinth_update_decision(
                &installed,
                &[mr_candidate_version(
                    "2",
                    "2026-02-01T00:00:00Z",
                    "release",
                    "1.9.0"
                )],
                "release"
            ),
            PackCandidate::UpToDate
        ));
        match modrinth_update_decision(
            &installed,
            &[mr_candidate_version(
                "3",
                "2025-12-01T00:00:00Z",
                "release",
                "2.1.0",
            )],
            "release",
        ) {
            PackCandidate::Newer(version) => assert_eq!(version.id, "3"),
            other => panic!("expected semver-newer candidate, got {other:?}"),
        }
    }

    #[test]
    fn modrinth_unknown_version_order_is_not_an_update() {
        let installed = mr_installed_version("1", "2026-01-01T00:00:00Z", "beta", "");
        assert!(matches!(
            modrinth_update_decision(
                &installed,
                &[mr_candidate_version(
                    "2",
                    "2026-02-01T00:00:00Z",
                    "beta",
                    "2.0.0"
                )],
                "beta"
            ),
            PackCandidate::UnknownVersionOrder
        ));
        let installed = mr_installed_version("1", "2026-01-01T00:00:00Z", "beta", "2.0.0");
        assert!(matches!(
            modrinth_update_decision(
                &installed,
                &[mr_candidate_version(
                    "2",
                    "2026-02-01T00:00:00Z",
                    "beta",
                    "Minecraft 1.20.1"
                )],
                "beta"
            ),
            PackCandidate::UnknownVersionOrder
        ));
    }
}

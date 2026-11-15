use crate::config::AppConfig;
use crate::content;
use crate::download;
use crate::error::{LauncherError, Result};
use crate::instances::{self, Instance, LoaderType};
use crate::modloaders;
use crate::modrinth;
use crate::modrinth::ModrinthFile;
use crate::verify;
use crate::versions;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::Semaphore;

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
    /// Real bytes completed for the current byte-aware stage (0 = unknown).
    pub bytes_done: u64,
    /// Real byte total when every file size is known (0 = indeterminate).
    pub bytes_total: u64,
}

/// One file that could not be installed during a pack import.
#[derive(Debug, Clone, Serialize)]
pub struct FailedImportFile {
    /// Display name when the provider returned one, otherwise "project:file".
    pub file_name: String,
    pub project_id: Option<u64>,
    pub file_id: Option<u64>,
    /// Human-readable reason (provider error, unsafe file name, bad manifest).
    pub reason: String,
    /// False for manifest entries that carry no ids вЂ” nothing to retry.
    pub retryable: bool,
}

/// Outcome of a pack import. A CurseForge import is allowed to finish with
/// some files missing; the caller must show the failures and offer a retry
/// instead of pretending the pack is complete.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportReport {
    pub total_files: usize,
    pub installed: usize,
    pub failed: Vec<FailedImportFile>,
    /// Entries skipped because manifest.json has no projectID/fileID.
    pub skipped: usize,
    pub warnings: Vec<String>,
}

/// Instance plus the per-file report of how its import went.
#[derive(Debug, Clone, Serialize)]
pub struct ImportOutcome {
    pub instance: Instance,
    pub report: ImportReport,
}

/// Outcome of the best-effort post-import prefetch (loader profile, game
/// files, assets). Every warning means the instance is usable but its first
/// launch still has to download something; the caller must surface that
/// instead of claiming a fully ready install.
#[derive(Debug, Clone, Default)]
pub struct PrepareReport {
    pub warnings: Vec<String>,
}

impl PrepareReport {
    fn warn(&mut self, message: String) {
        self.warnings.push(message);
    }
}

impl ImportReport {
    pub(crate) fn complete(total_files: usize, installed: usize) -> Self {
        Self {
            total_files,
            installed,
            ..Self::default()
        }
    }
}

/// How a pack import should be treated once all files were attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportCompletion {
    /// Nothing to install, or every file landed.
    Complete,
    /// At least one file is missing but the pack is still usable.
    Partial,
    /// Nothing landed: the pack would be a broken shell, so fail the import.
    Failed,
}

pub(crate) fn import_completion(total: usize, installed: usize) -> ImportCompletion {
    if total == 0 || installed >= total {
        ImportCompletion::Complete
    } else if installed > 0 {
        ImportCompletion::Partial
    } else {
        ImportCompletion::Failed
    }
}

pub(crate) fn emit_progress(
    app: Option<&AppHandle>,
    stage: &str,
    current: usize,
    total: usize,
    message: &str,
) {
    emit_progress_bytes(app, stage, current, total, 0, 0, message);
}

/// Same as [`emit_progress`], with byte counters for stages where the real
/// downloaded size is known. The UI uses bytes for a weighted bar instead of
/// treating every file as equally large.
pub(crate) fn emit_progress_bytes(
    app: Option<&AppHandle>,
    stage: &str,
    current: usize,
    total: usize,
    bytes_done: u64,
    bytes_total: u64,
    message: &str,
) {
    if let Some(app) = app {
        let _ = app.emit(
            "import-progress",
            ImportProgressPayload {
                stage: stage.to_string(),
                current,
                total,
                message: message.to_string(),
                bytes_done,
                bytes_total,
            },
        );
    }
}

/// Reject zip slip: ".." components, absolute paths and drive prefixes.
pub(crate) fn check_safe_relative(relative: &str) -> Result<()> {
    if relative.contains('\0') {
        return Err(LauncherError::Instance(
            "Null byte in zip entry path".into(),
        ));
    }
    // Windows-only launcher: a colon anywhere is either a drive prefix
    // (`C:foo`) or an NTFS alternate data stream (`mods/a.jar:evil`). Both
    // must be rejected before the path is joined/written.
    if relative.contains(':') {
        return Err(LauncherError::Instance(format!(
            "Drive prefix or NTFS stream in zip entry path: {}",
            relative
        )));
    }
    if Path::new(relative).is_absolute() {
        return Err(LauncherError::Instance(format!(
            "Absolute path in zip entry: {}",
            relative
        )));
    }
    let normalised = relative.replace('\\', "/");
    // Rooted paths (e.g. "/etc/passwd") are not `is_absolute` on Windows
    // but still escape the base when joined.
    if normalised.starts_with('/') {
        return Err(LauncherError::Instance(format!(
            "Rooted path in zip entry: {}",
            relative
        )));
    }
    for component in normalised.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        // Win32 ignores trailing dots and spaces in a component, so `.. `
        // opens the parent directory just like `..`, and `...`/`. ` collapse
        // to nothing. Compare the form Windows will actually use, not the raw
        // string.
        let trimmed = component.trim_end_matches(['.', ' ']);
        if trimmed.is_empty() || trimmed == ".." {
            return Err(LauncherError::Instance(format!(
                "Path traversal detected in zip entry: {}",
                relative
            )));
        }
    }
    Ok(())
}

/// Reject a remote-supplied file name that could escape the directory it is
/// joined into: must be a bare file name вЂ” no separators, drive prefixes,
/// "." / ".." or null bytes.
pub(crate) fn check_safe_file_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('\0') || name == "." || name == ".." {
        return Err(LauncherError::Instance(format!(
            "Invalid file name in remote metadata: {:?}",
            name
        )));
    }
    if name.contains('/') || name.contains('\\') || name.contains(':') {
        return Err(LauncherError::Instance(format!(
            "File name in remote metadata contains a path: {}",
            name
        )));
    }
    Ok(())
}

/// Validate version strings read from an untrusted pack manifest. They are
/// stored in `instance.json` and later joined into `versions/` / `libraries/`,
/// so reject path separators, drive prefixes and empty loader versions. An
/// empty Minecraft version is tolerated for legacy Prism imports only; the
/// launch path reports it clearly and the user can fix the instance.
pub(crate) fn validate_import_versions(
    mc_version: &str,
    loader_version: Option<&str>,
) -> Result<()> {
    if !mc_version.is_empty() && check_safe_file_name(mc_version).is_err() {
        return Err(LauncherError::Instance(format!(
            "Invalid Minecraft version in pack manifest: {:?}",
            mc_version
        )));
    }
    if let Some(version) = loader_version.filter(|v| !v.is_empty()) {
        if check_safe_file_name(version).is_err() {
            return Err(LauncherError::Instance(format!(
                "Invalid mod loader version in pack manifest: {:?}",
                version
            )));
        }
    }
    Ok(())
}

/// Reject an unsafe relative path from remote metadata (Mojang version JSON
/// libraries, NeoForge data map). Same rules as zip entries вЂ” no "..",
/// absolute/rooted paths, drive prefixes or null bytes вЂ” but with a message
/// naming the actual source.
pub(crate) fn check_safe_remote_path(relative: &str) -> Result<()> {
    if relative.trim().is_empty() {
        return Err(LauncherError::Instance(
            "Empty path in remote metadata".into(),
        ));
    }
    check_safe_relative(relative).map_err(|_| {
        LauncherError::Instance(format!("Unsafe path in remote metadata: {}", relative))
    })
}

/// Stream a zip entry to disk under `base`, verifying no path traversal.
/// File-backed writer: large entries are copied in chunks instead of being
/// buffered in memory (`std::io::copy`).
/// Per-entry and per-operation limits for extraction of untrusted archives.
/// Same order of magnitude as the existing local-import limits; these are
/// deliberately enforced on *actual* bytes read, not on metadata sizes.
pub(crate) const MAX_EXTRACT_ENTRY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const MAX_EXTRACT_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Files (not directory entries) one import/update operation may materialise.
/// Collapses the "millions of empty entries" archive into a hard failure.
pub(crate) const MAX_EXTRACT_FILES: usize = 100_000;
/// Total destination-path metadata a single operation may retain. Combined
/// with the per-path limit this bounds memory for attacker-controlled names.
pub(crate) const MAX_EXTRACT_PATH_BYTES: u64 = 16 * 1024 * 1024;
/// Longest single relative destination path accepted from an archive.
pub(crate) const MAX_EXTRACT_PATH_LEN: usize = 4096;
/// Maximum raw ZIP entries inspected in one archive.
pub(crate) const MAX_ARCHIVE_ENTRIES: usize = 100_000;
/// Embedded (in-memory) pack payload allowed for one import.
pub(crate) const MAX_EMBEDDED_BYTES: u64 = 1024 * 1024 * 1024;

static EXTRACT_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reject archives with an unreasonable number of entries before allocating
/// per-entry metadata or iterating them.
pub(crate) fn check_archive_entry_count(count: usize) -> Result<()> {
    if count > MAX_ARCHIVE_ENTRIES {
        return Err(LauncherError::Instance(format!(
            "Archive contains too many entries ({count}, limit {MAX_ARCHIVE_ENTRIES})"
        )));
    }
    Ok(())
}

/// Canonical collision key for a destination path. Removes `.`/empty
/// components, folds case, and strips trailing dots/spaces that Windows
/// ignores, so two names that resolve to the same file share one key.
pub(crate) fn destination_key(relative: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in relative.replace('\\', "/").split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        // Normalise before the `..` check: Windows strips trailing dots and
        // spaces, so `.. ` must be treated as a traversal, and `...`/`. `
        // collapse to an empty component rather than aliasing a real file.
        let part = part.trim_end_matches(['.', ' ']);
        if part.is_empty() || part == ".." {
            return None;
        }
        parts.push(part.to_lowercase());
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Resource counters shared by every extraction layer of one logical
/// import/update operation. Unlike path claims these counters are global: two
/// layers may legitimately contain the same path, but their bytes, files and
/// path metadata all consume the same budget.
#[derive(Debug)]
struct ExtractResources {
    total: u64,
    total_limit: u64,
    files: usize,
    files_limit: usize,
    path_bytes: u64,
    path_bytes_limit: u64,
}

impl ExtractResources {
    fn reserve_bytes(&self, entry_bytes: u64, entry_limit: u64) -> Result<()> {
        if entry_bytes > entry_limit {
            return Err(LauncherError::Instance(format!(
                "Archive entry exceeds the safe extraction limit ({} bytes)",
                entry_limit
            )));
        }
        if self.total.saturating_add(entry_bytes) > self.total_limit {
            return Err(LauncherError::Instance(
                "Archive extraction exceeds the safe total limit".to_string(),
            ));
        }
        Ok(())
    }

    fn reserve_file(&self) -> Result<()> {
        if self.files >= self.files_limit {
            return Err(LauncherError::Instance(format!(
                "Archive extraction exceeds the safe file count limit ({})",
                self.files_limit
            )));
        }
        Ok(())
    }

    fn commit(&mut self, bytes: u64) {
        self.total = self.total.saturating_add(bytes);
        self.files = self.files.saturating_add(1);
    }
}

/// One extraction layer's path-claim registry plus a handle to the operation's
/// shared resource counters. Layered overrides create a fresh layer (empty
/// `paths`) with `layer()`, so a base/client overlap is allowed while the
/// global byte/file/path limits still apply to the sum of both layers.
#[derive(Debug)]
pub(crate) struct ExtractBudget {
    resources: Arc<Mutex<ExtractResources>>,
    entry_limit: u64,
    paths: HashSet<String>,
}

impl ExtractBudget {
    pub(crate) fn new() -> Self {
        Self::with_limits(MAX_EXTRACT_TOTAL_BYTES, MAX_EXTRACT_ENTRY_BYTES)
    }

    pub(crate) fn with_limits(total_limit: u64, entry_limit: u64) -> Self {
        Self::with_all_limits(
            total_limit,
            entry_limit,
            MAX_EXTRACT_FILES,
            MAX_EXTRACT_PATH_BYTES,
        )
    }

    /// Fully explicit constructor used by regression tests with tiny limits.
    pub(crate) fn with_all_limits(
        total_limit: u64,
        entry_limit: u64,
        files_limit: usize,
        path_bytes_limit: u64,
    ) -> Self {
        Self {
            resources: Arc::new(Mutex::new(ExtractResources {
                total: 0,
                total_limit,
                files: 0,
                files_limit,
                path_bytes: 0,
                path_bytes_limit,
            })),
            entry_limit,
            paths: HashSet::new(),
        }
    }

    /// A new path-claim layer that shares this operation's resource counters.
    pub(crate) fn layer(&self) -> Self {
        Self {
            resources: Arc::clone(&self.resources),
            entry_limit: self.entry_limit,
            paths: HashSet::new(),
        }
    }

    /// A layer with a stricter per-entry limit (embedded payload keeps its
    /// historical 1 GiB cap) but the same operation-wide counters.
    pub(crate) fn layer_with_entry_limit(&self, entry_limit: u64) -> Self {
        Self {
            resources: Arc::clone(&self.resources),
            entry_limit,
            paths: HashSet::new(),
        }
    }

    /// A fresh path-claim layer that still sees the parent's existing claims.
    ///
    /// Two sibling override layers (base and client) must both be checked
    /// against non-override destinations already claimed by the operation,
    /// such as files listed in an mrpack index, while remaining independent of
    /// each other so a client override may legitimately replace a base file.
    pub(crate) fn layer_with_existing_claims(&self) -> Self {
        Self {
            resources: Arc::clone(&self.resources),
            entry_limit: self.entry_limit,
            paths: self.paths.clone(),
        }
    }

    fn resources(&self) -> Result<std::sync::MutexGuard<'_, ExtractResources>> {
        self.resources.lock().map_err(|_| {
            LauncherError::Instance("Archive extraction budget lock failed".to_string())
        })
    }

    pub(crate) fn claim_path(&mut self, relative: &str) -> Result<()> {
        if relative.len() > MAX_EXTRACT_PATH_LEN {
            return Err(LauncherError::Instance(format!(
                "Archive destination path is too long ({} bytes, limit {})",
                relative.len(),
                MAX_EXTRACT_PATH_LEN
            )));
        }
        let key = destination_key(relative).ok_or_else(|| {
            LauncherError::Instance(format!(
                "Invalid or empty archive destination path: {relative}"
            ))
        })?;
        if self.paths.contains(&key)
            || self
                .paths
                .iter()
                .any(|path| key.starts_with(&format!("{path}/")))
            || self
                .paths
                .iter()
                .any(|path| path.starts_with(&format!("{key}/")))
        {
            return Err(LauncherError::Instance(format!(
                "Duplicate or conflicting archive destination: {relative}"
            )));
        }
        let mut resources = self.resources()?;
        let next_path_bytes = resources
            .path_bytes
            .saturating_add(key.len() as u64)
            .saturating_add(1);
        if next_path_bytes > resources.path_bytes_limit {
            return Err(LauncherError::Instance(
                "Archive extraction exceeds the safe path metadata limit".to_string(),
            ));
        }
        resources.path_bytes = next_path_bytes;
        drop(resources);
        self.paths.insert(key);
        Ok(())
    }

    fn reserve_file(&self) -> Result<()> {
        self.resources()?.reserve_file()
    }

    fn reserve_bytes(&self, entry_bytes: u64) -> Result<()> {
        self.resources()?
            .reserve_bytes(entry_bytes, self.entry_limit)
    }

    fn commit(&mut self, bytes: u64) {
        if let Ok(mut resources) = self.resources.lock() {
            resources.commit(bytes);
        }
    }

    /// Account for a non-streamed payload (embedded pack content) as one file.
    pub(crate) fn consume_bytes(&mut self, bytes: u64) -> Result<()> {
        self.reserve_file()?;
        self.reserve_bytes(bytes)?;
        self.commit(bytes);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.resources.lock().map(|r| r.total).unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn files(&self) -> usize {
        self.resources.lock().map(|r| r.files).unwrap_or(0)
    }
}

fn extraction_temp_path(target: &Path) -> PathBuf {
    let unique = EXTRACT_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    target.with_file_name(format!(
        ".voidlauncher-extract-{}-{unique}.tmp",
        std::process::id()
    ))
}

#[cfg(test)]
pub(crate) fn write_extracted(base: &Path, entry_name: &str, reader: &mut dyn Read) -> Result<()> {
    let mut budget = ExtractBudget::new();
    write_extracted_budgeted(base, entry_name, reader, &mut budget)
}

/// Stream one entry to disk with actual-byte limits and a destination
/// collision guard. The file is written to a sibling temp path and renamed
/// only after the entire entry passed the budget, so a rejected entry never
/// leaves a partial file in the instance.
pub(crate) fn write_extracted_budgeted(
    base: &Path,
    entry_name: &str,
    reader: &mut dyn Read,
    budget: &mut ExtractBudget,
) -> Result<()> {
    check_safe_relative(entry_name)?;
    let target = base.join(entry_name);
    // Defense in depth: joined path must stay under base
    // (Path::join replaces base on absolute/drive-relative paths).
    if !target.starts_with(base) {
        return Err(LauncherError::Instance(format!(
            "Zip entry escapes extraction directory: {}",
            entry_name
        )));
    }
    budget.claim_path(entry_name)?;
    budget.reserve_file()?;
    ensure_no_reparse_points(base, &target)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Re-check after directory creation: a component could have appeared as a
    // junction between the first check and `create_dir_all`.
    ensure_no_reparse_points(base, &target)?;
    let temp = extraction_temp_path(&target);
    let mut written = 0u64;
    let result = (|| -> Result<()> {
        let file = std::fs::File::create(&temp)?;
        let mut out = std::io::BufWriter::new(file);
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            written = written.saturating_add(read as u64);
            budget.reserve_bytes(written)?;
            out.write_all(&buffer[..read])?;
        }
        out.flush()?;
        drop(out);
        std::fs::rename(&temp, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
        return result;
    }
    budget.commit(written);
    Ok(())
}

/// Extract a zip entry to disk under `base`, verifying no path traversal.
#[cfg(test)]
fn extract_entry(base: &Path, entry_name: &str, data: &[u8]) -> Result<()> {
    write_extracted(base, entry_name, &mut std::io::Cursor::new(data))
}

pub(crate) fn normalize_zip_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

#[cfg(windows)]
pub(crate) fn is_unsafe_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_REPARSE_POINT. Directory reparse points (junctions,
    // mount points, directory symlinks) can redirect a whole subtree.
    // File symlinks are rejected too. Cloud placeholders are file reparse
    // points but are not followed by rename/write of the file itself, so
    // they are allowed (a synced instance must keep working).
    if meta.file_attributes() & 0x400 == 0 {
        return false;
    }
    meta.is_dir() || meta.file_type().is_symlink()
}

#[cfg(not(windows))]
pub(crate) fn is_unsafe_reparse_point(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Reject a write that would pass through an existing reparse point
/// (Windows junction / mount point / symlink, or a symlink elsewhere).
///
/// The lexical `starts_with(base)` checks cannot see that `mods` is a
/// junction to another directory. Every existing component between `base` and
/// `target` is inspected with `symlink_metadata`, which does not follow links.
/// `base` itself may be a reparse point: it is the operation boundary (for
/// example an instance directory moved by the user), and its resolved target
/// is accepted as the instance.
pub(crate) fn ensure_no_reparse_points(base: &Path, target: &Path) -> std::io::Result<()> {
    let relative = target.strip_prefix(base).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Path is outside the target directory: {}", target.display()),
        )
    })?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if is_unsafe_reparse_point(&meta) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "Refusing to write through a reparse point (junction/symlink): {}",
                            current.display()
                        ),
                    ));
                }
            }
            // A missing component ends the existing part of the path; the
            // directories created below it cannot contain a pre-existing
            // reparse point.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Provider-only sidecar for a file extracted from a pack override into a
/// tracked content folder (mods / resourcepacks / shaderpacks). Skips when a
/// sidecar already exists (the index/manifest download may write a richer one
/// with project_id, and it runs after overrides).
pub(crate) fn write_override_sidecar(mc_dir: &Path, relative: &str, provider: &str) {
    let norm = normalize_zip_path(relative);
    let mut parts = norm.splitn(2, '/');
    let top = match parts.next() {
        Some(t) => t,
        None => return,
    };
    if !matches!(top, "mods" | "resourcepacks" | "shaderpacks") {
        return;
    }
    let rest = match parts.next() {
        Some(r) if !r.contains('/') => r,
        _ => return,
    };
    let lower = rest.to_lowercase();
    if !(lower.ends_with(".jar")
        || lower.ends_with(".zip")
        || lower.ends_with(".jar.disabled")
        || lower.ends_with(".zip.disabled"))
    {
        return;
    }
    let content_dir = mc_dir.join(top);
    if crate::instances::read_sidecar_meta(&content_dir, rest).is_some() {
        return;
    }
    let sidecar = serde_json::json!({
        "provider": provider,
        "downloaded_from_pack": true,
    });
    let path = crate::instances::sidecar_meta_path(&content_dir, rest);
    // A content folder or `.index` junction must never redirect this write
    // outside the instance.
    if ensure_no_reparse_points(mc_dir, &path).is_err() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, sidecar.to_string());
}

/// Extract every entry of a zip archive into `dest`, rejecting zip-slip paths.
pub(crate) fn extract_zip_to_dir(zip_path: &Path, dest: &Path) -> Result<()> {
    let mut budget = ExtractBudget::new();
    extract_zip_to_dir_budgeted(zip_path, dest, &mut budget)
}

/// Extract every entry into `dest` using the caller's shared budget/layer.
pub(crate) fn extract_zip_to_dir_budgeted(
    zip_path: &Path,
    dest: &Path,
    budget: &mut ExtractBudget,
) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    check_archive_entry_count(archive.len())?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        let entry_name = entry.name()?.to_string();
        if entry.is_dir() {
            continue;
        }
        write_extracted_budgeted(dest, &entry_name, &mut entry, budget)?;
    }
    Ok(())
}

/// Client-side import: skip files marked client=unsupported (server-only).
pub(crate) fn mrpack_client_required(file_entry: &serde_json::Value) -> bool {
    !matches!(
        file_entry
            .get("env")
            .and_then(|e| e.get("client"))
            .and_then(|c| c.as_str()),
        Some("unsupported")
    )
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
}

async fn resolve_mrpack_urls(file_entry: &serde_json::Value, path_str: &str) -> Vec<String> {
    // External metadata is untrusted: only URLs that pass the shared
    // downloader policy may reach a request. The downloader validates again
    // immediately before the request, but filtering here avoids forwarding
    // credentials or spoofed hosts at all.
    let mut urls: Vec<String> = file_entry["downloads"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| {
                    let allowed = crate::download::validate_download_url(s).is_ok();
                    if !allowed {
                        tracing::warn!(
                            target: "launcher",
                            "Ignoring an unsafe download URL in mrpack metadata"
                        );
                    }
                    allowed
                })
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
                if crate::download::validate_download_url(&file.url).is_ok() {
                    urls.push(file.url.clone());
                    return urls;
                }
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
                if crate::download::validate_download_url(&file.url).is_ok() {
                    urls.push(file.url.clone());
                    return urls;
                }
            }
        }
    }

    if let Some(pid) = file_entry["project_id"].as_str() {
        if let Ok(versions) = modrinth::get_project_versions(pid).await {
            for version in &versions {
                if let Some(file) = pick_modrinth_file(&version.files, filename, hashes) {
                    if crate::download::validate_download_url(&file.url).is_ok() {
                        urls.push(file.url.clone());
                        return urls;
                    }
                }
            }
        }
    }

    urls
}

fn write_mrpack_sidecar(dest_path: &Path, file_entry: &serde_json::Value) -> std::io::Result<()> {
    let filename = dest_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
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
        // Use the central sidecar naming rule: `.disabled` is stripped before
        // the extension, so a disabled MRPACK file is still discoverable by
        // `read_sidecar_meta`/update checks.
        let sidecar_path = crate::instances::sidecar_meta_path(parent, filename);
        ensure_no_reparse_points(parent, &sidecar_path)?;
        if let Some(index_dir) = sidecar_path.parent() {
            std::fs::create_dir_all(index_dir)?;
        }
        ensure_no_reparse_points(parent, &sidecar_path)?;
        std::fs::write(sidecar_path, sidecar.to_string())?;
    }
    Ok(())
}

/// Validate an mrpack index path and resolve its destination under `mc_dir`,
/// rejecting path traversal, absolute/rooted paths and drive prefixes вЂ” the
/// path comes straight from the pack's `modrinth.index.json` and must not
/// escape the instance directory.
fn mrpack_dest(mc_dir: &Path, path_str: &str) -> Result<PathBuf> {
    if path_str.is_empty() {
        return Err(LauncherError::Instance(
            "Missing path in mrpack file entry".into(),
        ));
    }
    check_safe_relative(path_str)?;
    let dest_path = mc_dir.join(path_str);
    // Defense in depth: Path::join replaces the base entirely on absolute or
    // drive-relative paths, so the joined path must stay under mc_dir.
    if !dest_path.starts_with(mc_dir) {
        return Err(LauncherError::Instance(format!(
            "Mrpack file path escapes the instance directory: {}",
            path_str
        )));
    }
    Ok(dest_path)
}

pub(crate) async fn install_mrpack_file(
    path_str: &str,
    file_entry: &serde_json::Value,
    mc_dir: &Path,
    embedded: &Arc<HashMap<String, Vec<u8>>>,
    with_sidecar: bool,
) -> Result<()> {
    let dest_path = mrpack_dest(mc_dir, path_str)?;
    ensure_no_reparse_points(mc_dir, &dest_path)?;
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    ensure_no_reparse_points(mc_dir, &dest_path)?;

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
        if with_sidecar {
            write_mrpack_sidecar(&dest_path, file_entry)?;
        }
        return Ok(());
    }

    if dest_path.exists() && !expected_sha1.is_empty() {
        let path_buf = dest_path.clone();
        if crate::download::verify_sha1(&path_buf, expected_sha1).unwrap_or(false) {
            if with_sidecar {
                write_mrpack_sidecar(&dest_path, file_entry)?;
            }
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
            if with_sidecar {
                write_mrpack_sidecar(&dest_path, file_entry)?;
            }
            return Ok(());
        }
    }

    Err(LauncherError::Instance(format!(
        "Failed to download {}",
        path_str
    )))
}

pub(crate) fn extract_mrpack_override(entry_name: &str) -> Option<&str> {
    for prefix in ["overrides/", "client-overrides/", "overrides-client/"] {
        if let Some(relative) = entry_name.strip_prefix(prefix) {
            return Some(relative);
        }
    }
    None
}

/// Normalize the `overrides` folder declared by a CurseForge manifest.
/// Returns the archive prefix (with a trailing slash) whose contents belong
/// inside `.minecraft`. The folder name comes from the pack, so it is
/// validated like any other archive path.
pub(crate) fn curseforge_overrides_prefix(manifest: &serde_json::Value) -> Result<String> {
    let raw = manifest
        .get("overrides")
        .and_then(|value| value.as_str())
        .unwrap_or("overrides");
    let normalised = raw.trim().replace('\\', "/");
    let normalised = normalised.trim_end_matches('/');
    if normalised.is_empty() {
        return Ok("overrides/".to_string());
    }
    check_safe_relative(normalised).map_err(|_| {
        LauncherError::Instance(format!("Unsafe CurseForge overrides folder: {raw}"))
    })?;
    if normalised
        .split('/')
        .any(|component| component.is_empty() || component == ".")
    {
        return Err(LauncherError::Instance(format!(
            "Unsafe CurseForge overrides folder: {raw}"
        )));
    }
    Ok(format!("{}/", normalised))
}

const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

fn read_zip_entry_text<R: Read + ?Sized>(
    entry: &mut zip::read::ZipFile<'_, R>,
    limit: u64,
) -> Option<String> {
    if entry.size() > limit {
        return None;
    }
    let mut text = String::new();
    (&mut *entry)
        .take(limit + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > limit {
        return None;
    }
    Some(text)
}

/// Detect modpack format from a zip file by peeking at known manifest files.
pub fn probe_modpack(path: &str) -> Result<ModpackMetadata> {
    // File-backed archive: no need to load potentially multi-hundred-MB packs
    // into memory just to read the manifest entries.
    let file = std::fs::File::open(path)
        .map_err(|e| LauncherError::Instance(format!("Cannot read file: {}", e)))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| LauncherError::Instance(format!("Invalid archive: {}", e)))?;
    check_archive_entry_count(archive.len())?;

    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            archive
                .by_index(i)
                .ok()
                .and_then(|entry| entry.name().ok().map(|name| name.into_owned()))
        })
        .collect();

    // Check Prism/MultiMC
    if names.iter().any(|n| n == "instance.cfg") {
        let mut cfg = String::new();
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                if entry.name().ok().as_deref() == Some("instance.cfg") {
                    cfg = read_zip_entry_text(&mut entry, MAX_MANIFEST_BYTES).unwrap_or_default();
                    break;
                }
            }
        }
        let name = cfg
            .lines()
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
                if entry.name().ok().as_deref() == Some("modrinth.index.json") {
                    index_str =
                        read_zip_entry_text(&mut entry, MAX_MANIFEST_BYTES).unwrap_or_default();
                    break;
                }
            }
        }
        if let Ok(index) = serde_json::from_str::<serde_json::Value>(&index_str) {
            let name = index["name"].as_str().unwrap_or("imported").to_string();
            let summary = index["summary"].as_str().map(|s| s.to_string());
            let mc_version = index["dependencies"]["minecraft"]
                .as_str()
                .map(|s| s.to_string());
            let loader = index["dependencies"]
                .as_object()
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
                if entry.name().ok().as_deref() == Some("manifest.json") {
                    manifest_str =
                        read_zip_entry_text(&mut entry, MAX_MANIFEST_BYTES).unwrap_or_default();
                    break;
                }
            }
        }
        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&manifest_str) {
            let name = manifest["name"].as_str().unwrap_or("imported").to_string();
            let summary = manifest["overrides"].as_str().map(|s| s.to_string());
            let mc_version = manifest["minecraft"]["version"]
                .as_str()
                .map(|s| s.to_string());
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
                if entry.name().ok().as_deref() == Some("instance.json") {
                    inst_str =
                        read_zip_entry_text(&mut entry, MAX_MANIFEST_BYTES).unwrap_or_default();
                    break;
                }
            }
        }
        if let Ok(inst) = serde_json::from_str::<serde_json::Value>(&inst_str) {
            let name = inst["name"].as_str().unwrap_or("imported").to_string();
            let mc = inst["minecraftVersion"].as_str().or_else(|| {
                inst["component"]
                    .as_array()
                    .and_then(|c| c.iter().find(|x| x["type"] == "minecraft"))
                    .and_then(|x| x["version"].as_str())
            });
            let (loader, loader_version) = atl_loader_from_instance_json(&inst, mc.unwrap_or(""));
            return Ok(ModpackMetadata {
                format: ModpackFormat::ATLauncher,
                name,
                mc_version: mc.map(|s| s.to_string()),
                loader: loader_slug(&loader),
                loader_version,
                summary: None,
            });
        }
    }

    Err(LauncherError::Instance(
        "Unrecognized modpack format. Supported: Prism/MultiMC (.zip), Modrinth (.mrpack), CurseForge/FTB (.zip), ATLauncher (.zip)".to_string()
    ))
}

/// Best-effort loader detection from an ATLauncher `instance.json`.
///
/// ATL exports are inconsistent: format v2 stores entries in `components`
/// (some exports use the singular `component`), with a `uid` like
/// `net.minecraftforge`, `net.neoforged` or `net.fabricmc.fabric-loader`;
/// the pack-detail API shape uses a top-level `loader` object instead. Both
/// shapes are probed and keywords are matched on `uid` + `type`.
fn atl_loader_from_instance_json(
    inst: &serde_json::Value,
    mc_version: &str,
) -> (LoaderType, Option<String>) {
    // Some exports store "<mc>-<loader>" (e.g. "1.20.1-47.2.0"); our loader
    // installer expects the bare loader version ("47.2.0").
    let normalize = |v: Option<String>| {
        v.map(|v| {
            v.strip_prefix(&format!("{}-", mc_version))
                .map(|s| s.to_string())
                .unwrap_or(v)
        })
    };
    let keyword_to_loader =
        |text: &str, version: Option<String>| -> Option<(LoaderType, Option<String>)> {
            let t = text.to_lowercase();
            if t.contains("neoforge") {
                Some((LoaderType::NeoForge, normalize(version)))
            } else if t.contains("forge") {
                Some((LoaderType::Forge, normalize(version)))
            } else if t.contains("fabric") || t.contains("quilt") {
                Some((LoaderType::Fabric, normalize(version)))
            } else {
                None
            }
        };

    // Top-level loader object (ATL API pack-detail shape).
    if let Some(t) = inst["loader"]["type"].as_str() {
        let version = inst["loader"]["version"]
            .as_str()
            .or_else(|| inst["loader"]["metadata"]["version"].as_str())
            .or_else(|| inst["loader"]["metadata"]["loader"].as_str())
            .map(|s| s.to_string());
        if let Some(hit) = keyword_to_loader(t, version) {
            return hit;
        }
    }

    // Components array (export v2). `type` may be "minecraft"/"game" for the
    // game itself вЂ” the keyword match naturally skips those.
    for key in ["components", "component"] {
        if let Some(arr) = inst[key].as_array() {
            for x in arr {
                let uid = x["uid"].as_str().unwrap_or("");
                let type_ = x["type"].as_str().unwrap_or("");
                let version = x["version"].as_str().map(|s| s.to_string());
                if let Some(hit) = keyword_to_loader(&format!("{} {}", uid, type_), version) {
                    return hit;
                }
            }
        }
    }

    (LoaderType::Vanilla, None)
}

/// Lowercase loader slug for the wizard's pack preview (matches the
/// Mrpack/CurseForge conventions: "forge", "neoforge", "fabric").
fn loader_slug(lt: &LoaderType) -> Option<String> {
    match lt {
        LoaderType::Vanilla => None,
        other => Some(format!("{:?}", other).to_lowercase()),
    }
}

/// Import a modpack into the instances directory
#[allow(dead_code)] // command layer uses `import_modpack_reserved`; kept for tests/embedders
pub async fn import_modpack(
    instances_dir: &Path,
    path: &str,
    instance_name: &str,
    curseforge_api_key: &str,
    libraries_dir: &Path,
    app: Option<&AppHandle>,
    launches: &crate::launch_state::LaunchRegistry,
) -> Result<ImportOutcome> {
    let (outcome, mut reservation) = import_modpack_reserved(
        instances_dir,
        path,
        instance_name,
        curseforge_api_key,
        libraries_dir,
        app,
        launches,
    )
    .await?;
    reservation.commit()?;
    Ok(outcome)
}

/// Import a pack and keep the name/directory reservation alive. Command
/// callers use this variant so post-import preparation (loader, assets, pack
/// lock) cannot race a rename/delete and a failure still removes only the
/// directory this operation created.
pub(crate) async fn import_modpack_reserved<'a>(
    instances_dir: &Path,
    path: &str,
    instance_name: &str,
    curseforge_api_key: &str,
    libraries_dir: &Path,
    app: Option<&AppHandle>,
    launches: &'a crate::launch_state::LaunchRegistry,
) -> Result<(ImportOutcome, InstanceReservation<'a>)> {
    emit_progress(app, "reading", 0, 1, "Reading archive...");

    // Never merge into an existing instance вЂ” a same-named import would
    // silently overwrite its mods/configs.
    ensure_instance_target_free(instances_dir, instance_name)?;

    // Sniff the archive format cheaply (one pass over the entry names). The
    // archive is file-backed, so no full copy of the pack is held in memory;
    // the format-specific importer below re-opens the file from disk itself.
    let format = {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let names: Vec<String> = (0..archive.len())
            .filter_map(|i| {
                archive
                    .by_index(i)
                    .ok()
                    .and_then(|entry| entry.name().ok().map(|name| name.into_owned()))
            })
            .collect();
        if names.iter().any(|n| n == "instance.cfg") {
            ModpackFormat::Prism
        } else if names.iter().any(|n| n == "modrinth.index.json") {
            ModpackFormat::Modrinth
        } else if names.iter().any(|n| n == "manifest.json") {
            ModpackFormat::CurseForge
        } else if names.iter().any(|n| n == "instance.json") {
            ModpackFormat::ATLauncher
        } else {
            return Err(LauncherError::Instance(
                "Unrecognized modpack format. Supported: Prism/MultiMC (.zip), Modrinth (.mrpack), CurseForge/FTB (.zip), ATLauncher (.zip)".to_string()
            ));
        }
    };

    let (outcome, reservation) = match format {
        ModpackFormat::Prism => {
            let (instance, reservation) = import_with_cleanup_keep(
                instances_dir,
                instance_name,
                launches,
                "import",
                || async {
                    instances::import_prism_pack_reserved(instances_dir, path, instance_name)
                },
            )
            .await?;
            (
                ImportOutcome {
                    report: ImportReport::complete(0, 0),
                    instance,
                },
                reservation,
            )
        }
        ModpackFormat::Modrinth => {
            let (instance, reservation) = import_with_cleanup_keep(
                instances_dir,
                instance_name,
                launches,
                "import",
                || async { import_mrpack(instances_dir, path, instance_name, app).await },
            )
            .await?;
            (
                ImportOutcome {
                    report: ImportReport::complete(0, 0),
                    instance,
                },
                reservation,
            )
        }
        ModpackFormat::CurseForge => {
            let ((instance, report), reservation) = import_with_cleanup_keep(
                instances_dir,
                instance_name,
                launches,
                "import",
                || async {
                    import_curseforge_pack(
                        instances_dir,
                        path,
                        instance_name,
                        curseforge_api_key,
                        libraries_dir,
                        app,
                    )
                    .await
                },
            )
            .await?;
            (ImportOutcome { instance, report }, reservation)
        }
        ModpackFormat::ATLauncher => {
            let (instance, reservation) = import_with_cleanup_keep(
                instances_dir,
                instance_name,
                launches,
                "import",
                || async { import_atlauncher_pack(instances_dir, path, instance_name) },
            )
            .await?;
            (
                ImportOutcome {
                    report: ImportReport::complete(0, 0),
                    instance,
                },
                reservation,
            )
        }
    };

    // No terminal "done" event here: the command layer runs
    // `prepare_instance_files` (loader profile + game files + assets) first
    // and only then emits "done", so the progress UI stays live until the
    // instance is actually ready for its first launch.
    Ok((outcome, reservation))
}

/// Download everything an instance needs for its first launch: the mod-loader
/// profile (when the import/install left it unset), the vanilla version files
/// (client JAR, libraries, natives) and the asset index + assets.
///
/// Called right after a pack import/install so the first launch does not have
/// to fetch hundreds of files while the player waits. Best-effort: a failed
/// step is logged and reported as a warning (the instance itself is already
/// fully imported, and the launch path self-heals anything still missing).
pub async fn prepare_instance_files(
    app: Option<&AppHandle>,
    config: &AppConfig,
    instance: &Instance,
) -> PrepareReport {
    let mut report = PrepareReport::default();
    prepare_loader(app, config, instance, &mut report).await;
    prepare_version_and_assets(app, config, instance, &mut report).await;
    report
}

/// Shared final step for every import path (UI modpack import, store installs,
/// legacy Prism IPC): prepare loader/game/assets, verify the instance is still
/// the one on disk, and only then retire the readiness marker.
///
/// When preparation is incomplete the directory and every imported file are
/// preserved, but the persistent pending marker stays in place so no launch can
/// start against a half-prepared instance. `finish_instance_setup` retries the
/// same preparation later.
pub(crate) async fn finish_import_for_launch(
    app: Option<&AppHandle>,
    config: &AppConfig,
    instance_name: &str,
    mut outcome: ImportOutcome,
    reservation: &mut InstanceReservation<'_>,
) -> Result<ImportOutcome> {
    reservation.set_stage(crate::instance_readiness::SetupStage::Preparing)?;
    let prepare = prepare_instance_files(app, config, &outcome.instance).await;
    outcome.report.warnings.extend(prepare.warnings);
    verify_instance_ready(&config.instances_dir(), instance_name)?;
    if outcome.report.warnings.is_empty() {
        reservation.commit()?;
    } else {
        outcome.report.warnings.push(
            "Instance setup is not complete yet. The launch stays blocked and the remaining files \
             will be downloaded when you finish setup."
                .to_string(),
        );
        reservation.preserve_for_retry();
    }
    Ok(outcome)
}

/// Final publication check: the instance directory must still exist and its
/// metadata must parse and describe the expected name. Only after this does
/// the caller retire the readiness marker.
pub(crate) fn verify_instance_ready(instances_dir: &Path, instance_name: &str) -> Result<()> {
    if !instances_dir.join(instance_name).is_dir() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' disappeared during import preparation.",
            instance_name
        )));
    }
    let instance = instances::get_instance(instances_dir, instance_name).map_err(|e| {
        LauncherError::Instance(format!(
            "Instance '{}' has unreadable metadata after preparation: {}",
            instance_name, e
        ))
    })?;
    if instance.name != instance_name {
        return Err(LauncherError::Instance(format!(
            "Instance metadata describes '{}', but the directory is '{}'.",
            instance.name, instance_name
        )));
    }
    Ok(())
}

/// Install the mod loader when the instance declares one but has no profile
/// saved yet (every importer stores `loader_profile: None` вЂ” the profile is
/// built from the loader's metadata service, not from the pack archive).
async fn prepare_loader(
    app: Option<&AppHandle>,
    config: &AppConfig,
    instance: &Instance,
    report: &mut PrepareReport,
) {
    if instance.loader == LoaderType::Vanilla || instance.loader_profile.is_some() {
        return;
    }
    let loader_str = match instance.loader {
        LoaderType::Fabric => "Fabric",
        LoaderType::Forge => "Forge",
        LoaderType::NeoForge => "NeoForge",
        LoaderType::Vanilla => return,
    };
    let Some(loader_version) = instance.loader_version.clone().filter(|v| !v.is_empty()) else {
        tracing::warn!(
            target: "launcher",
            "{} loader of '{}' has no version set; skipping loader install",
            loader_str, instance.name
        );
        report.warn(format!(
            "{} is required but no loader version is set for this instance.",
            loader_str
        ));
        return;
    };
    emit_progress(
        app,
        "loader",
        0,
        1,
        &format!("Installing {} {}...", loader_str, loader_version),
    );
    match modloaders::install_loader(
        loader_str,
        &instance.mc_version,
        &loader_version,
        &config.libraries_dir(),
        &config.versions_dir(),
        app,
    )
    .await
    {
        Ok(profile) => {
            let mut updated = instance.clone();
            updated.loader_profile = Some(profile);
            if let Err(e) =
                instances::save_instance_existing_reserved(&config.instances_dir(), &updated, None)
            {
                tracing::warn!(
                    target: "launcher",
                    "Failed to save {} profile for '{}': {}",
                    loader_str, instance.name, e
                );
                report.warn(format!(
                    "The {} {} profile could not be saved: {}",
                    loader_str, loader_version, e
                ));
            }
            emit_progress(
                app,
                "loader",
                1,
                1,
                &format!("{} {} installed", loader_str, loader_version),
            );
        }
        Err(e) => {
            tracing::warn!(
                target: "launcher",
                "Failed to install {} {} for '{}': {} (the launch will retry)",
                loader_str, loader_version, instance.name, e
            );
            report.warn(format!(
                "{} {} could not be installed: {}",
                loader_str, loader_version, e
            ));
        }
    }
}

/// Fetch the version manifest for the instance's Minecraft version and
/// download the missing version files + assets, writing the version JSON the
/// launch path expects (same layout `cmd_install_version` produces).
async fn prepare_version_and_assets(
    app: Option<&AppHandle>,
    config: &AppConfig,
    instance: &Instance,
    report: &mut PrepareReport,
) {
    emit_progress(app, "manifest", 0, 1, "Fetching version info...");
    let manifest = match versions::fetch_version_manifest().await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                target: "launcher",
                "Failed to fetch version manifest for '{}': {} (the launch will retry)",
                instance.name, e
            );
            report.warn(format!(
                "The Minecraft version list could not be downloaded: {}",
                e
            ));
            return;
        }
    };
    let Some(version_url) = manifest
        .versions
        .iter()
        .find(|v| v.id == instance.mc_version)
        .map(|v| v.url.clone())
    else {
        tracing::warn!(
            target: "launcher",
            "Version {} of '{}' not found in manifest; skipping file download",
            instance.mc_version, instance.name
        );
        report.warn(format!(
            "Minecraft {} was not found in the version list.",
            instance.mc_version
        ));
        return;
    };
    if !crate::is_allowed_download_host(&version_url) {
        tracing::warn!(
            target: "launcher",
            "Version URL for {} is not in the download allowlist; skipping",
            instance.mc_version
        );
        report.warn(format!(
            "The download source for Minecraft {} is not allowed.",
            instance.mc_version
        ));
        return;
    }
    let version_info = match versions::fetch_version_info(&version_url).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "launcher",
                "Failed to fetch version info for {}: {} (the launch will retry)",
                instance.mc_version, e
            );
            report.warn(format!(
                "Metadata for Minecraft {} could not be downloaded: {}",
                instance.mc_version, e
            ));
            return;
        }
    };
    // Remote metadata names are used as file/directory names below.
    if check_safe_file_name(&version_info.id).is_err()
        || check_safe_file_name(&version_info.assets).is_err()
    {
        tracing::warn!(
            target: "launcher",
            "Version metadata for {} contains unsafe names; skipping file download",
            instance.mc_version
        );
        report.warn(format!(
            "Metadata for Minecraft {} contains unsafe file names.",
            instance.mc_version
        ));
        return;
    }

    // Version files: client JAR, libraries, natives вЂ” only the missing ones.
    let all_files = match versions::collect_downloads(
        &version_info,
        &config.libraries_dir(),
        &config.versions_dir(),
    ) {
        Ok(files) => files,
        Err(e) => {
            tracing::warn!(
                target: "launcher",
                "Version metadata for '{}' is unsafe or invalid: {} (the launch will retry)",
                instance.name, e
            );
            report.warn(format!(
                "Metadata for Minecraft {} is unsafe or invalid: {}",
                instance.mc_version, e
            ));
            return;
        }
    };
    let missing = all_files
        .into_iter()
        .filter(|(_, path, _, _)| !path.exists())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let total = missing.len();
        emit_progress(
            app,
            "files",
            0,
            total,
            &format!("Downloading {} game file(s)...", total),
        );
        if let Err(e) = download::download_files(
            missing,
            |completed, total, bytes_done, bytes_total, _msg| {
                emit_progress_bytes(
                    app,
                    "files",
                    completed,
                    total,
                    bytes_done,
                    bytes_total,
                    &format!("Downloading game files ({}/{})", completed, total),
                );
            },
        )
        .await
        {
            tracing::warn!(
                target: "launcher",
                "Game file download for '{}' failed: {} (the launch will retry)",
                instance.name, e
            );
            report.warn(format!("Game files could not be downloaded: {}", e));
            return;
        }
    }

    // Version JSON вЂ” the launch path reads this file to build the command.
    let version_dir = config.versions_dir().join(&version_info.id);
    if let Err(e) = std::fs::create_dir_all(&version_dir) {
        tracing::warn!(target: "launcher", "Failed to create version dir for '{}': {}", instance.name, e);
        report.warn(format!(
            "The Minecraft version directory could not be created: {}",
            e
        ));
        return;
    }
    let version_json_path = version_dir.join(format!("{}.json", version_info.id));
    if !version_json_path.exists() {
        match serde_json::to_string_pretty(&version_info) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&version_json_path, json) {
                    tracing::warn!(target: "launcher", "Failed to write version JSON for '{}': {}", instance.name, e);
                    report.warn(format!(
                        "The Minecraft version metadata could not be saved: {}",
                        e
                    ));
                }
            }
            Err(e) => {
                tracing::warn!(target: "launcher", "Failed to serialize version JSON for '{}': {}", instance.name, e);
                report.warn(format!(
                    "The Minecraft version metadata could not be serialized: {}",
                    e
                ));
            }
        }
    }

    // Asset index + assets (skipped wholesale when the index is on disk вЂ”
    // same coarse check the launch self-heal uses).
    // Asset index + assets. A present index is validated, not trusted: a
    // truncated JSON or an index whose objects are missing must be restored
    // before the instance is published as ready.
    let index_path = config
        .assets_dir()
        .join("indexes")
        .join(format!("{}.json", version_info.assets));
    let mut asset_index = if index_path.exists() {
        match versions::load_asset_index(&index_path)
            .and_then(|index| verify::validate_asset_index(&index).map(|_| index))
        {
            Ok(index) => Some(index),
            Err(e) => {
                tracing::warn!(
                    target: "launcher",
                    "Local asset index for '{}' is unusable: {}",
                    instance.name, e
                );
                None
            }
        }
    } else {
        None
    };
    if asset_index.is_none() {
        emit_progress(app, "assets", 0, 1, "Downloading asset index...");
        if !crate::is_allowed_download_host(&version_info.asset_index.url) {
            tracing::warn!(
                target: "launcher",
                "Asset index URL for {} is not in the download allowlist; skipping",
                instance.mc_version
            );
            report.warn(format!(
                "The asset download source for Minecraft {} is not allowed.",
                instance.mc_version
            ));
            return;
        }
        let fetched = match versions::fetch_asset_index(&version_info.asset_index.url).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(
                    target: "launcher",
                    "Failed to fetch asset index for {}: {} (the launch will retry)",
                    instance.mc_version, e
                );
                report.warn(format!(
                    "The asset index for Minecraft {} could not be downloaded: {}",
                    instance.mc_version, e
                ));
                return;
            }
        };
        if let Err(e) = verify::validate_asset_index(&fetched) {
            tracing::warn!(
                target: "launcher",
                "Downloaded asset index for '{}' is invalid: {}",
                instance.name, e
            );
            report.warn(format!(
                "The asset index for Minecraft {} is invalid: {}",
                instance.mc_version, e
            ));
            return;
        }
        if let Some(parent) = index_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(target: "launcher", "Failed to create asset index dir for '{}': {}", instance.name, e);
                report.warn(format!(
                    "The asset index directory could not be created: {}",
                    e
                ));
                return;
            }
        }
        match serde_json::to_string_pretty(&fetched) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&index_path, json) {
                    tracing::warn!(target: "launcher", "Failed to write asset index for '{}': {}", instance.name, e);
                    report.warn(format!("The asset index could not be saved: {}", e));
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(target: "launcher", "Failed to serialize asset index for '{}': {}", instance.name, e);
                report.warn(format!("The asset index could not be serialized: {}", e));
                return;
            }
        }
        asset_index = Some(fetched);
    }
    let asset_index = asset_index.expect("asset index was loaded or downloaded");

    // Validate every object; download only what is missing/damaged. Import
    // preparation should leave the instance genuinely launchable, not merely
    // create an index file.
    let check = match verify::check_asset_objects_for_launch(&config.assets_dir(), &asset_index) {
        Ok(check) => check,
        Err(e) => {
            tracing::warn!(target: "launcher", "Asset validation for '{}' failed: {}", instance.name, e);
            report.warn(format!("Minecraft assets could not be validated: {}", e));
            return;
        }
    };
    if !check.is_healthy() {
        emit_progress(
            app,
            "assets",
            0,
            check.issue_count(),
            &format!("Downloading {} asset file(s)...", check.issue_count()),
        );
        let forced: std::collections::HashSet<PathBuf> = check.paths.iter().cloned().collect();
        if let Err(e) = download::download_assets_repair(
            &asset_index,
            &config.assets_dir(),
            &forced,
            |completed, total, bytes_done, bytes_total, _msg| {
                emit_progress_bytes(
                    app,
                    "assets",
                    completed,
                    total,
                    bytes_done,
                    bytes_total,
                    &format!("Downloading assets ({}/{})", completed, total),
                );
            },
        )
        .await
        {
            tracing::warn!(
                target: "launcher",
                "Asset download for '{}' failed: {} (the launch will retry)",
                instance.name, e
            );
            report.warn(format!("Game assets could not be downloaded: {}", e));
            return;
        }
        match verify::check_asset_objects_for_launch(&config.assets_dir(), &asset_index) {
            Ok(recheck) if recheck.is_healthy() => {}
            Ok(recheck) => {
                tracing::warn!(
                    target: "launcher",
                    "Assets for '{}' are still incomplete after download: {} problem(s)",
                    instance.name,
                    recheck.issue_count()
                );
                report.warn(format!(
                    "{} Minecraft asset object(s) are still missing or damaged after the download.",
                    recheck.issue_count()
                ));
                return;
            }
            Err(e) => {
                report.warn(format!("Minecraft assets could not be validated: {}", e));
                return;
            }
        }
    }
    // Virtual/legacy mirror for old versions (1.8 and older) вЂ” same step the
    // version installer performs.
    if let Err(e) =
        download::ensure_virtual_assets(&version_info.assets, &asset_index, &config.assets_dir())
    {
        tracing::warn!(target: "launcher", "Failed to mirror virtual assets for '{}': {}", instance.name, e);
        report.warn(format!("Legacy asset files could not be prepared: {}", e));
    }
}

/// Refuse to import/install into an ALREADY EXISTING instance directory.
///
/// Every import path used to merge files into an instance with the same name
/// (silently overwriting mods/configs, and on the ATLauncher path even
/// wiping the whole directory when a loader install failed). Update flows
/// for managed packs go through the pack-lock updater, never a re-import, so
/// a name collision here is always a user mistake worth stopping.
///
/// A directory left behind by an operation that crashed *before* writing
/// metadata is not a usable instance: [`InstanceReservation::reserve`] parks
/// it under a hidden recovery name, so a retry is not blocked by it.
pub(crate) fn ensure_instance_target_free(instances_dir: &Path, instance_name: &str) -> Result<()> {
    let target = instances_dir.join(instance_name);
    if target.exists() && !crate::instance_readiness::is_incomplete_target(&target) {
        return Err(LauncherError::Instance(format!(
            "An instance named '{}' already exists. Choose a different name for the imported pack.",
            instance_name
        )));
    }
    Ok(())
}

/// Marker file written inside a directory reserved by an import/install.
/// The random token inside proves which operation created the directory; a
/// failing operation may only delete a directory whose marker still carries
/// its own token.
pub(crate) const IMPORT_OWNER_MARKER: &str = ".void-import-owner";

/// Ownership handle for one instance directory created by an import/install.
///
/// `std::fs::create_dir` is the atomic claim: two concurrent operations can
/// never both succeed for the same target. The old "check `exists()` before
/// the import, delete on failure" sequence let a losing operation delete the
/// winner's files (TOCTOU). Here the loser fails before touching anything,
/// and `Drop` removes the directory only while the owner marker proves it is
/// still the directory this operation created.
///
/// The same handle also owns the *readiness* lifecycle:
///
/// - it holds a [`LaunchRegistry`] `Preparing` slot for its whole lifetime, so
///   a launch cannot start between the readiness check and the last file
///   write (and a launch that already won makes the import abort up front);
/// - it writes a persistent pending marker before any content, so an
///   interrupted operation is still not launchable after an app restart;
/// - only [`Self::commit`] retires that marker, after the importer, pack lock
///   and file preparation are all done.
///
/// [`LaunchRegistry`]: crate::launch_state::LaunchRegistry
pub(crate) struct InstanceReservation<'a> {
    target: PathBuf,
    marker: PathBuf,
    token: String,
    operation: &'static str,
    /// `false` for a resume of an already imported instance: a failed retry
    /// must never delete the user's pack files.
    owns_directory: bool,
    committed: bool,
    /// Name reservation. Declared before the launch slot so it is released
    /// first: a waiter can never see a free name while the import slot is
    /// still held (the reverse order would let a launch start and then fail
    /// when it tries to save the instance metadata).
    _name_guard: Option<crate::instances::InstanceNameReservation>,
    /// Launch slot, held while the directory is being written and retired by
    /// `commit` only after the readiness marker is gone.
    prepare: Option<crate::launch_state::PrepareGuard<'a>>,
}

fn launch_busy_error(instance_name: &str, busy: crate::launch_state::LaunchBusy) -> LauncherError {
    use crate::launch_state::LaunchBusy;
    let reason = match busy {
        LaunchBusy::Starting => "it is already starting",
        LaunchBusy::Running => "Minecraft is currently running",
        LaunchBusy::Editing => "another operation is already modifying it",
        LaunchBusy::Preparing => "another installation/preparation is already running for it",
    };
    LauncherError::Instance(format!(
        "Cannot set up instance '{}': {}. Wait for the current operation to finish and try again.",
        instance_name, reason
    ))
}

/// Move an operation-interrupted directory (pending marker, no metadata) out
/// of the way so the user can retry the same import name.
///
/// The files are preserved under a hidden `.void-recovery-*` name instead of
/// being deleted: the archive is the source of truth, but silently discarding
/// whatever the failed attempt produced is still worse than keeping it.
fn park_incomplete_target(instances_dir: &Path, target: &Path, name: &str) -> Result<()> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let parked = instances_dir.join(format!(".void-recovery-{}-{}", safe.trim(), stamp));
    std::fs::rename(target, &parked).map_err(|e| {
        LauncherError::Instance(format!(
            "An interrupted operation left a directory for '{}', and it could not be moved aside ({}). Remove {} manually, then retry.",
            name,
            e,
            target.display()
        ))
    })?;
    tracing::warn!(
        target: "launcher",
        "Interrupted setup for '{}' was moved to {}; the operation can be retried",
        name,
        parked.display()
    );
    Ok(())
}

impl std::fmt::Debug for InstanceReservation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceReservation")
            .field("target", &self.target)
            .field("operation", &self.operation)
            .field("owns_directory", &self.owns_directory)
            .field("committed", &self.committed)
            .finish()
    }
}

impl<'a> InstanceReservation<'a> {
    /// Atomically claim a fresh instance directory and its launch slot.
    ///
    /// `operation` is shown in the UI/recovery message ("import", "install",
    /// "create").
    pub(crate) fn reserve(
        instances_dir: &Path,
        instance_name: &str,
        launches: &'a crate::launch_state::LaunchRegistry,
        operation: &'static str,
    ) -> Result<Self> {
        let name = instance_name.trim();
        if name != instance_name {
            return Err(LauncherError::Instance(format!(
                "Invalid instance name: '{}'",
                instance_name
            )));
        }
        crate::instances::validate_instance_dir_name(name, "Instance name")?;
        std::fs::create_dir_all(instances_dir)?;
        let target = instances_dir.join(name);

        // Reserve the name before inspecting the directory: another operation
        // in this process either holds the name (we fail here) or is finished,
        // so an incomplete directory below is genuinely abandoned.
        let name_guard =
            crate::instances::InstanceNameReservation::acquire(instances_dir, &[name])?;

        // Atomic exclusion against launch/edit. Acquired before touching any
        // existing directory, so an incomplete directory is never moved while
        // a launch (or another mutation) still owns that name.
        let prepare = launches
            .prepare_guard(name)
            .map_err(|busy| launch_busy_error(name, busy))?;

        if target.exists() {
            if crate::instance_readiness::is_incomplete_target(&target) {
                park_incomplete_target(instances_dir, &target, name)?;
            } else {
                return Err(LauncherError::Instance(format!(
                    "An instance named '{}' already exists. Choose a different name for the imported pack.",
                    name
                )));
            }
        }

        // Build the directory with its ownership/readiness markers in a
        // hidden sibling, then rename it into place. A crash at any point
        // leaves either the old state or a hidden `.void-setup-*.tmp`; it can
        // never leave an unmarked directory that looks like a real instance.
        let token = uuid::Uuid::new_v4().simple().to_string();
        let staging =
            instances_dir.join(format!(".void-setup-{}.tmp", uuid::Uuid::new_v4().simple()));
        if let Err(e) = std::fs::create_dir(&staging) {
            return Err(e.into());
        }
        if let Err(e) = std::fs::write(staging.join(IMPORT_OWNER_MARKER), &token) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e.into());
        }
        let pending = crate::instance_readiness::PendingSetup {
            stage: crate::instance_readiness::SetupStage::Importing,
            operation: operation.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            token: token.clone(),
        };
        if let Err(e) = crate::instance_readiness::write_pending(&staging, &pending) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&staging, &target) {
            let _ = std::fs::remove_dir_all(&staging);
            if target.exists() {
                return Err(LauncherError::Instance(format!(
                    "An instance named '{}' already exists. Choose a different name for the imported pack.",
                    name
                )));
            }
            return Err(e.into());
        }
        let marker = target.join(IMPORT_OWNER_MARKER);

        Ok(Self {
            target,
            marker,
            token,
            operation,
            owns_directory: true,
            committed: false,
            _name_guard: Some(name_guard),
            prepare: Some(prepare),
        })
    }

    /// Re-open an interrupted *preparation* (metadata already exists) so the
    /// user can finish setup instead of losing a fully imported pack.
    ///
    /// An interrupted *import* cannot be resumed: extraction may be half done,
    /// so the honest recovery is delete + re-import (the UI offers that path).
    pub(crate) fn resume(
        instances_dir: &Path,
        instance_name: &str,
        launches: &'a crate::launch_state::LaunchRegistry,
        operation: &'static str,
    ) -> Result<Self> {
        let name = instance_name.trim();
        if name != instance_name {
            return Err(LauncherError::Instance(format!(
                "Invalid instance name: '{}'",
                instance_name
            )));
        }
        crate::instances::validate_instance_dir_name(name, "Instance name")?;
        let target = instances_dir.join(name);
        let name_guard =
            crate::instances::InstanceNameReservation::acquire(instances_dir, &[name])?;

        let Some(mut pending) = crate::instance_readiness::read_pending(&target) else {
            return Err(LauncherError::Instance(format!(
                "Instance '{}' is not waiting for setup.",
                name
            )));
        };
        if pending.stage != crate::instance_readiness::SetupStage::Preparing {
            return Err(LauncherError::Instance(format!(
                "The import of '{}' did not finish. Delete the incomplete instance and import the pack again.",
                name
            )));
        }
        if !target.join("instance.json").is_file() && !target.join("instance.cfg").is_file() {
            return Err(LauncherError::Instance(format!(
                "Instance '{}' has no usable metadata; delete it and import the pack again.",
                name
            )));
        }

        let prepare = launches
            .prepare_guard(name)
            .map_err(|busy| launch_busy_error(name, busy))?;

        // Refresh the operation label, but keep the original start time so the
        // UI can still explain when the interrupted operation began.
        pending.operation = operation.to_string();
        crate::instance_readiness::write_pending(&target, &pending)?;

        Ok(Self {
            marker: target.join(IMPORT_OWNER_MARKER),
            target,
            token: pending.token,
            operation,
            owns_directory: false,
            committed: false,
            _name_guard: Some(name_guard),
            prepare: Some(prepare),
        })
    }

    fn marker_matches(&self) -> bool {
        matches!(
            std::fs::read_to_string(&self.marker),
            Ok(token) if token.trim() == self.token
        )
    }

    /// Advance the persisted stage. Called once the importer has written
    /// metadata, so a crash from this point can be resumed.
    pub(crate) fn set_stage(&self, stage: crate::instance_readiness::SetupStage) -> Result<()> {
        let Some(mut pending) = crate::instance_readiness::read_pending(&self.target) else {
            return Err(LauncherError::Instance(format!(
                "The readiness marker for '{}' disappeared during setup.",
                self.target.display()
            )));
        };
        if pending.token != self.token {
            return Err(LauncherError::Instance(format!(
                "The readiness marker for '{}' belongs to another operation.",
                self.target.display()
            )));
        }
        pending.stage = stage;
        crate::instance_readiness::write_pending(&self.target, &pending)
    }

    /// Keep the directory and make the instance launchable: the import,
    /// pack-lock write, preparation and final check all succeeded.
    ///
    /// If retiring the marker fails, the directory is still kept (it contains
    /// user data) but the instance stays blocked: the caller receives an
    /// error instead of a half-published "ready" instance.
    pub(crate) fn commit(&mut self) -> Result<()> {
        if self.committed {
            return Ok(());
        }
        // Never delete the directory after this point, even if the marker
        // cannot be removed.
        self.committed = true;
        let result = crate::instance_readiness::clear_pending(&self.target, &self.token);
        // The owner marker has served its purpose once the instance is
        // published. Failure to remove it is not a readiness problem, but it
        // should not be left behind silently.
        if let Err(e) = std::fs::remove_file(&self.marker) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    target: "launcher",
                    "Could not remove the import owner marker {}: {}",
                    self.marker.display(),
                    e
                );
            }
        }
        // Readiness is decided by the marker; once it is retired the name and
        // slot must be released now, not whenever the caller drops this
        // handle. Name first, launch slot last: a launch can only start once
        // every part of the import lifecycle is finished.
        self._name_guard.take();
        self.prepare.take();
        result.map_err(|e| {
            LauncherError::Instance(format!(
                "Instance '{}' was created, but its setup marker could not be cleared ({}). Close any program using the folder and restart the launcher.",
                self.target.display(),
                e
            ))
        })
    }

    /// Keep an imported instance on disk but leave its readiness marker in
    /// place when preparation could not finish (offline first run, loader
    /// download failure). Launch stays blocked; `cmd_finish_instance_setup`
    /// can retry without deleting the pack.
    pub(crate) fn preserve_for_retry(&mut self) {
        self.committed = true;
        self.owns_directory = false;
        self._name_guard.take();
        self.prepare.take();
    }

    /// Keep the failure from being masked: best-effort rewrite of the pending
    /// marker after a failed cleanup, so the leftover directory is still
    /// refused by the launch gate and visible as "setup incomplete".
    fn keep_gated(&self) {
        let pending = crate::instance_readiness::PendingSetup {
            stage: crate::instance_readiness::SetupStage::Importing,
            operation: self.operation.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            token: self.token.clone(),
        };
        if crate::instance_readiness::read_pending(&self.target).is_none() {
            if let Err(e) = crate::instance_readiness::write_pending(&self.target, &pending) {
                tracing::warn!(
                    target: "launcher",
                    "Could not keep {} marked as incomplete: {}",
                    self.target.display(),
                    e
                );
            }
        }
    }
}

impl Drop for InstanceReservation<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if !self.owns_directory {
            // A resumed preparation failed: preserve every file and keep the
            // persistent gate in place. The user can retry or delete.
            self.keep_gated();
            tracing::warn!(
                target: "launcher",
                "Preparation of {} failed; keeping the instance and its setup marker",
                self.target.display()
            );
            return;
        }
        if !self.marker_matches() {
            // The directory (or its owner marker) is no longer ours. Never
            // delete it and never write a marker into what another operation
            // may now own: leave the directory exactly as we found it.
            tracing::warn!(
                target: "launcher",
                "Not removing {}: the import owner marker changed",
                self.target.display()
            );
            return;
        }
        tracing::warn!(
            target: "launcher",
            "Import failed, removing partially-created instance {}",
            self.target.display()
        );
        if let Err(e) = std::fs::remove_dir_all(&self.target) {
            // `remove_dir_all` can delete the marker and then fail on a locked
            // file, which would expose a partial directory to the launch gate.
            // Rewrite the marker so the leftover stays refused.
            self.keep_gated();
            tracing::warn!(
                target: "launcher",
                "Cleanup of {} failed: {}; the directory stays marked as incomplete",
                self.target.display(),
                e
            );
        }
    }
}

/// Run a format-specific importer against a freshly reserved
/// `instances_dir/<instance_name>` and, if it fails, remove that directory so
/// a failed import never leaves broken/missing files behind.
///
/// The reservation is atomic: an already existing directory (including one
/// created by a concurrent import) is never touched by this operation.
#[allow(dead_code)] // test-only convenience; commands use import_with_cleanup_keep
pub(crate) async fn import_with_cleanup<T, F, Fut>(
    instances_dir: &Path,
    instance_name: &str,
    launches: &crate::launch_state::LaunchRegistry,
    operation: &'static str,
    importer: F,
) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let (value, mut reservation) =
        import_with_cleanup_keep(instances_dir, instance_name, launches, operation, importer)
            .await?;
    reservation.commit()?;
    Ok(value)
}

/// Same as [`import_with_cleanup`], but returns the live reservation so the
/// command can keep the directory protected through loader preparation, pack
/// lock writing and the final instance check. The caller must call
/// [`InstanceReservation::commit`] on success; dropping it cleans up.
pub(crate) async fn import_with_cleanup_keep<'a, T, F, Fut>(
    instances_dir: &Path,
    instance_name: &str,
    launches: &'a crate::launch_state::LaunchRegistry,
    operation: &'static str,
    importer: F,
) -> Result<(T, InstanceReservation<'a>)>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let reservation =
        InstanceReservation::reserve(instances_dir, instance_name, launches, operation)?;
    let value = importer().await?;
    Ok((value, reservation))
}

/// Whether any override was extracted, the embedded index payload, and the
/// normalized destination keys claimed by the override layers.
type MrpackLayers = (bool, HashMap<String, Vec<u8>>, HashSet<String>);

/// Import a Modrinth .mrpack
/// Extract the Modrinth `overrides/` and `client-overrides/` layers under one
/// shared resource budget. Each layer gets a fresh path-claim registry so a
/// client file may legally replace the base file, while bytes/files/metadata
/// are counted for the operation as a whole. Also collects root-level embedded
/// files (index payload) into memory under the same budget.
fn extract_mrpack_layers(
    archive: &mut zip::ZipArchive<std::fs::File>,
    mc_dir: &Path,
    operation_budget: &ExtractBudget,
) -> Result<MrpackLayers> {
    let mut extracted_any = false;
    let mut embedded: HashMap<String, Vec<u8>> = HashMap::new();
    let mut embedded_budget = operation_budget.layer_with_entry_limit(MAX_EMBEDDED_BYTES);
    let mut server_override_budget = operation_budget.layer();
    let mut client_override_budget = operation_budget.layer();
    let mut override_keys: HashSet<String> = HashSet::new();
    check_archive_entry_count(archive.len())?;

    for client_layer in [false, true] {
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            let entry_name = entry.name()?.to_string();

            if entry_name == "modrinth.index.json"
                || entry_name.starts_with("overrides-server/")
                || entry_name.starts_with("server-overrides/")
            {
                continue;
            }

            if let Some(relative) = extract_mrpack_override(&entry_name) {
                let is_client_layer = entry_name.starts_with("client-overrides/")
                    || entry_name.starts_with("overrides-client/");
                if is_client_layer != client_layer {
                    continue;
                }
                if relative.is_empty() {
                    continue;
                }
                check_safe_relative(relative)?;
                extracted_any = true;

                if entry.is_dir() {
                    std::fs::create_dir_all(mc_dir.join(relative))?;
                } else {
                    let budget = if client_layer {
                        &mut client_override_budget
                    } else {
                        &mut server_override_budget
                    };
                    write_extracted_budgeted(mc_dir, relative, &mut entry, budget)?;
                    write_override_sidecar(mc_dir, relative, "modrinth");
                }
                if let Some(key) = destination_key(relative) {
                    override_keys.insert(key);
                }
                continue;
            }

            // Embedded files are collected once, during the server-layer pass.
            if client_layer || entry.is_dir() {
                continue;
            }

            check_safe_relative(&entry_name)?;
            // Embedded files stay in memory: the per-file install tasks below
            // run concurrently against a shared map, and file-backed lookup
            // would require re-architecting the concurrency model. Bound the
            // entries under the same operation-wide budget.
            if entry.size() > MAX_EMBEDDED_BYTES {
                return Err(LauncherError::Instance(
                    "The modpack embeds too much data to install safely".to_string(),
                ));
            }
            let mut buf = Vec::new();
            (&mut entry)
                .take(MAX_EMBEDDED_BYTES + 1)
                .read_to_end(&mut buf)?;
            embedded_budget.consume_bytes(buf.len() as u64)?;
            embedded.insert(normalize_zip_path(&entry_name), buf);
        }
    }
    Ok((extracted_any, embedded, override_keys))
}

pub(crate) async fn import_mrpack(
    instances_dir: &Path,
    path: &str,
    instance_name: &str,
    app: Option<&AppHandle>,
) -> Result<Instance> {
    emit_progress(app, "extracting", 0, 1, "Extracting overrides...");

    // File-backed archive: the pack is not buffered in memory.
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    check_archive_entry_count(archive.len())?;

    // Read index
    let mut index_str = String::new();
    let mut found = false;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.name().ok().as_deref() == Some("modrinth.index.json") {
            index_str = read_zip_entry_text(&mut entry, MAX_MANIFEST_BYTES).unwrap_or_default();
            found = true;
            break;
        }
    }
    if !found {
        return Err(LauncherError::Instance(
            "Missing modrinth.index.json".to_string(),
        ));
    }
    let index: serde_json::Value = serde_json::from_str(&index_str)?;
    let mc_version = index["dependencies"]["minecraft"]
        .as_str()
        .ok_or_else(|| {
            LauncherError::Instance(
                "Missing minecraft dependency in modrinth.index.json".to_string(),
            )
        })?
        .to_string();
    let deps = index["dependencies"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let (loader, loader_version) = detect_mrpack_loader(&deps);
    validate_import_versions(&mc_version, loader_version.as_deref())?;
    let ldr_type = mrpack_loader_type(loader.as_deref())?;

    let target_dir = instances_dir.join(instance_name);
    let mc_dir = target_dir.join(".minecraft");
    std::fs::create_dir_all(&mc_dir)?;
    std::fs::create_dir_all(mc_dir.join("mods"))?;
    std::fs::create_dir_all(mc_dir.join("resourcepacks"))?;
    std::fs::create_dir_all(mc_dir.join("shaderpacks"))?;
    std::fs::create_dir_all(mc_dir.join("config"))?;

    // Extract overrides/ first, then client-overrides/ (and its synonym
    // overrides-client/) so the documented layered-overrides feature works:
    // a file present in both layers is replaced by the client variant. The
    // helper shares one operation-wide resource budget across both layers and
    // the embedded payload while keeping path claims per layer.
    let operation_budget = ExtractBudget::new();
    let (extracted_any, embedded, override_keys) =
        extract_mrpack_layers(&mut archive, &mc_dir, &operation_budget)?;
    let embedded = Arc::new(embedded);

    if !extracted_any {
        tracing::warn!(target: "launcher", "Modrinth pack has no overrides/ folder");
    }

    // Install files listed in modrinth.index.json (embedded, overrides, or download)
    let files = index["files"].as_array().cloned().unwrap_or_default();
    // A malformed pack can list the same destination twice (or with different
    // case on Windows). Concurrent writers would truncate each other, so keep
    // the first entry per normalized, case-folded path.
    let mut seen_paths = std::collections::HashSet::new();
    let client_files: Vec<_> = files
        .iter()
        .filter(|f| mrpack_client_required(f))
        .filter(|f| {
            let path = f["path"].as_str().unwrap_or("");
            if path.is_empty() {
                return true;
            }
            seen_paths.insert(normalize_zip_path(path).to_ascii_lowercase())
        })
        .collect();

    // Index files and extracted overrides share the real Windows destination
    // space. Reject a collision before any concurrent install task starts.
    for file_entry in &client_files {
        if let Some(path) = file_entry["path"].as_str().filter(|path| !path.is_empty()) {
            let key = destination_key(path).ok_or_else(|| {
                LauncherError::Instance(format!(
                    "Invalid or empty archive destination path: {path}"
                ))
            })?;
            let conflicts = override_keys.iter().any(|claimed| {
                key == *claimed
                    || key.starts_with(&format!("{claimed}/"))
                    || claimed.starts_with(&format!("{key}/"))
            });
            if conflicts {
                return Err(LauncherError::Instance(format!(
                    "Duplicate or conflicting archive destination: {path}"
                )));
            }
        }
    }

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
                    install_mrpack_file(&path_str, &file_entry, &mc_dir, &embedded, true).await;
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
                            bytes_done: 0,
                            bytes_total: 0,
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

    instances::save_instance_reserved(instances_dir, &instance, None)?;
    tracing::info!(target: "launcher", "Imported Modrinth pack as '{}' ({} client files, {} mods)", instance_name, total, mod_count);
    Ok(instance)
}

/// One manifest file resolved to a real content class. The CurseForge file
/// API does not expose the project class, so it must come from project
/// metadata; an unresolved project is reported, never installed into `mods/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CfImportTarget {
    pub project_id: u64,
    pub file_id: u64,
    pub class: crate::curseforge::CfContentClass,
}

pub(crate) fn plan_curseforge_manifest_files(
    files: &[serde_json::Value],
    projects: &[crate::curseforge::CfBulkMod],
) -> (Vec<CfImportTarget>, Vec<FailedImportFile>) {
    let mut targets = Vec::new();
    let mut failed = Vec::new();
    for entry in files {
        let (Some(project_id), Some(file_id)) =
            (entry["projectID"].as_u64(), entry["fileID"].as_u64())
        else {
            // The caller counts these as skipped; there is nothing to retry.
            continue;
        };
        match crate::curseforge::content_class_from_bulk(projects, project_id) {
            Some(class) => targets.push(CfImportTarget {
                project_id,
                file_id,
                class,
            }),
            None => failed.push(FailedImportFile {
                file_name: format!("{project_id}:{file_id}"),
                project_id: Some(project_id),
                file_id: Some(file_id),
                reason: "CurseForge project metadata did not identify a supported content class \
                         (mod, resource pack or shader pack); the file was not installed."
                    .to_string(),
                retryable: true,
            }),
        }
    }
    (targets, failed)
}

/// Download one CurseForge mod file into the correct content folder of an
/// instance and write its sidecar. The error carries the file name and a
/// human-readable reason so the import report (and the retry UI) can name the
/// failed file.
pub(crate) async fn install_curseforge_file(
    mc_dir: &Path,
    class: crate::curseforge::CfContentClass,
    pid: u64,
    fid: u64,
    api_key: &str,
    claims: Option<&Mutex<ExtractBudget>>,
) -> std::result::Result<String, (String, String)> {
    let content_dir = mc_dir.join(class.folder());
    if let Err(e) = std::fs::create_dir_all(&content_dir) {
        return Err((
            format!("{}:{}", pid, fid),
            format!("Cannot create {} folder: {}", class.folder(), e),
        ));
    }
    let fallback = format!("{}:{}", pid, fid);
    let cf_file = crate::curseforge::get_mod_file(pid, fid, api_key)
        .await
        .map_err(|e| (fallback.clone(), format!("CurseForge API error: {}", e)))?;
    let display = if cf_file.display_name.trim().is_empty() {
        fallback.clone()
    } else {
        cf_file.display_name.clone()
    };

    // The file name comes from remote metadata and is joined into a content
    // folder вЂ” reject any path component.
    if let Err(e) = check_safe_file_name(&cf_file.file_name) {
        return Err((display, format!("Unsafe file name: {}", e)));
    }
    // The manifest filename is only known from remote metadata, so claim it
    // here: a mod that lands on the same destination as an extracted override
    // (or another manifest entry) must be reported, not silently overwrite it.
    if let Some(claims) = claims {
        let rel = format!("{}/{}", class.folder(), cf_file.file_name);
        let mut guard = claims.lock().map_err(|_| {
            (
                display.clone(),
                "Destination registry lock failed".to_string(),
            )
        })?;
        if let Err(e) = guard.claim_path(&rel) {
            return Err((display, e.to_string()));
        }
    }
    let dest_path = content_dir.join(&cf_file.file_name);
    ensure_no_reparse_points(mc_dir, &dest_path)
        .map_err(|e| (display.clone(), format!("Unsafe destination: {}", e)))?;
    if let Err(e) =
        crate::curseforge::download_mod_file(pid, fid, &cf_file, api_key, &dest_path).await
    {
        return Err((display, format!("Download failed: {}", e)));
    }

    // Sidecar: unified writer, with the CF file id and the verified hash so
    // update checks can compare this file against the CF file list later.
    let pid_str = pid.to_string();
    let fid_str = fid.to_string();
    let hash = cf_file.sha1_hash().map(|s| s.to_string());
    let _ = content::write_sidecar(
        &content_dir,
        &cf_file.file_name,
        &content::SidecarWrite {
            provider: "curseforge",
            content_type: class.content_type(),
            project_id: Some(&pid_str),
            project_name: Some(&display),
            version_id: None,
            // CurseForge has no separate version field; the display name
            // (e.g. "jei-1.20.1-forge-15.21.0.138") is the closest thing to
            // a real version and beats the "${file.jarVersion}" placeholder
            // many JARs ship with.
            version_number: Some(&display),
            file_id: Some(&fid_str),
            hash: hash.as_deref(),
        },
    );

    Ok(display)
}

/// Retry a subset of a CurseForge import (the files reported as failed by a
/// previous import). Best-effort: the returned report lists what still could
/// not be downloaded; the instance itself is never removed here.
pub(crate) async fn retry_curseforge_import_files(
    instances_dir: &Path,
    instance_name: &str,
    files: &[(u64, u64)],
    api_key: &str,
) -> Result<ImportReport> {
    let instance_dir = instances_dir.join(instance_name);
    let mc_dir = instance_dir.join(".minecraft");
    if !instance_dir.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' does not exist",
            instance_name
        )));
    }
    for dir in ["mods", "resourcepacks", "shaderpacks"] {
        std::fs::create_dir_all(mc_dir.join(dir))?;
    }

    // Resolve the project class once for the whole retry. A failed metadata
    // request is a per-file failure with a retryable reason, not a silent
    // fallback to `mods/`.
    let mut project_ids: Vec<u64> = files.iter().map(|(pid, _)| *pid).collect();
    project_ids.sort_unstable();
    project_ids.dedup();
    let projects = match crate::curseforge::get_mods_by_ids_chunked(&project_ids, api_key).await {
        Ok(projects) => projects,
        Err(e) => {
            let total = files.len();
            return Ok(ImportReport {
                total_files: total,
                installed: 0,
                failed: files
                    .iter()
                    .map(|(pid, fid)| FailedImportFile {
                        file_name: format!("{pid}:{fid}"),
                        project_id: Some(*pid),
                        file_id: Some(*fid),
                        reason: format!(
                            "CurseForge project metadata is unavailable, so the content class \
                             could not be verified: {e}"
                        ),
                        retryable: true,
                    })
                    .collect(),
                skipped: 0,
                warnings: Vec::new(),
            });
        }
    };

    let total = files.len();
    let mut report = ImportReport::complete(total, 0);
    for (pid, fid) in files {
        let Some(class) = crate::curseforge::content_class_from_bulk(&projects, *pid) else {
            report.failed.push(FailedImportFile {
                file_name: format!("{pid}:{fid}"),
                project_id: Some(*pid),
                file_id: Some(*fid),
                reason: "CurseForge project metadata did not identify a supported content class \
                         (mod, resource pack or shader pack); the file was not installed."
                    .to_string(),
                retryable: true,
            });
            continue;
        };
        match install_curseforge_file(&mc_dir, class, *pid, *fid, api_key, None).await {
            Ok(_) => report.installed += 1,
            Err((name, reason)) => report.failed.push(FailedImportFile {
                file_name: name,
                project_id: Some(*pid),
                file_id: Some(*fid),
                reason,
                retryable: true,
            }),
        }
    }
    report.total_files = total;
    Ok(report)
}

/// Import a CurseForge modpack
pub(crate) async fn import_curseforge_pack(
    instances_dir: &Path,
    path: &str,
    instance_name: &str,
    curseforge_api_key: &str,
    _libraries_dir: &Path,
    app: Option<&AppHandle>,
) -> Result<(Instance, ImportReport)> {
    emit_progress(app, "extracting", 0, 1, "Extracting overrides...");

    // File-backed archive: the pack is not buffered in memory.
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    check_archive_entry_count(archive.len())?;

    // Read manifest
    let mut manifest_str = String::new();
    let mut found = false;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.name().ok().as_deref() == Some("manifest.json") {
            manifest_str = read_zip_entry_text(&mut entry, MAX_MANIFEST_BYTES).unwrap_or_default();
            found = true;
            break;
        }
    }
    if !found {
        return Err(LauncherError::Instance("Missing manifest.json".to_string()));
    }
    let manifest: serde_json::Value = serde_json::from_str(&manifest_str)?;
    let mc_version = manifest["minecraft"]["version"]
        .as_str()
        .ok_or_else(|| {
            LauncherError::Instance("Missing minecraft version in manifest.json".to_string())
        })?
        .to_string();

    let (loader, loader_version) = manifest["minecraft"]["modLoaders"]
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|v| v["primary"].as_bool().unwrap_or(false))
                .or_else(|| arr.first())
        })
        .map(|v| {
            let id = v["id"].as_str().unwrap_or("");
            let parts: Vec<&str> = id.splitn(2, '-').collect();
            let loader = parts.first().copied();
            // "forge-1.20.1-47.2.0" -> "47.2.0"
            let version = parts.get(1).and_then(|p| p.rsplit('-').next());
            (loader, version)
        })
        .unwrap_or((None, None));

    validate_import_versions(&mc_version, loader_version)?;

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

    // CurseForge packs may store their files in a custom folder declared by
    // manifest.json (default: "overrides"). Ignoring that field silently
    // installs a pack without its configs and scripts.
    let overrides_prefix = curseforge_overrides_prefix(&manifest)?;

    // Extract overrides/ with path traversal protection
    let mut extract_budget = ExtractBudget::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        let entry_name = entry.name()?.to_string();

        if entry_name == "manifest.json" {
            continue;
        }

        if let Some(relative) = entry_name.strip_prefix(&overrides_prefix) {
            if relative.is_empty() {
                continue;
            }
            check_safe_relative(relative)?;

            if entry.is_dir() {
                std::fs::create_dir_all(mc_dir.join(relative))?;
            } else {
                write_extracted_budgeted(&mc_dir, relative, &mut entry, &mut extract_budget)?;
                write_override_sidecar(&mc_dir, relative, "curseforge");
            }
        }
    }

    // Override destinations and downloaded mod destinations share the real
    // Windows filesystem. One registry handles both: a download that resolves
    // to an existing override path is reported as a conflict instead of
    // silently replacing the override.
    let claims = Arc::new(Mutex::new(extract_budget));

    // Download mods listed in the files array via CurseForge API (concurrent)
    let files = manifest["files"].as_array().cloned().unwrap_or_default();
    let total = files.len();
    let mut report = ImportReport::complete(total, total);
    if total > 0 {
        // The file endpoint does not expose the project class. Resolve it once
        // for all projects so resource packs and shaders never land in mods/.
        let mut project_ids: Vec<u64> = files
            .iter()
            .filter_map(|entry| entry["projectID"].as_u64())
            .collect();
        project_ids.sort_unstable();
        project_ids.dedup();
        let projects = match crate::curseforge::get_mods_by_ids_chunked(
            &project_ids,
            curseforge_api_key,
        )
        .await
        {
            Ok(projects) => projects,
            Err(e) => {
                tracing::warn!(
                    target: "launcher",
                    "CurseForge project metadata request failed; content will not be guessed: {}",
                    e
                );
                Vec::new()
            }
        };
        let (targets, classification_failed) = plan_curseforge_manifest_files(&files, &projects);
        let skipped = files
            .iter()
            .filter(|entry| {
                entry["projectID"].as_u64().is_none() || entry["fileID"].as_u64().is_none()
            })
            .count();
        let total_tasks = targets.len();

        let semaphore = Arc::new(Semaphore::new(4));
        let completed = Arc::new(AtomicUsize::new(classification_failed.len()));
        let in_progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let succeeded = Arc::new(AtomicUsize::new(0));
        let failed_ids = Arc::new(Mutex::new(classification_failed));
        let mut tasks = Vec::with_capacity(total_tasks);

        // Initial progress
        emit_progress(
            app,
            "downloading-mods",
            0,
            total,
            &format!("Preparing {} files...", total),
        );

        for target in targets {
            let sem = semaphore.clone();
            let mc_dir = mc_dir.clone();
            let api_key = curseforge_api_key.to_string();
            let app_owned = app.cloned();
            let completed = completed.clone();
            let in_progress = in_progress.clone();
            let succeeded = succeeded.clone();
            let failed_ids = failed_ids.clone();
            let claims = claims.clone();
            let pid = target.project_id;
            let fid = target.file_id;
            let class = target.class;

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
                        message: format!("Downloading {} files ({} active)...", total, inflight),
                        bytes_done: 0,
                        bytes_total: 0,
                    });
                }

                let result = install_curseforge_file(
                    &mc_dir,
                    class,
                    pid,
                    fid,
                    &api_key,
                    Some(claims.as_ref()),
                )
                .await;

                in_progress.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

                let (last_name, file_ok) = match &result {
                    Ok(name) => {
                        succeeded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        (name.clone(), true)
                    }
                    Err((name, reason)) => {
                        tracing::warn!(target: "launcher", "CurseForge {} install failed ({}:{}): {}", class.label(), pid, fid, reason);
                        if let Ok(mut failed) = failed_ids.lock() {
                            failed.push(FailedImportFile {
                                file_name: name.clone(),
                                project_id: Some(pid),
                                file_id: Some(fid),
                                reason: reason.clone(),
                                retryable: true,
                            });
                        }
                        (name.clone(), false)
                    }
                };

                // Emit completion progress
                if let Some(ref a) = app_owned {
                    let inflight = in_progress.load(std::sync::atomic::Ordering::Relaxed);
                    let ok = succeeded.load(std::sync::atomic::Ordering::Relaxed);
                    let msg = if file_ok {
                        format!("Downloaded: {} ({}/{} installed, {} active)", last_name, ok, total, inflight)
                    } else {
                        format!("Failed: {} ({}/{} installed, {} active)", last_name, ok, total, inflight)
                    };
                    let _ = a.emit("import-progress", ImportProgressPayload {
                        stage: "downloading-mods".into(),
                        current: done,
                        total,
                        message: msg,
                        bytes_done: 0,
                        bytes_total: 0,
                    });
                }

                Ok::<_, String>(())
            }));
        }

        // Wait for all downloads to complete
        join_all(tasks).await;
        let failed_list = failed_ids.lock().map(|f| f.clone()).unwrap_or_default();
        let ok = succeeded.load(std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            target: "launcher",
            "CurseForge content install: {} succeeded, {} failed, {} skipped (no IDs)",
            ok,
            failed_list.len(),
            skipped
        );

        // A pack with at least one installed file is usable: keep it and let
        // the caller show the failed list with a retry. Only a pack where
        // nothing at all landed is unrecoverable вЂ” return an error so the
        // caller's cleanup removes the empty shell.
        if import_completion(total, ok) == ImportCompletion::Failed {
            let preview: Vec<String> = failed_list
                .iter()
                .take(3)
                .map(|f| format!("{}: {}", f.file_name, f.reason))
                .collect();
            let details = if preview.is_empty() {
                String::new()
            } else {
                format!(" First errors: {}.", preview.join("; "))
            };
            let skipped_note = if skipped > 0 {
                format!(" {} manifest entries had no projectID/fileID.", skipped)
            } else {
                String::new()
            };
            return Err(LauncherError::Instance(format!(
                "CurseForge import failed: none of the {} pack files could be installed.{}{}",
                total, details, skipped_note
            )));
        }

        report = ImportReport {
            total_files: total,
            installed: ok,
            failed: failed_list,
            skipped,
            warnings: Vec::new(),
        };
    }

    let ldr_type = match loader {
        Some("fabric") => LoaderType::Fabric,
        Some("forge") => LoaderType::Forge,
        Some("neoforge") => LoaderType::NeoForge,
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

    instances::save_instance_reserved(instances_dir, &instance, None)?;

    tracing::info!(target: "launcher", "Imported CurseForge pack as '{}' ({} of {} mods) - loader install pending", instance_name, report.installed, total);
    Ok((instance, report))
}

/// ATLauncher mod type that changes how the downloaded archive is placed.
/// Upstream `ModType` also has `texturepackextract` and
/// `resourcepackextract`; all other types copy the archive verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtlModKind {
    Normal,
    Extract,
    TexturePackExtract,
    ResourcePackExtract,
}

#[derive(Debug, Clone)]
pub(crate) struct AtlModSpec {
    /// Candidate archive file names from the manifest (`file`, then `name`).
    pub(crate) names: Vec<String>,
    pub(crate) kind: AtlModKind,
    pub(crate) extract_to: Option<String>,
    pub(crate) extract_folder: Option<String>,
}

/// Build the extraction description for one ATLauncher mod record. Returns
/// `None` for ordinary archive types. Shared by local `.zip` import and the
/// ATLauncher catalog installer so both use identical upstream semantics.
pub(crate) fn atl_extract_spec(
    kind: &str,
    extract_to: Option<&str>,
    extract_folder: Option<&str>,
) -> Option<AtlModSpec> {
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "extract" => AtlModKind::Extract,
        "texturepackextract" => AtlModKind::TexturePackExtract,
        "resourcepackextract" => AtlModKind::ResourcePackExtract,
        _ => return None,
    };
    Some(AtlModSpec {
        names: Vec::new(),
        kind,
        extract_to: extract_to.map(str::to_string),
        extract_folder: extract_folder.map(str::to_string),
    })
}

fn atl_mod_specs(instance: &serde_json::Value) -> Vec<AtlModSpec> {
    let Some(mods) = instance["mods"].as_array() else {
        return Vec::new();
    };
    mods.iter()
        .filter_map(|entry| {
            let type_name = entry["type"].as_str().unwrap_or("").to_ascii_lowercase();
            let kind = match type_name.as_str() {
                "extract" => AtlModKind::Extract,
                "texturepackextract" => AtlModKind::TexturePackExtract,
                "resourcepackextract" => AtlModKind::ResourcePackExtract,
                _ => AtlModKind::Normal,
            };
            let mut names = Vec::new();
            for key in ["file", "name"] {
                if let Some(name) = entry[key]
                    .as_str()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                {
                    if !names.iter().any(|existing| existing == name) {
                        names.push(name.to_string());
                    }
                }
            }
            if names.is_empty() {
                return None;
            }
            Some(AtlModSpec {
                names,
                kind,
                extract_to: entry["extractTo"].as_str().map(str::to_string),
                extract_folder: entry["extractFolder"].as_str().map(str::to_string),
            })
        })
        .collect()
}

/// Basename an ATLauncher export may use for a mod archive. Only root-level
/// files and direct `mods/` children are considered, so a same-named config
/// deep in the tree is never mistaken for the declared mod archive.
fn atl_source_basename(entry_name: &str) -> Option<&str> {
    let relative = entry_name.strip_prefix("mods/").unwrap_or(entry_name);
    (!relative.is_empty() && !relative.contains('/')).then_some(relative)
}

fn atl_archive_spec<'a>(specs: &'a [AtlModSpec], entry_name: &str) -> Option<&'a AtlModSpec> {
    let basename = atl_source_basename(entry_name)?;
    specs.iter().find(|spec| {
        spec.kind != AtlModKind::Normal
            && spec
                .names
                .iter()
                .any(|name| name.eq_ignore_ascii_case(basename))
    })
}

fn atl_extract_folder_components(raw: Option<&str>) -> Result<Vec<String>> {
    let raw = raw.unwrap_or("").trim();
    if raw.is_empty() {
        return Err(LauncherError::Instance(
            "ATLauncher extract mod has no extractFolder".to_string(),
        ));
    }
    let normalised = normalize_zip_path(raw);
    let normalised = normalised.trim_matches('/');
    if normalised.is_empty() || normalised == "." {
        return Ok(Vec::new());
    }
    check_safe_relative(normalised)?;
    let components: Vec<String> = normalised
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(|part| part.to_string())
        .collect();
    if components.is_empty() {
        return Err(LauncherError::Instance(format!(
            "ATLauncher extractFolder is empty: {raw}"
        )));
    }
    Ok(components)
}

fn atl_strip_folder(entry_name: &str, folder: &[String]) -> Option<String> {
    let normalised = normalize_zip_path(entry_name);
    let parts: Vec<&str> = normalised
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.len() <= folder.len() {
        return None;
    }
    for (part, expected) in parts.iter().zip(folder.iter()) {
        if !part.eq_ignore_ascii_case(expected) {
            return None;
        }
    }
    Some(parts[folder.len()..].join("/"))
}

fn atl_copy_entry_to_file<R: Read + ?Sized>(
    entry: &mut zip::read::ZipFile<'_, R>,
    dest: &Path,
    max_bytes: u64,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(dest)?;
    let mut out = std::io::BufWriter::new(file);
    let mut written = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = entry.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        written = written.saturating_add(read as u64);
        if written > max_bytes {
            return Err(LauncherError::Instance(
                "ATLauncher mod archive exceeds the safe extraction limit".to_string(),
            ));
        }
        out.write_all(&buffer[..read])?;
    }
    out.flush()?;
    Ok(())
}

/// Remove an unpublished ATL staging tree on every return path.
struct AtlStageGuard(PathBuf);

impl Drop for AtlStageGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Publish every file from `stage` under `mc_dir`, preserving the relative
/// path. Paths were already claimed in the shared budget while they were
/// extracted; this step only performs the atomic same-volume move.
fn atl_publish_staged(stage: &Path, mc_dir: &Path) -> Result<()> {
    fn walk(root: &Path, dir: &Path, mc_dir: &Path) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if is_unsafe_reparse_point(&meta) {
                return Err(LauncherError::Instance(format!(
                    "ATLauncher extraction produced an unsafe link: {}",
                    path.display()
                )));
            }
            if meta.is_dir() {
                walk(root, &path, mc_dir)?;
                continue;
            }
            let relative = path.strip_prefix(root).map_err(|_| {
                LauncherError::Instance("ATLauncher staging path escaped its root".to_string())
            })?;
            let relative_text = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            check_safe_relative(&relative_text)?;
            let dest = mc_dir.join(relative);
            ensure_no_reparse_points(mc_dir, &dest)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            ensure_no_reparse_points(mc_dir, &dest)?;
            std::fs::rename(&path, &dest)?;
            write_override_sidecar(mc_dir, &relative_text, "atlauncher");
        }
        Ok(())
    }
    walk(stage, stage, mc_dir)
}

/// Expand one ATLauncher `extract`/`texturepackextract`/`resourcepackextract`
/// archive into the instance. `source` is a file inside the ATL staging tree.
pub(crate) fn atl_extract_mod_archive(
    source: &Path,
    spec: &AtlModSpec,
    staging: &Path,
    mc_dir: &Path,
    final_budget: &mut ExtractBudget,
) -> Result<()> {
    let mut nested = zip::ZipArchive::new(std::fs::File::open(source)?)?;
    check_archive_entry_count(nested.len())?;
    let (destination_prefix, folder): (&str, Vec<String>) = match spec.kind {
        AtlModKind::Extract => {
            let to = spec
                .extract_to
                .as_deref()
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    LauncherError::Instance(
                        "ATLauncher extract mod has no extractTo destination".to_string(),
                    )
                })?;
            let prefix = match to.as_str() {
                "root" => "",
                "mods" => "mods",
                "coremods" => "coremods",
                other => {
                    return Err(LauncherError::Instance(format!(
                        "Unsupported ATLauncher extractTo destination: {other}"
                    )))
                }
            };
            let folder = atl_extract_folder_components(spec.extract_folder.as_deref())?;
            (prefix, folder)
        }
        AtlModKind::TexturePackExtract => ("texturepacks/extracted", Vec::new()),
        AtlModKind::ResourcePackExtract => ("resourcepacks/extracted", Vec::new()),
        AtlModKind::Normal => return Ok(()),
    };

    let nested_stage = staging.join(format!("nested-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&nested_stage)?;
    let mut matched_any = false;
    for i in 0..nested.len() {
        let mut entry = nested
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let entry_name = entry.name()?.to_string();
        check_safe_relative(&entry_name)?;
        // For `extract`, upstream copies only the extractFolder subtree; the
        // folder is a path inside the archive, not a destination folder.
        let relative = if folder.is_empty() {
            Some(normalize_zip_path(&entry_name))
        } else {
            atl_strip_folder(&entry_name, &folder)
        };
        let Some(relative) = relative else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        matched_any = true;
        let final_relative = if destination_prefix.is_empty() {
            relative
        } else {
            format!("{destination_prefix}/{relative}")
        };
        write_extracted_budgeted(&nested_stage, &final_relative, &mut entry, final_budget)?;
    }
    if !matched_any {
        return Err(LauncherError::Instance(format!(
            "ATLauncher extract mod does not contain its extractFolder '{}'",
            spec.extract_folder.as_deref().unwrap_or("")
        )));
    }
    atl_publish_staged(&nested_stage, mc_dir)?;
    let _ = std::fs::remove_dir_all(&nested_stage);
    Ok(())
}

/// Import an ATLauncher instance
fn import_atlauncher_pack(
    instances_dir: &Path,
    path: &str,
    instance_name: &str,
) -> Result<Instance> {
    // File-backed archive: the pack is not buffered in memory.
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    check_archive_entry_count(archive.len())?;

    // Read instance.json for metadata
    let mut inst_str = String::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.name().ok().as_deref() == Some("instance.json") {
            inst_str = read_zip_entry_text(&mut entry, MAX_MANIFEST_BYTES).unwrap_or_default();
            break;
        }
    }

    let mut mod_specs = Vec::new();
    let (mc_version, loader, loader_version);
    if let Ok(inst) = serde_json::from_str::<serde_json::Value>(&inst_str) {
        mod_specs = atl_mod_specs(&inst);
        mc_version = inst["minecraftVersion"]
            .as_str()
            .or_else(|| {
                inst["component"]
                    .as_array()
                    .and_then(|c| c.iter().find(|x| x["type"] == "minecraft"))
                    .and_then(|x| x["version"].as_str())
            })
            .unwrap_or("1.20.1")
            .to_string();
        let detected = atl_loader_from_instance_json(&inst, &mc_version);
        loader = detected.0;
        loader_version = detected.1;
    } else {
        mc_version = "1.20.1".to_string();
        loader = LoaderType::Vanilla;
        loader_version = None;
    }

    validate_import_versions(&mc_version, loader_version.as_deref())?;

    let target_dir = instances_dir.join(instance_name);
    let mc_dir = target_dir.join(".minecraft");
    std::fs::create_dir_all(&mc_dir)?;

    // All ATL work happens inside the instance so every rename stays on the
    // same volume; the guard removes unpublished staging on every error path.
    let staging = mc_dir.join(format!(
        ".voidlauncher-atl-stage-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&staging)?;
    let _stage_guard = AtlStageGuard(staging.clone());

    // One shared resource budget for normal files and every nested archive.
    // Each nested archive has its own path-claim registry only where needed;
    // final destination paths all live in `final_budget` so conflicts between
    // archives and outer files are still rejected.
    let mut final_budget = ExtractBudget::new();

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        let entry_name = entry.name()?.to_string();
        if entry_name == "instance.json" || entry.is_dir() {
            continue;
        }
        check_safe_relative(&entry_name)?;

        if let Some(spec) = atl_archive_spec(&mod_specs, &entry_name) {
            if spec.kind != AtlModKind::Normal {
                // The archive is an ATL installation input, not content: it is
                // expanded according to extractTo/extractFolder and the source
                // file itself is consumed.
                let source = staging.join(format!("source-{i}.zip"));
                atl_copy_entry_to_file(&mut entry, &source, MAX_EXTRACT_ENTRY_BYTES)?;
                atl_extract_mod_archive(&source, spec, &staging, &mc_dir, &mut final_budget)?;
                let _ = std::fs::remove_file(&source);
                continue;
            }
        }

        write_extracted_budgeted(&mc_dir, &entry_name, &mut entry, &mut final_budget)?;
        write_override_sidecar(&mc_dir, &entry_name, "atlauncher");
    }

    let now = chrono::Utc::now().to_rfc3339();
    let instance = Instance {
        name: instance_name.to_string(),
        mc_version,
        loader,
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

    instances::save_instance_reserved(instances_dir, &instance, None)?;
    tracing::info!(target: "launcher", "Imported ATLauncher pack as '{}'", instance_name);
    Ok(instance)
}

/// Map a detected mrpack loader key to our LoaderType. Quilt is rejected
/// with a clear error вЂ” Quilt mods are not always Fabric-compatible, so a
/// silent conversion would produce a broken instance.
pub(crate) fn mrpack_loader_type(loader: Option<&str>) -> Result<LoaderType> {
    Ok(match loader {
        Some("neoforge") => LoaderType::NeoForge,
        Some("forge") => LoaderType::Forge,
        Some("fabric-loader") | Some("fabric") => LoaderType::Fabric,
        Some("quilt-loader") => {
            return Err(LauncherError::Instance(
                "This pack requires Quilt, which is not supported by VoidLauncher.".to_string(),
            ));
        }
        _ => LoaderType::Vanilla,
    })
}

/// Deterministic loader detection from an mrpack manifest's dependencies
/// object. Keys can be "fabric-loader", "forge", "neoforge", "quilt-loader"
/// вЂ” and JSON key order is not guaranteed, so we probe known keys in
/// priority order instead of taking the first non-minecraft key.
pub(crate) fn detect_mrpack_loader(
    deps: &serde_json::Map<String, serde_json::Value>,
) -> (Option<String>, Option<String>) {
    let loader = [
        "neoforge",
        "forge",
        "fabric-loader",
        "fabric",
        "quilt-loader",
    ]
    .iter()
    .find(|k| deps.contains_key(**k))
    .map(|s| s.to_string());
    let version = loader
        .as_ref()
        .and_then(|l| deps.get(l))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (loader, version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_relative_allows_normal_paths() {
        assert!(check_safe_relative("mods/foo.jar").is_ok());
        assert!(check_safe_relative("config/backslash\\dir\\file.cfg").is_ok());
        assert!(check_safe_relative("file.txt").is_ok());
    }

    #[test]
    fn safe_relative_rejects_traversal() {
        assert!(check_safe_relative("../evil.jar").is_err());
        assert!(check_safe_relative("mods/../../evil.jar").is_err());
        assert!(check_safe_relative("mods/..\\..\\evil.jar").is_err());
    }

    #[test]
    fn safe_relative_normalizes_windows_trailing_dots_and_spaces() {
        // Win32 strips trailing dots/spaces before resolving a component, so
        // every one of these names the parent directory on disk.
        for bad in [
            ".. ",
            "..\\ ",
            "mods/.. ",
            "mods\\.. ",
            "mods/...",
            "mods/.. .",
            "mods/. ",
            ". /evil.jar",
            "mods/../evil.jar ",
            "mods\\.. \\evil.jar",
        ] {
            assert!(
                check_safe_relative(bad).is_err(),
                "Windows-normalised traversal must be rejected: {bad:?}"
            );
            assert!(
                destination_key(bad).is_none(),
                "the collision key must also reject {bad:?}"
            );
        }
        // Legitimate file names that merely end in a dot/space still map to
        // their Windows form instead of being treated as traversal.
        assert!(check_safe_relative("mods/example.").is_ok());
        assert!(check_safe_relative("mods/example ").is_ok());
        assert_eq!(
            destination_key("mods/example."),
            Some("mods/example".to_string())
        );
        assert_eq!(
            destination_key("Saves.\\World \\level.dat"),
            Some("saves/world/level.dat".to_string())
        );
    }

    #[test]
    fn safe_relative_rejects_absolute_and_drive_paths() {
        assert!(check_safe_relative("/etc/passwd").is_err());
        assert!(check_safe_relative("\\Windows\\system32").is_err());
        assert!(check_safe_relative("C:/evil.exe").is_err());
        assert!(check_safe_relative("C:\\evil.exe").is_err());
        assert!(check_safe_relative("C:evil.exe").is_err());
        // NTFS alternate data stream (Windows would attach it to `a.jar`).
        assert!(check_safe_relative("mods/a.jar:evil").is_err());
    }

    fn extraction_test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vl_extract_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn extract_bytes(
        base: &Path,
        budget: &mut ExtractBudget,
        name: &str,
        bytes: &[u8],
    ) -> Result<()> {
        write_extracted_budgeted(base, name, &mut std::io::Cursor::new(bytes), budget)
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

    fn temp_extract_files(root: &Path) -> Vec<PathBuf> {
        fn collect(root: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(root) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(".voidlauncher-extract-"))
                {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        collect(root, &mut out);
        out
    }

    #[test]
    fn extraction_budget_counts_actual_bytes_and_rejects_over_total() {
        let dir = extraction_test_dir("total");
        let mut budget = ExtractBudget::with_limits(10, 6);
        extract_bytes(&dir, &mut budget, "mods/a.jar", &[1u8; 5]).unwrap();
        extract_bytes(&dir, &mut budget, "mods/b.jar", &[2u8; 5]).unwrap();
        assert_eq!(budget.total(), 10);
        assert!(extract_bytes(&dir, &mut budget, "mods/c.jar", &[3u8; 1]).is_err());
        assert!(dir.join("mods/a.jar").is_file());
        assert!(dir.join("mods/b.jar").is_file());
        assert!(!dir.join("mods/c.jar").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extraction_budget_rejects_large_entry_without_touching_old_file() {
        let dir = extraction_test_dir("entry");
        let target = dir.join("mods").join("foo.jar");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"old-content").unwrap();
        let mut budget = ExtractBudget::with_limits(100, 5);
        let error = extract_bytes(&dir, &mut budget, "mods/foo.jar", &[9u8; 6]).unwrap_err();
        assert!(error.to_string().contains("limit"), "got: {error}");
        assert_eq!(std::fs::read(&target).unwrap(), b"old-content");
        assert!(
            temp_extract_files(&dir).is_empty(),
            "a rejected entry must not leave a temp file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extraction_rejects_case_insensitive_destination_collisions() {
        let dir = extraction_test_dir("case");
        let mut budget = ExtractBudget::with_limits(1000, 1000);
        extract_bytes(&dir, &mut budget, "mods/Foo.jar", b"first").unwrap();
        let error = extract_bytes(&dir, &mut budget, "mods/foo.jar", b"second").unwrap_err();
        assert!(error.to_string().contains("conflicting"), "got: {error}");
        assert_eq!(std::fs::read(dir.join("mods/Foo.jar")).unwrap(), b"first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn override_prefixes_cannot_silently_overwrite_the_same_destination() {
        let dir = extraction_test_dir("prefix-collision");
        let mut budget = ExtractBudget::with_limits(1000, 1000);
        let first = extract_mrpack_override("overrides/mods/Foo.jar").expect("first override");
        let second =
            extract_mrpack_override("client-overrides/mods/foo.jar").expect("second override");
        extract_bytes(&dir, &mut budget, first, b"first").unwrap();
        let error = extract_bytes(&dir, &mut budget, second, b"second").unwrap_err();
        assert!(error.to_string().contains("conflicting"), "got: {error}");
        assert_eq!(std::fs::read(dir.join("mods/Foo.jar")).unwrap(), b"first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_and_override_paths_share_one_collision_registry() {
        let dir = extraction_test_dir("index-vs-override");
        let mut budget = ExtractBudget::with_limits(1000, 1000);
        // The override is extracted first; the MRPACK/CF index path is claimed
        // before downloads start, against the same registry.
        extract_bytes(&dir, &mut budget, "mods/Foo.jar", b"override").unwrap();
        let error = budget.claim_path("mods/foo.jar").unwrap_err();
        assert!(error.to_string().contains("conflicting"), "got: {error}");
        assert_eq!(
            std::fs::read(dir.join("mods/Foo.jar")).unwrap(),
            b"override"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extraction_replaces_an_existing_file_only_after_success() {
        let dir = extraction_test_dir("replace");
        let target = dir.join("mods").join("foo.jar");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"old").unwrap();
        let mut budget = ExtractBudget::with_limits(100, 100);
        extract_bytes(&dir, &mut budget, "mods/foo.jar", b"new-content").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new-content");
        assert!(temp_extract_files(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn extraction_rejects_a_pre_existing_junction_destination() {
        let base = extraction_test_dir("junction");
        let outside = std::env::temp_dir().join(format!(
            "vl_extract_junction_outside_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&outside).unwrap();
        let link = base.join("mods");
        if !create_junction(&link, &outside) {
            eprintln!("skipping junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&outside);
            let _ = std::fs::remove_dir_all(&base);
            return;
        }

        let mut budget = ExtractBudget::with_limits(1000, 1000);
        let error = extract_bytes(&base, &mut budget, "mods/example.jar", b"payload").unwrap_err();
        assert!(error.to_string().contains("reparse point"), "got: {error}");
        assert!(
            !outside.join("example.jar").exists(),
            "extraction must not follow the junction"
        );

        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn extraction_does_not_reject_hard_links() {
        let dir = extraction_test_dir("hardlink");
        let original = dir.join("original.bin");
        std::fs::write(&original, b"old").unwrap();
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        let link = dir.join("mods/link.jar");
        if std::fs::hard_link(&original, &link).is_err() {
            eprintln!("skipping hard-link test: filesystem does not support hard links");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let mut budget = ExtractBudget::with_limits(1000, 1000);
        extract_bytes(&dir, &mut budget, "mods/link.jar", b"new").unwrap();
        assert_eq!(std::fs::read(&link).unwrap(), b"new");
        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"old",
            "replacing the hard link must not rewrite the other link"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn override_sidecar_does_not_follow_an_index_junction() {
        let base = extraction_test_dir("sidecar-junction");
        std::fs::create_dir_all(base.join("mods")).unwrap();
        let outside = std::env::temp_dir().join(format!(
            "vl_extract_sidecar_junction_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&outside).unwrap();
        let index = base.join("mods/.index");
        if !create_junction(&index, &outside) {
            eprintln!("skipping junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&outside);
            let _ = std::fs::remove_dir_all(&base);
            return;
        }

        write_override_sidecar(&base, "mods/example.jar", "curseforge");
        assert!(
            !outside.join("example.voidlauncher.json").exists(),
            "the sidecar must not be written through the `.index` junction"
        );

        let _ = std::fs::remove_dir(&index);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn extraction_collapses_dot_segments_and_windows_trailing_dots() {
        let dir = extraction_test_dir("dot-segments");
        let mut budget = ExtractBudget::with_limits(1000, 1000);
        extract_bytes(&dir, &mut budget, "mods/Foo.jar", b"first").unwrap();
        let error = extract_bytes(&dir, &mut budget, "mods/./Foo.jar", b"second").unwrap_err();
        assert!(error.to_string().contains("conflicting"), "got: {error}");
        let error = extract_bytes(&dir, &mut budget, "mods/Foo.jar.", b"third").unwrap_err();
        assert!(error.to_string().contains("conflicting"), "got: {error}");
        assert_eq!(std::fs::read(dir.join("mods/Foo.jar")).unwrap(), b"first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extraction_rejects_duplicate_and_file_directory_conflicts() {
        let dir = extraction_test_dir("dir-conflict");
        let mut budget = ExtractBudget::with_limits(1000, 1000);
        extract_bytes(&dir, &mut budget, "mods/A", b"file").unwrap();
        let error = extract_bytes(&dir, &mut budget, "mods/A/inner.txt", b"nested").unwrap_err();
        assert!(error.to_string().contains("conflicting"), "got: {error}");
        assert_eq!(std::fs::read(dir.join("mods/A")).unwrap(), b"file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extraction_allows_independent_paths() {
        let dir = extraction_test_dir("independent");
        let mut budget = ExtractBudget::with_limits(1000, 1000);
        extract_bytes(&dir, &mut budget, "mods/a.jar", b"a").unwrap();
        extract_bytes(&dir, &mut budget, "config/b.cfg", b"b").unwrap();
        assert_eq!(std::fs::read(dir.join("mods/a.jar")).unwrap(), b"a");
        assert_eq!(std::fs::read(dir.join("config/b.cfg")).unwrap(), b"b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn embedded_style_budget_uses_actual_bytes_not_declared_size() {
        let mut budget = ExtractBudget::with_limits(10, 6);
        budget.reserve_bytes(5).unwrap();
        budget.commit(5);
        budget.reserve_bytes(5).unwrap();
        budget.commit(5);
        assert!(budget.reserve_bytes(1).is_err());
        assert!(ExtractBudget::with_limits(100, 5).reserve_bytes(6).is_err());
    }

    fn open_probe_zip(entries: &[(&str, &[u8])], name: &str) -> (PathBuf, std::fs::File) {
        let path = temp_probe_zip(name, entries);
        let file = std::fs::File::open(&path).unwrap();
        (path, file)
    }

    fn layered_test_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vl_{}_{}", tag, uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn layered_override_layers_share_the_total_byte_budget() {
        let (zip, file) = open_probe_zip(
            &[
                ("overrides/config/base.toml", b"123456"),
                ("client-overrides/config/client.toml", b"654321"),
            ],
            "layered_total.mrpack",
        );
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let dir = layered_test_dir("layered_total");
        let budget = ExtractBudget::with_limits(10, 10);
        let err = extract_mrpack_layers(&mut archive, &dir, &budget).unwrap_err();
        assert!(
            err.to_string().contains("total limit"),
            "each layer fits its own 10-byte limit, their sum must fail: {err}"
        );
        assert_eq!(budget.total(), 6, "only the first layer was committed");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn a_layer_that_exhausts_the_budget_blocks_the_next_layer() {
        let (zip, file) = open_probe_zip(
            &[
                ("overrides/config/base.toml", b"123456789"),
                ("client-overrides/config/client.toml", b"12"),
            ],
            "layered_remaining.mrpack",
        );
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let dir = layered_test_dir("layered_remaining");
        let err = extract_mrpack_layers(&mut archive, &dir, &ExtractBudget::with_limits(10, 10))
            .unwrap_err();
        assert!(err.to_string().contains("total limit"), "got: {err}");
        assert!(!dir.join("config/client.toml").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn embedded_payload_consumes_the_same_operation_budget_as_overrides() {
        let (zip, file) = open_probe_zip(
            &[
                ("overrides/config/base.toml", b"123456"),
                ("embedded.bin", b"abcdef"),
            ],
            "layered_embedded.mrpack",
        );
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let dir = layered_test_dir("layered_embedded");
        let err = extract_mrpack_layers(&mut archive, &dir, &ExtractBudget::with_limits(10, 10))
            .unwrap_err();
        assert!(err.to_string().contains("total limit"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn legal_layer_overlap_keeps_client_priority_with_a_shared_budget() {
        let (zip, file) = open_probe_zip(
            &[
                ("overrides/config/shared.toml", b"base"),
                ("client-overrides/config/shared.toml", b"client"),
            ],
            "layered_overlap.mrpack",
        );
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let dir = layered_test_dir("layered_overlap");
        extract_mrpack_layers(&mut archive, &dir, &ExtractBudget::with_limits(100, 100)).unwrap();
        assert_eq!(
            std::fs::read(dir.join("config/shared.toml")).unwrap(),
            b"client"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn duplicate_path_inside_one_layer_is_still_rejected() {
        let (zip, file) = open_probe_zip(
            &[
                ("overrides/config/a.toml", b"one"),
                ("overrides/config/A.TOML", b"two"),
            ],
            "layered_duplicate.mrpack",
        );
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let dir = layered_test_dir("layered_duplicate");
        let err = extract_mrpack_layers(&mut archive, &dir, &ExtractBudget::with_limits(100, 100))
            .unwrap_err();
        assert!(
            err.to_string().contains("Duplicate or conflicting"),
            "got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn zip_extraction_rejects_traversal_entries_before_they_escape() {
        // Regression for the ZIP 9 migration: entry names are fallible now and
        // must still be validated before anything is written outside `dest`.
        let zip = temp_probe_zip(
            &format!("traversal_{}.zip", uuid::Uuid::new_v4().simple()),
            &[("safe.txt", b"safe"), ("../escaped.txt", b"evil")],
        );
        let dest = layered_test_dir("traversal_extract");
        let mut budget = ExtractBudget::with_limits(100, 100);
        let error = extract_zip_to_dir_budgeted(&zip, &dest, &mut budget).unwrap_err();
        assert!(
            !error.to_string().is_empty(),
            "a traversal entry must produce a clean error"
        );
        assert!(
            !dest.parent().unwrap().join("escaped.txt").exists(),
            "traversal entry must not escape the destination"
        );
        assert_eq!(std::fs::read(dest.join("safe.txt")).unwrap(), b"safe");
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn zip_extraction_rejects_duplicate_destination_paths() {
        let zip = temp_probe_zip(
            &format!("duplicate_{}.zip", uuid::Uuid::new_v4().simple()),
            &[("mods/A.jar", b"one"), ("mods/./A.jar", b"two")],
        );
        let dest = layered_test_dir("duplicate_extract");
        let mut budget = ExtractBudget::with_limits(100, 100);
        let error = extract_zip_to_dir_budgeted(&zip, &dest, &mut budget).unwrap_err();
        assert!(
            error.to_string().contains("Duplicate or conflicting"),
            "got: {error}"
        );
        assert_eq!(std::fs::read(dest.join("mods/A.jar")).unwrap(), b"one");
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn file_count_limit_stops_a_flood_of_empty_entries() {
        let (zip, file) = open_probe_zip(
            &[
                ("overrides/a", b""),
                ("overrides/b", b""),
                ("client-overrides/c", b""),
            ],
            "layered_empty_files.mrpack",
        );
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let dir = layered_test_dir("layered_empty_files");
        let budget = ExtractBudget::with_all_limits(1000, 1000, 2, 1000);
        let err = extract_mrpack_layers(&mut archive, &dir, &budget).unwrap_err();
        assert!(err.to_string().contains("file count limit"), "got: {err}");
        assert_eq!(budget.files(), 2);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn path_metadata_limit_bounds_attacker_controlled_names() {
        let mut budget = ExtractBudget::with_all_limits(1000, 1000, 1000, 8);
        let err = budget.claim_path("config/long-name.toml").unwrap_err();
        assert!(err.to_string().contains("path metadata"), "got: {err}");
    }

    #[test]
    fn mrpack_detect_fabric_loader_key() {
        let deps: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"fabric-loader":"0.15.11","minecraft":"1.20.4"}"#).unwrap();
        let (l, v) = detect_mrpack_loader(&deps);
        assert_eq!(l.as_deref(), Some("fabric-loader"));
        assert_eq!(v.as_deref(), Some("0.15.11"));
    }

    #[test]
    fn mrpack_detect_forge_and_neoforge() {
        let deps: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"forge":"47.2.0","minecraft":"1.20.1"}"#).unwrap();
        let (l, _) = detect_mrpack_loader(&deps);
        assert_eq!(l.as_deref(), Some("forge"));

        let deps: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"neoforge":"21.1.135","minecraft":"1.21.1"}"#).unwrap();
        let (l, v) = detect_mrpack_loader(&deps);
        assert_eq!(l.as_deref(), Some("neoforge"));
        assert_eq!(v.as_deref(), Some("21.1.135"));
    }

    #[test]
    fn mrpack_detect_vanilla_when_no_loader() {
        let deps: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"minecraft":"1.21.4"}"#).unwrap();
        let (l, v) = detect_mrpack_loader(&deps);
        assert_eq!(l, None);
        assert_eq!(v, None);
    }

    #[test]
    fn mrpack_loader_type_maps_supported_loaders() {
        assert_eq!(mrpack_loader_type(None).unwrap(), LoaderType::Vanilla);
        assert_eq!(
            mrpack_loader_type(Some("fabric-loader")).unwrap(),
            LoaderType::Fabric
        );
        assert_eq!(
            mrpack_loader_type(Some("fabric")).unwrap(),
            LoaderType::Fabric
        );
        assert_eq!(
            mrpack_loader_type(Some("forge")).unwrap(),
            LoaderType::Forge
        );
        assert_eq!(
            mrpack_loader_type(Some("neoforge")).unwrap(),
            LoaderType::NeoForge
        );
    }

    #[test]
    fn mrpack_loader_type_rejects_quilt() {
        let err = mrpack_loader_type(Some("quilt-loader")).unwrap_err();
        assert!(
            err.to_string().contains("Quilt"),
            "error must name Quilt: {}",
            err
        );
    }

    #[test]
    fn mrpack_dest_rejects_traversal() {
        let base = std::env::temp_dir().join(format!("vl_mrpack_dest_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        for evil in [
            "../evil.json",
            "mods/../../evil.json",
            "mods/..\\..\\evil.json",
            "C:/Windows/evil.exe",
            "C:\\Windows\\evil.exe",
            "\\evil.json",
            "/etc/passwd",
            "",
            "mods/\0evil.json",
        ] {
            assert!(
                mrpack_dest(&base, evil).is_err(),
                "path {:?} must be rejected",
                evil
            );
        }
        let ok = mrpack_dest(&base, "mods/foo.jar").unwrap();
        assert_eq!(ok, base.join("mods/foo.jar"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn mrpack_client_overrides_are_extracted_and_server_overrides_are_not() {
        assert_eq!(
            extract_mrpack_override("overrides/config/test.cfg"),
            Some("config/test.cfg")
        );
        assert_eq!(
            extract_mrpack_override("client-overrides/config/test.cfg"),
            Some("config/test.cfg")
        );
        assert_eq!(
            extract_mrpack_override("overrides-client/config/test.cfg"),
            Some("config/test.cfg")
        );
        assert_eq!(
            extract_mrpack_override("server-overrides/config/test.cfg"),
            None
        );
        assert_eq!(
            extract_mrpack_override("overrides-server/config/test.cfg"),
            None
        );
    }

    #[test]
    fn disabled_mrpack_sidecar_uses_the_shared_stem() {
        let base = std::env::temp_dir().join(format!(
            "vl_mrpack_sidecar_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dest = base.join("mods").join("foo.jar.disabled");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"jar").unwrap();
        let entry = serde_json::json!({"project_id": "abc"});
        write_mrpack_sidecar(&dest, &entry).unwrap();
        let expected = base
            .join("mods")
            .join(".index")
            .join("foo.voidlauncher.json");
        assert!(
            expected.is_file(),
            "disabled sidecar must use the content stem"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&expected).unwrap()).unwrap();
        assert_eq!(parsed["project_id"], "abc");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn curseforge_manifest_files_route_by_project_class() {
        let files = serde_json::json!([
            {"projectID": 1, "fileID": 10},
            {"projectID": 2, "fileID": 20},
            {"projectID": 3, "fileID": 30},
            {"projectID": 4, "fileID": 40},
            {"projectID": 5, "fileID": 50}
        ]);
        let projects = vec![
            crate::curseforge::CfBulkMod {
                id: 1,
                name: "Mod".into(),
                class_id: Some(6),
                latest_files: Vec::new(),
                latest_files_indexes: Vec::new(),
            },
            crate::curseforge::CfBulkMod {
                id: 2,
                name: "Pack".into(),
                class_id: Some(12),
                latest_files: Vec::new(),
                latest_files_indexes: Vec::new(),
            },
            crate::curseforge::CfBulkMod {
                id: 3,
                name: "Shader".into(),
                class_id: Some(6552),
                latest_files: Vec::new(),
                latest_files_indexes: Vec::new(),
            },
            crate::curseforge::CfBulkMod {
                id: 4,
                name: "Unknown".into(),
                class_id: Some(999),
                latest_files: Vec::new(),
                latest_files_indexes: Vec::new(),
            },
        ];
        let (targets, failed) =
            plan_curseforge_manifest_files(files.as_array().unwrap(), &projects);
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].class, crate::curseforge::CfContentClass::Mod);
        assert_eq!(
            targets[1].class,
            crate::curseforge::CfContentClass::ResourcePack
        );
        assert_eq!(
            targets[2].class,
            crate::curseforge::CfContentClass::ShaderPack
        );
        assert_eq!(failed.len(), 2, "unknown and missing classes are reported");
        assert!(failed.iter().all(|f| f.retryable));
        assert!(failed.iter().all(|f| f.reason.contains("content class")));
        // A missing class must never be turned into `mods/`.
        assert!(targets
            .iter()
            .all(|t| t.project_id != 4 && t.project_id != 5));
    }

    #[test]
    fn curseforge_custom_overrides_folder_is_supported_and_contained() {
        assert_eq!(
            curseforge_overrides_prefix(&serde_json::json!({})).unwrap(),
            "overrides/"
        );
        assert_eq!(
            curseforge_overrides_prefix(&serde_json::json!({"overrides": "custom"})).unwrap(),
            "custom/"
        );
        assert_eq!(
            curseforge_overrides_prefix(&serde_json::json!({"overrides": "pack/core/"})).unwrap(),
            "pack/core/"
        );
        for unsafe_folder in ["../outside", "/rooted", "C:/windows", "a//b", "./a"] {
            assert!(
                curseforge_overrides_prefix(&serde_json::json!({ "overrides": unsafe_folder }))
                    .is_err(),
                "unsafe overrides folder must be rejected: {unsafe_folder}"
            );
        }
    }

    #[test]
    fn mrpack_install_rejects_unsafe_path() {
        // Regression: a malicious mrpack index entry must fail before any file
        // is written or any download is attempted.
        let base = std::env::temp_dir().join(format!("vl_mrpack_inst_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let entry = serde_json::json!({"path": "../evil.json", "hashes": {}});
        let embedded = Arc::new(HashMap::new());
        let result = tauri::async_runtime::block_on(install_mrpack_file(
            "../evil.json",
            &entry,
            &base,
            &embedded,
            true,
        ));
        assert!(result.is_err());
        assert!(!base.parent().unwrap().join("evil.json").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn mrpack_download_metadata_rejects_spoofed_urls() {
        let entry = serde_json::json!({
            "downloads": [
                "https://cdn.modrinth.com:443@evil.example/payload",
                "https://cdn.modrinth.com@evil.example/payload",
                "https://cdn.modrinth.com:8443/data/file.jar",
                "https://cdn.modrinth.com/data/ok.jar"
            ]
        });
        assert_eq!(
            resolve_mrpack_urls(&entry, "mods/ok.jar").await,
            vec!["https://cdn.modrinth.com/data/ok.jar".to_string()]
        );

        let only_bad = serde_json::json!({
            "downloads": ["https://cdn.modrinth.com:443@evil.example/payload"]
        });
        assert!(
            resolve_mrpack_urls(&only_bad, "mods/bad.jar")
                .await
                .is_empty(),
            "an unsafe URL must never be handed to the downloader"
        );
    }

    #[tokio::test]
    async fn mrpack_install_rejects_a_spoofed_download_before_any_request() {
        let base = std::env::temp_dir().join(format!(
            "vl_mrpack_spoof_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&base).unwrap();
        let entry = serde_json::json!({
            "path": "mods/bad.jar",
            "hashes": {},
            "downloads": ["https://cdn.modrinth.com:443@evil.example/payload"]
        });
        let embedded = Arc::new(HashMap::new());
        let err = install_mrpack_file("mods/bad.jar", &entry, &base, &embedded, false)
            .await
            .expect_err("the spoofed URL must be rejected");
        assert!(
            err.to_string().contains("No download URL"),
            "unexpected error: {err}"
        );
        assert!(!base.join("mods/bad.jar").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn mrpack_layered_client_overrides_win_over_base_overrides() {
        let index = serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "1",
            "name": "Layered",
            "files": [],
            "dependencies": { "minecraft": "1.20.1" }
        });
        let zip_path = temp_probe_zip(
            "layered_overrides.mrpack",
            &[
                (
                    "modrinth.index.json",
                    serde_json::to_vec(&index).unwrap().as_slice(),
                ),
                ("overrides/config/shared.toml", b"base".as_slice()),
                ("overrides/config/base-only.toml", b"base".as_slice()),
                ("client-overrides/config/shared.toml", b"client".as_slice()),
                (
                    "client-overrides/config/client-only.toml",
                    b"client".as_slice(),
                ),
            ],
        );
        let instances_dir =
            std::env::temp_dir().join(format!("vl_mrpack_layered_{}", std::process::id()));
        std::fs::create_dir_all(&instances_dir).unwrap();

        let instance = import_mrpack(&instances_dir, zip_path.to_str().unwrap(), "Layered", None)
            .await
            .expect("synthetic mrpack must import");
        assert_eq!(instance.name, "Layered");

        let config = instances_dir.join("Layered/.minecraft/config");
        assert_eq!(
            std::fs::read_to_string(config.join("shared.toml")).unwrap(),
            "client",
            "client-overrides must replace the base overrides layer"
        );
        assert!(config.join("base-only.toml").is_file());
        assert!(config.join("client-only.toml").is_file());

        let _ = std::fs::remove_dir_all(&instances_dir);
        let _ = std::fs::remove_file(&zip_path);
    }

    #[tokio::test]
    async fn mrpack_index_colliding_with_an_override_is_rejected() {
        let index = serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "1",
            "name": "Collision",
            "files": [{
                "path": "config/shared.toml",
                "hashes": { "sha1": "0000000000000000000000000000000000000000" },
                "downloads": ["https://cdn.modrinth.com/data/x/versions/y/file.jar"],
                "fileSize": 1
            }],
            "dependencies": { "minecraft": "1.20.1" }
        });
        let zip_path = temp_probe_zip(
            "index_override_collision.mrpack",
            &[
                (
                    "modrinth.index.json",
                    serde_json::to_vec(&index).unwrap().as_slice(),
                ),
                ("client-overrides/config/shared.toml", b"client".as_slice()),
            ],
        );
        let instances_dir =
            std::env::temp_dir().join(format!("vl_mrpack_collision_{}", std::process::id()));
        std::fs::create_dir_all(&instances_dir).unwrap();

        let error = import_mrpack(
            &instances_dir,
            zip_path.to_str().unwrap(),
            "Collision",
            None,
        )
        .await
        .expect_err("index and override destinations must not silently overwrite");
        let message = error.to_string();
        assert!(
            message.contains("Duplicate or conflicting"),
            "got: {message}"
        );

        let _ = std::fs::remove_dir_all(&instances_dir);
        let _ = std::fs::remove_file(&zip_path);
    }

    #[test]
    fn extract_entry_writes_inside_base() {
        let base = std::env::temp_dir().join(format!("vl_zip_test_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        // Absolute entry must be rejected by starts_with guard even if the
        // component check somehow passed.
        assert!(extract_entry(&base, "C:/escape/evil.txt", b"x").is_err());
        assert!(extract_entry(&base, "sub/file.txt", b"ok").is_ok());
        assert!(base.join("sub/file.txt").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    // в”Ђв”Ђ Import format matrix: probe_modpack across pack formats в”Ђв”Ђ

    fn temp_probe_zip(name: &str, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("vl_probe_{}_{}", std::process::id(), name));
        let file = std::fs::File::create(&path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (n, data) in entries {
            zw.start_file(*n, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
        path
    }

    #[test]
    fn probe_rejects_non_zip_bytes() {
        let p = std::env::temp_dir().join(format!("vl_probe_bad_{}.zip", std::process::id()));
        std::fs::write(&p, b"this is definitely not a zip archive").unwrap();
        let err = probe_modpack(p.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("Invalid archive"), "got: {}", err);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn probe_rejects_unrecognized_archive() {
        let p = temp_probe_zip("unrecognized.zip", &[("readme.txt", b"hello".as_slice())]);
        let err = probe_modpack(p.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("Unrecognized"), "got: {}", err);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn probe_reads_minimal_mrpack() {
        let index = br#"{"name":"My Pack","summary":"test pack","dependencies":{"minecraft":"1.21.4","fabric-loader":"0.16.9"}}"#;
        let p = temp_probe_zip(
            "minimal.mrpack",
            &[("modrinth.index.json", index.as_slice())],
        );
        let meta = probe_modpack(p.to_str().unwrap()).unwrap();
        assert_eq!(meta.format, ModpackFormat::Modrinth);
        assert_eq!(meta.name, "My Pack");
        assert_eq!(meta.summary.as_deref(), Some("test pack"));
        assert_eq!(meta.mc_version.as_deref(), Some("1.21.4"));
        assert_eq!(meta.loader.as_deref(), Some("fabric-loader"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn probe_malformed_mrpack_index_falls_through_to_unrecognized() {
        // A present-but-broken manifest must not crash or mislabel the pack:
        // it should end in the same "unrecognized format" error as an empty zip.
        let p = temp_probe_zip(
            "malformed.mrpack",
            &[("modrinth.index.json", b"{ this is not json".as_slice())],
        );
        let err = probe_modpack(p.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("Unrecognized"), "got: {}", err);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn probe_reads_prism_instance_cfg() {
        let p = temp_probe_zip(
            "prism.zip",
            &[("instance.cfg", b"name=My Prism Pack\nnotes=hi".as_slice())],
        );
        let meta = probe_modpack(p.to_str().unwrap()).unwrap();
        assert_eq!(meta.format, ModpackFormat::Prism);
        assert_eq!(meta.name, "My Prism Pack");
        let _ = std::fs::remove_file(&p);
    }

    // в”Ђв”Ђ CurseForge partial-success policy в”Ђв”Ђ

    #[test]
    fn validate_import_versions_rejects_path_components() {
        assert!(validate_import_versions("1.20.1", Some("47.2.0")).is_ok());
        assert!(validate_import_versions("", None).is_ok(), "legacy Prism");
        assert!(validate_import_versions("../outside", None).is_err());
        assert!(validate_import_versions("1.20.1", Some("../../evil")).is_err());
        assert!(validate_import_versions("1.20.1", Some("47.2.0:bad")).is_err());
    }

    #[test]
    fn import_completion_keeps_partially_downloaded_pack() {
        // Some files missing is a warning, not a failed import: the instance
        // stays usable and the UI offers a retry for the missing files.
        assert_eq!(import_completion(10, 7), ImportCompletion::Partial);
        assert_eq!(import_completion(10, 9), ImportCompletion::Partial);
        assert_eq!(import_completion(10, 10), ImportCompletion::Complete);
        assert_eq!(import_completion(0, 0), ImportCompletion::Complete);
    }

    #[test]
    fn import_completion_rejects_pack_with_no_installed_files() {
        assert_eq!(import_completion(10, 0), ImportCompletion::Failed);
        assert_eq!(import_completion(1, 0), ImportCompletion::Failed);
    }

    #[tokio::test]
    async fn curseforge_import_without_any_downloadable_file_is_rejected() {
        // A manifest whose only entry has no fileID cannot install a single
        // mod. That is a critical failure: no network is needed to prove it,
        // and the partially created instance directory must be cleaned up.
        let manifest = br#"{"minecraft":{"version":"1.20.1","modLoaders":[{"id":"forge-1.20.1-47.2.0","primary":true}]},"files":[{"projectID":123}]}"#;
        let zip_path = temp_probe_zip(
            "cf_critical.zip",
            &[
                ("manifest.json", manifest.as_slice()),
                ("overrides/config/example.cfg", b"x=1".as_slice()),
            ],
        );
        let instances_dir =
            std::env::temp_dir().join(format!("vl_cf_critical_{}", std::process::id()));
        std::fs::create_dir_all(&instances_dir).unwrap();

        let launches = crate::launch_state::LaunchRegistry::new();
        let err = import_modpack(
            &instances_dir,
            zip_path.to_str().unwrap(),
            "Broken Pack",
            "",
            &instances_dir,
            None,
            &launches,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("none of the 1 pack files"),
            "unexpected error: {}",
            err
        );
        assert!(
            !instances_dir.join("Broken Pack").exists(),
            "a critically failed import must not leave a shell instance"
        );

        let _ = std::fs::remove_file(&zip_path);
        let _ = std::fs::remove_dir_all(&instances_dir);
    }

    #[test]
    fn probe_reads_curseforge_manifest_with_loader() {
        let manifest = br#"{"name":"CF Pack","minecraft":{"version":"1.20.1","modLoaders":[{"id":"fabric-0.15.11","primary":true}]}}"#;
        let p = temp_probe_zip("cf.zip", &[("manifest.json", manifest.as_slice())]);
        let meta = probe_modpack(p.to_str().unwrap()).unwrap();
        assert_eq!(meta.format, ModpackFormat::CurseForge);
        assert_eq!(meta.name, "CF Pack");
        assert_eq!(meta.mc_version.as_deref(), Some("1.20.1"));
        assert_eq!(meta.loader.as_deref(), Some("fabric"));
        assert_eq!(meta.loader_version.as_deref(), Some("0.15.11"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn probe_reads_atlauncher_instance_json() {
        let inst = br#"{"name":"AT Pack","minecraftVersion":"1.19.2"}"#;
        let p = temp_probe_zip("atl.zip", &[("instance.json", inst.as_slice())]);
        let meta = probe_modpack(p.to_str().unwrap()).unwrap();
        assert_eq!(meta.format, ModpackFormat::ATLauncher);
        assert_eq!(meta.name, "AT Pack");
        assert_eq!(meta.mc_version.as_deref(), Some("1.19.2"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn atl_loader_detection_components_and_top_level() {
        // Export v2 components array; an MC-prefixed loader version must be
        // normalised to the bare loader version the installer expects.
        let inst: serde_json::Value = serde_json::from_str(
            r#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.neoforged","version":"1.20.1-47.1.106"}]}"#,
        )
        .unwrap();
        let (l, v) = atl_loader_from_instance_json(&inst, "1.20.1");
        assert_eq!(l, LoaderType::NeoForge);
        assert_eq!(v.as_deref(), Some("47.1.106"));

        // API pack-detail shape: top-level loader object.
        let inst: serde_json::Value =
            serde_json::from_str(r#"{"loader":{"type":"forge","version":"47.2.0"}}"#).unwrap();
        let (l, v) = atl_loader_from_instance_json(&inst, "1.20.1");
        assert_eq!(l, LoaderType::Forge);
        assert_eq!(v.as_deref(), Some("47.2.0"));

        // No loader components -> Vanilla.
        let inst: serde_json::Value =
            serde_json::from_str(r#"{"components":[{"uid":"net.minecraft","version":"1.20.1"}]}"#)
                .unwrap();
        let (l, v) = atl_loader_from_instance_json(&inst, "1.20.1");
        assert_eq!(l, LoaderType::Vanilla);
        assert_eq!(v, None);
    }

    #[test]
    fn probe_reads_atlauncher_loader_from_components() {
        let inst = br#"{"name":"AT Forge Pack","minecraftVersion":"1.20.1","components":[{"uid":"net.minecraftforge","version":"47.2.0"}]}"#;
        let p = temp_probe_zip("atl_loader.zip", &[("instance.json", inst.as_slice())]);
        let meta = probe_modpack(p.to_str().unwrap()).unwrap();
        assert_eq!(meta.format, ModpackFormat::ATLauncher);
        assert_eq!(meta.loader.as_deref(), Some("forge"));
        assert_eq!(meta.loader_version.as_deref(), Some("47.2.0"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn import_atlauncher_detects_loader_and_rejects_existing_target() {
        let dir = std::env::temp_dir().join(format!("vl_atl_import_{}", std::process::id()));
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let inst = br#"{"name":"AT Pack","minecraftVersion":"1.19.2","components":[{"uid":"net.minecraftforge","version":"43.3.5"}]}"#;
        let p = temp_probe_zip(
            "atl_full.zip",
            &[
                ("instance.json", inst.as_slice()),
                ("mods/example.jar", b"jar".as_slice()),
            ],
        );

        let launches = crate::launch_state::LaunchRegistry::new();
        let mut reservation =
            InstanceReservation::reserve(&instances_dir, "AT Pack", &launches, "test").unwrap();
        let instance =
            import_atlauncher_pack(&instances_dir, p.to_str().unwrap(), "AT Pack").unwrap();
        reservation.commit().unwrap();
        drop(reservation);
        assert_eq!(instance.loader, LoaderType::Forge);
        assert_eq!(instance.loader_version.as_deref(), Some("43.3.5"));
        assert!(instances_dir
            .join("AT Pack")
            .join(".minecraft")
            .join("mods")
            .join("example.jar")
            .exists());

        // A second import into the same name must be refused, not merged.
        let err =
            InstanceReservation::reserve(&instances_dir, "AT Pack", &launches, "test").unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {}", err);

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn nested_zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::{Cursor, Write};
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut bytes));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                writer.start_file(*name, opts).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    fn atl_import_fixture(
        name: &str,
        mods_json: &str,
        nested: &[(&str, &[u8])],
        extra: &[(&str, &[u8])],
    ) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "vl_atl_fixture_{}_{}",
            name,
            uuid::Uuid::new_v4().simple()
        ));
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let instance_json =
            format!(r#"{{"name":"{name}","minecraftVersion":"1.20.1","mods":{mods_json}}}"#);
        let nested_bytes = nested_zip_bytes(nested);
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("instance.json", instance_json.as_bytes()),
            ("mods/extract.zip", nested_bytes.as_slice()),
        ];
        entries.extend(extra.iter().copied());
        let zip = temp_probe_zip(
            &format!("atl_{}_{}.zip", name, uuid::Uuid::new_v4().simple()),
            &entries,
        );
        (dir, zip)
    }

    #[test]
    fn atl_extract_to_mods_coremods_and_root_respects_extract_folder() {
        for (to, nested_name, expected) in [
            ("mods", "payload/foo.jar", "mods/foo.jar"),
            ("coremods", "payload/core.jar", "coremods/core.jar"),
            ("root", "payload/config/root.toml", "config/root.toml"),
        ] {
            let mods_json = format!(
                r#"[{{"name":"Old","file":"extract.zip","type":"extract","extractTo":"{to}","extractFolder":"payload"}}]"#
            );
            let (dir, zip) = atl_import_fixture(
                &format!("atl_{to}"),
                &mods_json,
                &[(nested_name, b"content")],
                &[],
            );
            let instances_dir = dir.join("instances");
            let instance =
                import_atlauncher_pack(&instances_dir, zip.to_str().unwrap(), &format!("ATL {to}"))
                    .unwrap();
            let mc = instances_dir.join(&instance.name).join(".minecraft");
            assert!(
                mc.join(expected).is_file(),
                "extractTo={to} must publish {expected}"
            );
            assert!(
                !mc.join("mods/extract.zip").exists(),
                "the extract source archive must be consumed"
            );
            assert!(
                !mc.join("payload").exists(),
                "extractFolder is an archive-internal path, not a destination"
            );
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_file(&zip);
        }
    }

    #[test]
    fn atl_texture_and_resource_pack_extract_use_the_upstream_extracted_folders() {
        for (kind, target) in [
            ("texturepackextract", "texturepacks/extracted"),
            ("resourcepackextract", "resourcepacks/extracted"),
        ] {
            let mods_json = format!(r#"[{{"name":"Pack","file":"extract.zip","type":"{kind}"}}]"#);
            let (dir, zip) = atl_import_fixture(
                &format!("atl_{kind}"),
                &mods_json,
                &[("pack.mcmeta", b"{}"), ("assets/icon.png", b"png")],
                &[],
            );
            let instances_dir = dir.join("instances");
            let instance =
                import_atlauncher_pack(&instances_dir, zip.to_str().unwrap(), "ATL Pack").unwrap();
            let mc = instances_dir.join(&instance.name).join(".minecraft");
            assert!(mc.join(target).join("pack.mcmeta").is_file());
            assert!(mc.join(target).join("assets/icon.png").is_file());
            assert!(!mc.join("mods/extract.zip").exists());
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_file(&zip);
        }
    }

    #[test]
    fn atl_extract_without_extract_folder_is_a_clear_failure() {
        let mods_json =
            r#"[{"name":"Old","file":"extract.zip","type":"extract","extractTo":"mods"}]"#;
        let (dir, zip) =
            atl_import_fixture("atl_missing_folder", mods_json, &[("foo.jar", b"jar")], &[]);
        let instances_dir = dir.join("instances");
        let err =
            import_atlauncher_pack(&instances_dir, zip.to_str().unwrap(), "ATL Bad").unwrap_err();
        assert!(
            err.to_string().contains("extractFolder"),
            "expected an actionable error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn atl_extract_rejects_nested_traversal_and_invalid_archives() {
        let mods_json = r#"[{"name":"Old","file":"extract.zip","type":"extract","extractTo":"mods","extractFolder":"payload"}]"#;
        let (dir, zip) = atl_import_fixture(
            "atl_traversal",
            mods_json,
            &[("payload/../evil.jar", b"evil")],
            &[],
        );
        let instances_dir = dir.join("instances");
        let err =
            import_atlauncher_pack(&instances_dir, zip.to_str().unwrap(), "ATL Evil").unwrap_err();
        assert!(err.to_string().contains("Path traversal"), "got: {err}");
        assert!(!instances_dir.join("ATL Evil/evil.jar").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);

        let dir =
            std::env::temp_dir().join(format!("vl_atl_invalid_{}", uuid::Uuid::new_v4().simple()));
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let instance_json =
            format!(r#"{{"name":"ATL Invalid","minecraftVersion":"1.20.1","mods":{mods_json}}}"#);
        let zip = temp_probe_zip(
            &format!("atl_invalid_{}.zip", uuid::Uuid::new_v4().simple()),
            &[
                ("instance.json", instance_json.as_bytes()),
                ("mods/extract.zip", b"not-a-zip"),
            ],
        );
        let err = import_atlauncher_pack(&instances_dir, zip.to_str().unwrap(), "ATL Invalid")
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("invalid zip"),
            "got: {err}"
        );
        assert!(!instances_dir.join("ATL Invalid/mods/extract.zip").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);
    }

    #[test]
    fn atl_partial_extract_failure_removes_the_instance() {
        let mods_json = r#"[{"name":"Good","file":"first.zip","type":"extract","extractTo":"mods","extractFolder":"payload"},{"name":"Bad","file":"second.zip","type":"extract","extractTo":"mods","extractFolder":"payload"}]"#;
        let good = nested_zip_bytes(&[("payload/good.jar", b"good")]);
        let dir =
            std::env::temp_dir().join(format!("vl_atl_partial_{}", uuid::Uuid::new_v4().simple()));
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let instance_json =
            format!(r#"{{"name":"ATL Partial","minecraftVersion":"1.20.1","mods":{mods_json}}}"#);
        let zip = temp_probe_zip(
            &format!("atl_partial_{}.zip", uuid::Uuid::new_v4().simple()),
            &[
                ("instance.json", instance_json.as_bytes()),
                ("mods/first.zip", good.as_slice()),
                ("mods/second.zip", b"not-a-zip"),
            ],
        );
        let launches = crate::launch_state::LaunchRegistry::new();
        let err = futures::executor::block_on(import_modpack(
            &instances_dir,
            zip.to_str().unwrap(),
            "ATL Partial",
            "",
            &dir.join("libraries"),
            None,
            &launches,
        ))
        .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("invalid zip"),
            "got: {err}"
        );
        assert!(
            !instances_dir.join("ATL Partial").exists(),
            "a failed import must not leave a partially extracted instance"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&zip);
    }

    fn prism_zip(path: &Path, cfg_name: &str, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        let cfg = format!("[General]\nname={cfg_name}\n");
        writer.start_file("instance.cfg", opts).unwrap();
        std::io::Write::write_all(&mut writer, cfg.as_bytes()).unwrap();

        let pack =
            r#"{"components":[{"uid":"net.minecraft","version":"1.20.1"}],"formatVersion":1}"#;
        writer.start_file("mmc-pack.json", opts).unwrap();
        std::io::Write::write_all(&mut writer, pack.as_bytes()).unwrap();

        for (name, bytes) in entries {
            writer.start_file(*name, opts).unwrap();
            std::io::Write::write_all(&mut writer, bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn preserving_a_failed_preparation_keeps_the_files_and_the_launch_gate() {
        let dir = std::env::temp_dir().join(format!(
            "vl_preserve_gated_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let launches = crate::launch_state::LaunchRegistry::new();
        let mut reservation =
            InstanceReservation::reserve(&instances_dir, "Gated", &launches, "import").unwrap();
        std::fs::write(instances_dir.join("Gated/instance.json"), b"{}").unwrap();
        reservation
            .set_stage(crate::instance_readiness::SetupStage::Preparing)
            .unwrap();
        reservation.preserve_for_retry();
        drop(reservation);

        assert!(
            instances_dir.join("Gated/instance.json").is_file(),
            "the imported pack must be preserved for a retry"
        );
        assert!(
            crate::instance_readiness::read_pending(&instances_dir.join("Gated")).is_some(),
            "readiness must stay pending after an incomplete preparation"
        );
        assert!(
            crate::instance_readiness::ensure_launch_ready(&instances_dir.join("Gated"), "Gated")
                .is_err(),
            "a pending instance must not be launchable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn prism_import_uses_the_chosen_name_and_cleans_failed_imports() {
        let dir = std::env::temp_dir().join(format!(
            "vl_prism_name_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();

        let good = dir.join("good.zip");
        prism_zip(
            &good,
            "Pack Name",
            &[("minecraft/mods/testmod.jar", b"PK\x03\x04jar")],
        );

        // The archive says "Pack Name", but the user typed "Chosen Name".
        let launches = crate::launch_state::LaunchRegistry::new();
        let outcome = import_modpack(
            &instances_dir,
            good.to_str().unwrap(),
            "Chosen Name",
            "",
            &instances_dir,
            None,
            &launches,
        )
        .await
        .expect("Prism import with a different user-chosen name must succeed");

        assert_eq!(outcome.instance.name, "Chosen Name");
        assert!(instances_dir
            .join("Chosen Name/.minecraft/mods/testmod.jar")
            .exists());
        assert!(
            !instances_dir.join("Pack Name").exists(),
            "the pack-internal name must not create a second directory"
        );
        let stored = instances::get_instance(&instances_dir, "Chosen Name").unwrap();
        assert_eq!(stored.name, "Chosen Name");

        // A file where the pack expects a directory makes extraction fail
        // after the reservation; only that directory may be removed.
        let broken = dir.join("broken.zip");
        prism_zip(
            &broken,
            "Pack Name",
            &[
                ("minecraft/mods", b"not a directory"),
                ("minecraft/mods/broken.jar", b"PK\x03\x04jar"),
            ],
        );
        let err = import_modpack(
            &instances_dir,
            broken.to_str().unwrap(),
            "Broken Name",
            "",
            &instances_dir,
            None,
            &launches,
        )
        .await
        .unwrap_err();
        assert!(!err.to_string().is_empty());
        assert!(
            !instances_dir.join("Broken Name").exists(),
            "a failed Prism import must not leave a blocking partial directory"
        );

        // Re-importing after the failed attempt must work with the same name.
        import_modpack(
            &instances_dir,
            good.to_str().unwrap(),
            "Broken Name",
            "",
            &instances_dir,
            None,
            &launches,
        )
        .await
        .expect("repeat import after cleanup must succeed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn concurrent_imports_of_the_same_name_have_exactly_one_winner() {
        let dir = std::env::temp_dir().join(format!(
            "vl_import_race_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let target = instances_dir.join("Same Name");

        let launches = crate::launch_state::LaunchRegistry::new();
        let (first, second) = tokio::join!(
            import_with_cleanup(&instances_dir, "Same Name", &launches, "test", || async {
                // Hold the reservation long enough for the second future to
                // attempt (and lose) the same target.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                std::fs::create_dir_all(target.join(".minecraft")).unwrap();
                std::fs::write(target.join(".minecraft/winner.txt"), b"winner").unwrap();
                Ok::<_, LauncherError>("first")
            }),
            import_with_cleanup(&instances_dir, "Same Name", &launches, "test", || async {
                Ok::<_, LauncherError>("second")
            }),
        );

        assert_eq!(first.expect("first import must win"), "first");
        let err = second.expect_err("second import must be refused");
        assert!(err.to_string().contains("already"), "got: {}", err);
        assert_eq!(
            std::fs::read_to_string(instances_dir.join("Same Name/.minecraft/winner.txt")).unwrap(),
            "winner"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn kept_import_reservation_protects_the_directory_until_commit() {
        let dir = std::env::temp_dir().join(format!(
            "vl_import_keep_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let target = instances_dir.join("Kept");

        let launches = crate::launch_state::LaunchRegistry::new();
        let ((), reservation) =
            import_with_cleanup_keep(&instances_dir, "Kept", &launches, "test", || async {
                std::fs::create_dir_all(target.join(".minecraft")).unwrap();
                std::fs::write(target.join(".minecraft/data.txt"), b"data").unwrap();
                Ok::<_, LauncherError>(())
            })
            .await
            .expect("reserve import");
        assert!(target.is_dir());

        // While the reservation is alive, a concurrent delete/rename of the
        // same name must be refused, even after the importer returned.
        let err = crate::instances::delete_instance(&instances_dir, "Kept")
            .expect_err("delete must not race post-import preparation")
            .to_string();
        assert!(err.contains("operation is already"), "got: {err}");
        let renamed = crate::instances::Instance::new(
            "Renamed",
            "1.20.1",
            crate::instances::LoaderType::Vanilla,
            None,
        );
        let err = crate::instances::save_instance(&instances_dir, &renamed, Some("Kept"))
            .expect_err("rename must not race post-import preparation")
            .to_string();
        assert!(err.contains("operation is already"), "got: {err}");
        assert_eq!(
            std::fs::read(target.join(".minecraft/data.txt")).unwrap(),
            b"data"
        );

        // Dropping without commit is a failed command: only this operation's
        // directory is removed.
        drop(reservation);
        assert!(!target.exists(), "an uncommitted import is cleaned up");

        // A committed reservation keeps the prepared instance.
        let ((), mut reservation) =
            import_with_cleanup_keep(&instances_dir, "Kept", &launches, "test", || async {
                std::fs::create_dir_all(target.join(".minecraft")).unwrap();
                Ok::<_, LauncherError>(())
            })
            .await
            .expect("reserve import again");
        reservation.commit().unwrap();
        drop(reservation);
        assert!(target.is_dir(), "a committed import must survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn failed_import_removes_only_its_own_directory() {
        let dir = std::env::temp_dir().join(format!(
            "vl_import_cleanup_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();

        // 1) Partial directory created by this operation is removed.
        let launches = crate::launch_state::LaunchRegistry::new();
        let err = import_with_cleanup(&instances_dir, "Broken", &launches, "test", || async {
            let target = instances_dir.join("Broken");
            std::fs::create_dir_all(target.join(".minecraft")).unwrap();
            std::fs::write(target.join(".minecraft/partial.jar"), b"partial").unwrap();
            Err::<(), _>(LauncherError::Instance("injected import failure".into()))
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("injected import failure"));
        assert!(
            !instances_dir.join("Broken").exists(),
            "the failed operation must remove its own directory"
        );

        // 2) An existing instance is never deleted or merged into.
        let existing = instances_dir.join("Existing");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("user-data.txt"), b"keep me").unwrap();
        let err = import_with_cleanup(&instances_dir, "Existing", &launches, "test", || async {
            Ok::<_, LauncherError>(())
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {}", err);
        assert_eq!(
            std::fs::read_to_string(existing.join("user-data.txt")).unwrap(),
            "keep me"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn readiness_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vl_readiness_import_{}_{}",
            tag,
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_pending_marker(
        instances_dir: &Path,
        name: &str,
        stage: crate::instance_readiness::SetupStage,
        token: &str,
    ) {
        let dir = instances_dir.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        crate::instance_readiness::write_pending(
            &dir,
            &crate::instance_readiness::PendingSetup {
                stage,
                operation: "import".to_string(),
                started_at: "2026-11-14T00:00:00Z".to_string(),
                token: token.to_string(),
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn launch_is_refused_while_an_import_reservation_is_held() {
        let dir = readiness_dir("held");
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let launches = crate::launch_state::LaunchRegistry::new();
        let mut reservation =
            InstanceReservation::reserve(&instances_dir, "Pack", &launches, "import").unwrap();

        // The importer has already written metadata — the exact state the
        // previous audit proved launchable.
        let instance = crate::instances::Instance::new(
            "Pack",
            "1.20.1",
            crate::instances::LoaderType::Vanilla,
            None,
        );
        crate::instances::save_instance_reserved(&instances_dir, &instance, None).unwrap();
        assert!(instances_dir.join("Pack/instance.json").is_file());

        assert_eq!(
            launches.begin_launch("Pack"),
            Err(crate::launch_state::LaunchBusy::Preparing),
            "launch must be refused while post-import preparation is running"
        );
        // Case-insensitive: the same Windows directory under another spelling.
        assert_eq!(
            launches.begin_launch("pack"),
            Err(crate::launch_state::LaunchBusy::Preparing)
        );
        // A delete/rename is refused too, so cleanup cannot run under a launch.
        assert_eq!(
            launches.begin_edit("PACK"),
            Err(crate::launch_state::LaunchBusy::Preparing)
        );

        reservation.commit().unwrap();
        drop(reservation);
        assert!(
            launches.begin_launch("Pack").is_ok(),
            "after commit the instance is launchable again"
        );
        assert!(crate::instance_readiness::read_pending(&instances_dir.join("Pack")).is_none());
        assert!(
            !instances_dir.join("Pack/.void-import-owner").exists(),
            "the ownership marker must be retired with the pending marker"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_launch_that_won_prevents_an_import_from_creating_anything() {
        let dir = readiness_dir("launch-wins");
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let launches = crate::launch_state::LaunchRegistry::new();
        let token = launches.begin_launch("Pack").expect("launch wins");

        let err = InstanceReservation::reserve(&instances_dir, "Pack", &launches, "import")
            .expect_err("an import must not start while the launch is preparing");
        assert!(err.to_string().contains("already starting"), "got: {err}");
        assert!(
            !instances_dir.join("Pack").exists(),
            "the losing import must leave no directory behind"
        );

        // The same holds for an abandoned incomplete directory: a launch that
        // owns the name must not have its directory moved aside.
        write_pending_marker(
            &instances_dir,
            "Pack",
            crate::instance_readiness::SetupStage::Importing,
            "interrupted",
        );
        let err = InstanceReservation::reserve(&instances_dir, "Pack", &launches, "import")
            .expect_err("the launch still owns the name");
        assert!(err.to_string().contains("already starting"), "got: {err}");
        assert!(instances_dir.join("Pack/.void-instance-pending").is_file());
        let parked = std::fs::read_dir(&instances_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".void-recovery-")
            })
            .count();
        assert_eq!(
            parked, 0,
            "nothing may be parked while a launch owns the name"
        );

        launches.release("Pack", token);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_crashed_import_is_not_launchable_even_with_metadata() {
        let dir = readiness_dir("crash");
        let instances_dir = dir.join("instances");
        let instance = crate::instances::Instance::new(
            "Pack",
            "1.20.1",
            crate::instances::LoaderType::Vanilla,
            None,
        );
        crate::instances::create_instance(
            &instances_dir,
            &instance,
            &crate::launch_state::LaunchRegistry::new(),
        )
        .unwrap();
        // Simulate a crash after metadata but before preparation finished.
        write_pending_marker(
            &instances_dir,
            "Pack",
            crate::instance_readiness::SetupStage::Preparing,
            "crashed-owner",
        );

        // A fresh process has no in-memory Preparing entry, so the registry
        // alone would grant the launch. The persistent marker must refuse it.
        let launches = crate::launch_state::LaunchRegistry::new();
        let token = launches.begin_launch("Pack").expect("registry grants");
        let err =
            crate::instance_readiness::ensure_launch_ready(&instances_dir.join("Pack"), "Pack")
                .expect_err("the persistent readiness gate must refuse an interrupted setup");
        assert!(err.to_string().contains("not ready"), "got: {err}");
        launches.release("Pack", token);

        // The user can finish the interrupted preparation.
        let mut reservation =
            InstanceReservation::resume(&instances_dir, "Pack", &launches, "repair").unwrap();
        assert_eq!(
            launches.begin_launch("Pack"),
            Err(crate::launch_state::LaunchBusy::Preparing)
        );
        reservation.commit().unwrap();
        drop(reservation);
        assert!(crate::instance_readiness::ensure_launch_ready(
            &instances_dir.join("Pack"),
            "Pack"
        )
        .is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_interrupted_import_cannot_be_resumed_and_a_retry_parks_it() {
        let dir = readiness_dir("park");
        let instances_dir = dir.join("instances");
        let launches = crate::launch_state::LaunchRegistry::new();
        write_pending_marker(
            &instances_dir,
            "Pack",
            crate::instance_readiness::SetupStage::Importing,
            "interrupted",
        );
        std::fs::write(instances_dir.join("Pack/partial.jar"), b"partial").unwrap();

        let err = InstanceReservation::resume(&instances_dir, "Pack", &launches, "repair")
            .expect_err("an interrupted import must not be resumed");
        assert!(err.to_string().contains("did not finish"), "got: {err}");

        // Re-importing the same name parks the incomplete directory instead of
        // failing or deleting it, then creates a fresh one.
        let mut reservation =
            InstanceReservation::reserve(&instances_dir, "Pack", &launches, "import").unwrap();
        assert!(instances_dir.join("Pack").is_dir());
        assert!(!instances_dir.join("Pack/partial.jar").exists());
        let parked: Vec<_> = std::fs::read_dir(&instances_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".void-recovery-")
            })
            .collect();
        assert_eq!(
            parked.len(),
            1,
            "the old files must be preserved, not deleted"
        );
        assert!(parked[0].path().join("partial.jar").is_file());

        reservation.commit().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn two_independent_instances_can_be_prepared_at_the_same_time() {
        let dir = readiness_dir("independent");
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let launches = crate::launch_state::LaunchRegistry::new();
        let mut a =
            InstanceReservation::reserve(&instances_dir, "Alpha", &launches, "import").unwrap();
        let mut b =
            InstanceReservation::reserve(&instances_dir, "Beta", &launches, "import").unwrap();
        assert_eq!(
            launches.begin_launch("Alpha"),
            Err(crate::launch_state::LaunchBusy::Preparing)
        );
        assert_eq!(
            launches.begin_launch("Beta"),
            Err(crate::launch_state::LaunchBusy::Preparing)
        );
        a.commit().unwrap();
        assert!(launches.begin_launch("Alpha").is_ok());
        assert_eq!(
            launches.begin_launch("Beta"),
            Err(crate::launch_state::LaunchBusy::Preparing),
            "committing one import must not release the other"
        );
        b.commit().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn failed_resume_never_deletes_the_imported_instance() {
        let dir = readiness_dir("resume-fail");
        let instances_dir = dir.join("instances");
        let launches = crate::launch_state::LaunchRegistry::new();
        write_pending_marker(
            &instances_dir,
            "Pack",
            crate::instance_readiness::SetupStage::Preparing,
            "resume-owner",
        );
        let instance = crate::instances::Instance::new(
            "Pack",
            "1.20.1",
            crate::instances::LoaderType::Vanilla,
            None,
        );
        crate::instances::save_instance_reserved(&instances_dir, &instance, None).unwrap();
        std::fs::write(instances_dir.join("Pack/mods.jar"), b"user data").unwrap();

        {
            let _reservation =
                InstanceReservation::resume(&instances_dir, "Pack", &launches, "repair").unwrap();
            // Dropped without commit == the repair failed.
        }
        assert!(instances_dir.join("Pack/mods.jar").is_file());
        assert!(
            crate::instance_readiness::read_pending(&instances_dir.join("Pack")).is_some(),
            "a failed repair must leave the instance blocked, not publish it"
        );
        assert!(
            launches.begin_launch("Pack").is_ok(),
            "the slot is released"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

use crate::config::AppConfig;
use crate::content;
use crate::download;
use crate::error::{LauncherError, Result};
use crate::instances::{self, Instance, LoaderType};
use crate::modloaders;
use crate::modrinth;
use crate::modrinth::ModrinthFile;
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
    /// False for manifest entries that carry no ids — nothing to retry.
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
        if component == ".." {
            return Err(LauncherError::Instance(format!(
                "Path traversal detected in zip entry: {}",
                relative
            )));
        }
    }
    Ok(())
}

/// Reject a remote-supplied file name that could escape the directory it is
/// joined into: must be a bare file name — no separators, drive prefixes,
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
/// libraries, NeoForge data map). Same rules as zip entries — no "..",
/// absolute/rooted paths, drive prefixes or null bytes — but with a message
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

static EXTRACT_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Canonical collision key for a destination path. Removes `.`/empty
/// components, folds case, and strips trailing dots/spaces that Windows
/// ignores, so two names that resolve to the same file share one key.
pub(crate) fn destination_key(relative: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in relative.replace('\\', "/").split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        let part = part.trim_end_matches(['.', ' ']).to_lowercase();
        if part.is_empty() {
            return None;
        }
        parts.push(part);
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Shared extraction budget for one logical import/update operation.
#[derive(Debug)]
pub(crate) struct ExtractBudget {
    total: u64,
    total_limit: u64,
    entry_limit: u64,
    paths: HashSet<String>,
}

impl ExtractBudget {
    pub(crate) fn new() -> Self {
        Self::with_limits(MAX_EXTRACT_TOTAL_BYTES, MAX_EXTRACT_ENTRY_BYTES)
    }

    pub(crate) fn with_limits(total_limit: u64, entry_limit: u64) -> Self {
        Self {
            total: 0,
            total_limit,
            entry_limit,
            paths: HashSet::new(),
        }
    }
    pub(crate) fn claim_path(&mut self, relative: &str) -> Result<()> {
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
        self.paths.insert(key);
        Ok(())
    }

    fn reserve_bytes(&self, entry_bytes: u64) -> Result<()> {
        if entry_bytes > self.entry_limit {
            return Err(LauncherError::Instance(format!(
                "Archive entry exceeds the safe extraction limit ({} bytes)",
                self.entry_limit
            )));
        }
        if self.total.saturating_add(entry_bytes) > self.total_limit {
            return Err(LauncherError::Instance(
                "Archive extraction exceeds the safe total limit".to_string(),
            ));
        }
        Ok(())
    }

    fn commit(&mut self, bytes: u64) {
        self.total = self.total.saturating_add(bytes);
    }

    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.total
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
fn is_unsafe_reparse_point(meta: &std::fs::Metadata) -> bool {
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
fn is_unsafe_reparse_point(meta: &std::fs::Metadata) -> bool {
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
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut budget = ExtractBudget::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        let entry_name = entry.name().to_string();
        if entry.is_dir() {
            continue;
        }
        write_extracted_budgeted(dest, &entry_name, &mut entry, &mut budget)?;
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
/// rejecting path traversal, absolute/rooted paths and drive prefixes — the
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

fn read_zip_entry_text(entry: &mut zip::read::ZipFile<'_>, limit: u64) -> Option<String> {
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

    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .collect();

    // Check Prism/MultiMC
    if names.iter().any(|n| n == "instance.cfg") {
        let mut cfg = String::new();
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                if entry.name() == "instance.cfg" {
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
                if entry.name() == "modrinth.index.json" {
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
                if entry.name() == "manifest.json" {
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
                if entry.name() == "instance.json" {
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
    // game itself — the keyword match naturally skips those.
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
pub async fn import_modpack(
    instances_dir: &Path,
    path: &str,
    instance_name: &str,
    curseforge_api_key: &str,
    libraries_dir: &Path,
    app: Option<&AppHandle>,
) -> Result<ImportOutcome> {
    emit_progress(app, "reading", 0, 1, "Reading archive...");

    // Never merge into an existing instance — a same-named import would
    // silently overwrite its mods/configs.
    ensure_instance_target_free(instances_dir, instance_name)?;

    // Sniff the archive format cheaply (one pass over the entry names). The
    // archive is file-backed, so no full copy of the pack is held in memory;
    // the format-specific importer below re-opens the file from disk itself.
    let format = {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let names: Vec<String> = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
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

    let outcome = match format {
        ModpackFormat::Prism => {
            let instance = import_with_cleanup(instances_dir, instance_name, || async {
                instances::import_prism_pack_reserved(instances_dir, path, instance_name)
            })
            .await?;
            ImportOutcome {
                report: ImportReport::complete(0, 0),
                instance,
            }
        }
        ModpackFormat::Modrinth => {
            let instance = import_with_cleanup(instances_dir, instance_name, || async {
                import_mrpack(instances_dir, path, instance_name, app).await
            })
            .await?;
            ImportOutcome {
                report: ImportReport::complete(0, 0),
                instance,
            }
        }
        ModpackFormat::CurseForge => {
            let (instance, report) = import_with_cleanup(instances_dir, instance_name, || async {
                import_curseforge_pack(
                    instances_dir,
                    path,
                    instance_name,
                    curseforge_api_key,
                    libraries_dir,
                    app,
                )
                .await
            })
            .await?;
            ImportOutcome { instance, report }
        }
        ModpackFormat::ATLauncher => {
            let instance = import_with_cleanup(instances_dir, instance_name, || async {
                import_atlauncher_pack(instances_dir, path, instance_name)
            })
            .await?;
            ImportOutcome {
                report: ImportReport::complete(0, 0),
                instance,
            }
        }
    };

    // No terminal "done" event here: the command layer runs
    // `prepare_instance_files` (loader profile + game files + assets) first
    // and only then emits "done", so the progress UI stays live until the
    // instance is actually ready for its first launch.
    Ok(outcome)
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

/// Install the mod loader when the instance declares one but has no profile
/// saved yet (every importer stores `loader_profile: None` — the profile is
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
            if let Err(e) = instances::save_instance(&config.instances_dir(), &updated, None) {
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

    // Version files: client JAR, libraries, natives — only the missing ones.
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

    // Version JSON — the launch path reads this file to build the command.
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

    // Asset index + assets (skipped wholesale when the index is on disk —
    // same coarse check the launch self-heal uses).
    let index_path = config
        .assets_dir()
        .join("indexes")
        .join(format!("{}.json", version_info.assets));
    if index_path.exists() {
        return;
    }
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
    let asset_index = match versions::fetch_asset_index(&version_info.asset_index.url).await {
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
    match serde_json::to_string_pretty(&asset_index) {
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
    if let Err(e) = download::download_assets(
        &asset_index,
        &config.assets_dir(),
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
    // Virtual/legacy mirror for old versions (1.8 and older) — same step the
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
pub(crate) fn ensure_instance_target_free(instances_dir: &Path, instance_name: &str) -> Result<()> {
    let target = instances_dir.join(instance_name);
    if target.exists() {
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
const IMPORT_OWNER_MARKER: &str = ".void-import-owner";

/// Ownership handle for one instance directory created by an import/install.
///
/// `std::fs::create_dir` is the atomic claim: two concurrent operations can
/// never both succeed for the same target. The old "check `exists()` before
/// the import, delete on failure" sequence let a losing operation delete the
/// winner's files (TOCTOU). Here the loser fails before touching anything,
/// and `Drop` removes the directory only while the owner marker proves it is
/// still the directory this operation created.
#[derive(Debug)]
pub(crate) struct InstanceReservation {
    target: PathBuf,
    marker: PathBuf,
    token: String,
    committed: bool,
}

impl InstanceReservation {
    pub(crate) fn reserve(instances_dir: &Path, instance_name: &str) -> Result<Self> {
        let name = instance_name.trim();
        if name.is_empty() || name.starts_with('.') {
            return Err(LauncherError::Instance(format!(
                "Invalid instance name: '{}'",
                instance_name
            )));
        }
        check_safe_file_name(name)?;
        std::fs::create_dir_all(instances_dir)?;

        let target = instances_dir.join(name);
        match std::fs::create_dir(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(LauncherError::Instance(format!(
                    "An instance named '{}' already exists. Choose a different name for the imported pack.",
                    name
                )))
            }
            Err(e) => return Err(e.into()),
        }

        let token = uuid::Uuid::new_v4().simple().to_string();
        let marker = target.join(IMPORT_OWNER_MARKER);
        if let Err(e) = std::fs::write(&marker, &token) {
            let _ = std::fs::remove_dir_all(&target);
            return Err(e.into());
        }
        Ok(Self {
            target,
            marker,
            token,
            committed: false,
        })
    }

    /// Keep the directory: the import completed and the marker is retired.
    pub(crate) fn commit(&mut self) {
        let _ = std::fs::remove_file(&self.marker);
        self.committed = true;
    }
}

impl Drop for InstanceReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        match std::fs::read_to_string(&self.marker) {
            Ok(token) if token.trim() == self.token => {
                tracing::warn!(
                    target: "launcher",
                    "Import failed, removing partially-created instance {}",
                    self.target.display()
                );
                if let Err(e) = std::fs::remove_dir_all(&self.target) {
                    tracing::warn!(
                        target: "launcher",
                        "Cleanup of {} failed: {}",
                        self.target.display(), e
                    );
                }
            }
            Ok(_) => tracing::warn!(
                target: "launcher",
                "Not removing {}: the import owner marker changed",
                self.target.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // The import itself (or the user) removed/moved the directory;
                // there is nothing left this operation owns.
            }
            Err(e) => tracing::warn!(
                target: "launcher",
                "Not removing {}: owner marker is unreadable ({})",
                self.target.display(), e
            ),
        }
    }
}

/// Run a format-specific importer against a freshly reserved
/// `instances_dir/<instance_name>` and, if it fails, remove that directory so
/// a failed import never leaves broken/missing files behind.
///
/// The reservation is atomic: an already existing directory (including one
/// created by a concurrent import) is never touched by this operation.
pub(crate) async fn import_with_cleanup<T, F, Fut>(
    instances_dir: &Path,
    instance_name: &str,
    importer: F,
) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut reservation = InstanceReservation::reserve(instances_dir, instance_name)?;
    let result = importer().await;
    if result.is_ok() {
        reservation.commit();
    }
    result
}

/// Import a Modrinth .mrpack
pub(crate) async fn import_mrpack(
    instances_dir: &Path,
    path: &str,
    instance_name: &str,
    app: Option<&AppHandle>,
) -> Result<Instance> {
    emit_progress(app, "extracting", 0, 1, "Extracting overrides...");

    // File-backed archive: the pack is not buffered in memory.
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;

    // Read index
    let mut index_str = String::new();
    let mut found = false;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.name() == "modrinth.index.json" {
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
    // a file present in both layers is replaced by the client variant. Each
    // layer has its own collision budget, while `override_keys` keeps the
    // index-vs-override collision check below intact.
    let mut extracted_any = false;
    let mut embedded: HashMap<String, Vec<u8>> = HashMap::new();
    let mut embedded_budget = ExtractBudget::with_limits(MAX_EMBEDDED_BYTES, MAX_EMBEDDED_BYTES);
    let mut server_override_budget = ExtractBudget::new();
    let mut client_override_budget = ExtractBudget::new();
    let mut override_keys: HashSet<String> = HashSet::new();
    const MAX_EMBEDDED_BYTES: u64 = 1024 * 1024 * 1024;

    for client_layer in [false, true] {
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            let entry_name = entry.name().to_string();

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
                    write_extracted_budgeted(&mc_dir, relative, &mut entry, budget)?;
                    write_override_sidecar(&mc_dir, relative, "modrinth");
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
            // Embedded files stay in memory: the per-file install tasks below run
            // concurrently against a shared map, and file-backed lookup would
            // require re-architecting the concurrency model. Bound both a single
            // entry and the total so a malicious pack cannot exhaust RAM.
            let size = entry.size();
            if size > MAX_EMBEDDED_BYTES {
                return Err(LauncherError::Instance(
                    "The modpack embeds too much data to install safely".to_string(),
                ));
            }
            let mut buf = Vec::new();
            (&mut entry)
                .take(MAX_EMBEDDED_BYTES + 1)
                .read_to_end(&mut buf)?;
            embedded_budget.reserve_bytes(buf.len() as u64)?;
            embedded_budget.commit(buf.len() as u64);
            embedded.insert(normalize_zip_path(&entry_name), buf);
        }
    }

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

    instances::save_instance(instances_dir, &instance, None)?;
    tracing::info!(target: "launcher", "Imported Modrinth pack as '{}' ({} client files, {} mods)", instance_name, total, mod_count);
    Ok(instance)
}

/// Download one CurseForge mod file into an instance's `mods` directory and
/// write its sidecar. The error carries the file name and a human-readable
/// reason so the import report (and the retry UI) can name the failed file.
pub(crate) async fn install_curseforge_file(
    mods_dir: &Path,
    pid: u64,
    fid: u64,
    api_key: &str,
    claims: Option<&Mutex<ExtractBudget>>,
) -> std::result::Result<String, (String, String)> {
    let fallback = format!("{}:{}", pid, fid);
    let cf_file = crate::curseforge::get_mod_file(pid, fid, api_key)
        .await
        .map_err(|e| (fallback.clone(), format!("CurseForge API error: {}", e)))?;
    let display = if cf_file.display_name.trim().is_empty() {
        fallback.clone()
    } else {
        cf_file.display_name.clone()
    };

    // The file name comes from remote metadata and is joined into mods/ —
    // reject any path component.
    if let Err(e) = check_safe_file_name(&cf_file.file_name) {
        return Err((display, format!("Unsafe file name: {}", e)));
    }
    // The manifest filename is only known from remote metadata, so claim it
    // here: a mod that lands on the same destination as an extracted override
    // (or another manifest entry) must be reported, not silently overwrite it.
    if let Some(claims) = claims {
        let rel = format!("mods/{}", cf_file.file_name);
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
    let dest_path = mods_dir.join(&cf_file.file_name);
    let base = mods_dir.parent().unwrap_or(mods_dir);
    ensure_no_reparse_points(base, &dest_path)
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
        mods_dir,
        &cf_file.file_name,
        &content::SidecarWrite {
            provider: "curseforge",
            content_type: "mod",
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
    let mods_dir = instances_dir
        .join(instance_name)
        .join(".minecraft")
        .join("mods");
    if !instances_dir.join(instance_name).exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' does not exist",
            instance_name
        )));
    }
    std::fs::create_dir_all(&mods_dir)?;

    let total = files.len();
    let mut report = ImportReport::complete(total, 0);
    for (pid, fid) in files {
        match install_curseforge_file(&mods_dir, *pid, *fid, api_key, None).await {
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

    // Read manifest
    let mut manifest_str = String::new();
    let mut found = false;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.name() == "manifest.json" {
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
        let entry_name = entry.name().to_string();

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
        let semaphore = Arc::new(Semaphore::new(4));
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let in_progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let succeeded = Arc::new(AtomicUsize::new(0));
        let failed_ids = Arc::new(Mutex::new(Vec::<FailedImportFile>::new()));
        let mut tasks = Vec::with_capacity(total);
        let mut skipped = 0usize;

        // Initial progress
        emit_progress(
            app,
            "downloading-mods",
            0,
            total,
            &format!("Preparing {} mods...", total),
        );

        for (entry_idx, file_entry) in files.into_iter().enumerate() {
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
                let claims = claims.clone();

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
                            bytes_done: 0,
                            bytes_total: 0,
                        });
                    }

                    let result =
                        install_curseforge_file(&mods_dir, pid, fid, &api_key, Some(claims.as_ref()))
                            .await;

                    in_progress.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

                    let (last_name, file_ok) = match &result {
                        Ok(name) => {
                            succeeded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            (name.clone(), true)
                        }
                        Err((name, reason)) => {
                            tracing::warn!(target: "launcher", "CurseForge mod download failed ({}:{}): {}", pid, fid, reason);
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
            } else {
                skipped += 1;
                tracing::warn!(target: "launcher", "Skipping CurseForge file entry #{}: missing projectID or fileID", entry_idx + 1);
            }
        }

        // Wait for all downloads to complete
        join_all(tasks).await;
        let failed_list = failed_ids.lock().map(|f| f.clone()).unwrap_or_default();
        let ok = succeeded.load(std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            target: "launcher",
            "CurseForge mod download: {} succeeded, {} failed, {} skipped (no IDs)",
            ok,
            failed_list.len(),
            skipped
        );

        // A pack with at least one installed file is usable: keep it and let
        // the caller show the failed list with a retry. Only a pack where
        // nothing at all landed is unrecoverable — return an error so the
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
                "CurseForge import failed: none of the {} mod files could be installed.{}{}",
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

    instances::save_instance(instances_dir, &instance, None)?;

    tracing::info!(target: "launcher", "Imported CurseForge pack as '{}' ({} of {} mods) - loader install pending", instance_name, report.installed, total);
    Ok((instance, report))
}

/// Import an ATLauncher instance
fn import_atlauncher_pack(
    instances_dir: &Path,
    path: &str,
    instance_name: &str,
) -> Result<Instance> {
    // File-backed archive: the pack is not buffered in memory.
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;

    // Read instance.json for metadata
    let mut inst_str = String::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.name() == "instance.json" {
            inst_str = read_zip_entry_text(&mut entry, MAX_MANIFEST_BYTES).unwrap_or_default();
            break;
        }
    }

    let (mc_version, loader, loader_version);
    if let Ok(inst) = serde_json::from_str::<serde_json::Value>(&inst_str) {
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

    // Extract all files with path traversal protection
    let mut extract_budget = ExtractBudget::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        let entry_name = entry.name().to_string();
        if entry_name == "instance.json" || entry.is_dir() {
            continue;
        }
        check_safe_relative(&entry_name)?;

        write_extracted_budgeted(&mc_dir, &entry_name, &mut entry, &mut extract_budget)?;
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

    instances::save_instance(instances_dir, &instance, None)?;
    tracing::info!(target: "launcher", "Imported ATLauncher pack as '{}'", instance_name);
    Ok(instance)
}

/// Map a detected mrpack loader key to our LoaderType. Quilt is rejected
/// with a clear error — Quilt mods are not always Fabric-compatible, so a
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
/// — and JSON key order is not guaranteed, so we probe known keys in
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

    // ── Import format matrix: probe_modpack across pack formats ──

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

    // ── CurseForge partial-success policy ──

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

        let err = import_modpack(
            &instances_dir,
            zip_path.to_str().unwrap(),
            "Broken Pack",
            "",
            &instances_dir,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("none of the 1 mod files"),
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

        let mut reservation = InstanceReservation::reserve(&instances_dir, "AT Pack").unwrap();
        let instance =
            import_atlauncher_pack(&instances_dir, p.to_str().unwrap(), "AT Pack").unwrap();
        reservation.commit();
        assert_eq!(instance.loader, LoaderType::Forge);
        assert_eq!(instance.loader_version.as_deref(), Some("43.3.5"));
        assert!(instances_dir
            .join("AT Pack")
            .join(".minecraft")
            .join("mods")
            .join("example.jar")
            .exists());

        // A second import into the same name must be refused, not merged.
        let err = InstanceReservation::reserve(&instances_dir, "AT Pack").unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {}", err);

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir_all(&dir);
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
        let outcome = import_modpack(
            &instances_dir,
            good.to_str().unwrap(),
            "Chosen Name",
            "",
            &instances_dir,
            None,
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

        let (first, second) = tokio::join!(
            import_with_cleanup(&instances_dir, "Same Name", || async {
                // Hold the reservation long enough for the second future to
                // attempt (and lose) the same target.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                std::fs::create_dir_all(target.join(".minecraft")).unwrap();
                std::fs::write(target.join(".minecraft/winner.txt"), b"winner").unwrap();
                Ok::<_, LauncherError>("first")
            }),
            import_with_cleanup(&instances_dir, "Same Name", || async {
                Ok::<_, LauncherError>("second")
            }),
        );

        assert_eq!(first.expect("first import must win"), "first");
        let err = second.expect_err("second import must be refused");
        assert!(err.to_string().contains("already exists"), "got: {}", err);
        assert_eq!(
            std::fs::read_to_string(instances_dir.join("Same Name/.minecraft/winner.txt")).unwrap(),
            "winner"
        );

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
        let err = import_with_cleanup(&instances_dir, "Broken", || async {
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
        let err = import_with_cleanup(&instances_dir, "Existing", || async {
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
}

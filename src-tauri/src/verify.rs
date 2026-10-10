//! Instance file verification and repair.
//!
//! `verify_instance` is a fully offline integrity scan: it checks
//! launcher-managed files (version metadata, client JAR, libraries,
//! assets, loader profile, Java runtime) against the locally stored
//! version JSON, and reports user-managed content (mods, resource packs,
//! shader packs) as read-only issues.
//!
//! `repair_instance` re-downloads only the launcher-managed files that
//! failed verification, restoring metadata, client JAR, libraries, assets,
//! the loader profile, and a missing Java runtime. User content is never
//! deleted or replaced.

use crate::config::AppConfig;
use crate::download;
use crate::error::{LauncherError, Result};
use crate::instances::{self, Instance, LoaderType};
use crate::modloaders;
use crate::versions::{self, VersionInfo};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

/// Maximum number of individual issues listed per category in the report
/// (the full count is still tracked in `issue_count`).
const MAX_ISSUES_LISTED: usize = 100;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyIssue {
    /// Path relative to the launcher data or instance directory.
    pub path: String,
    /// "missing" | "corrupt" | "invalid"
    pub kind: String,
    /// True when repair can fix this issue by re-downloading.
    pub repairable: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyCategory {
    /// "metadata" | "minecraft" | "libraries" | "assets" | "loader" | "java" | "content"
    pub id: String,
    pub checked: u32,
    /// Total number of issues (may exceed `issues.len()` when capped).
    pub issue_count: u32,
    pub issues: Vec<VerifyIssue>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyReport {
    pub categories: Vec<VerifyCategory>,
    /// Issues that repair can fix (launcher-managed files only).
    pub repairable_count: u32,
    /// User-content issues (reported but never auto-repaired).
    pub content_issues: u32,
    pub healthy: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct RepairSummary {
    pub repaired: u32,
    pub failed: u32,
    pub content_skipped: u32,
    pub errors: Vec<String>,
}

#[derive(Clone, Serialize)]
struct RepairProgress {
    stage: String,
    percent: f64,
    message: String,
}

struct CategoryBuilder {
    id: &'static str,
    checked: u32,
    issue_count: u32,
    issues: Vec<VerifyIssue>,
}

impl CategoryBuilder {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            checked: 0,
            issue_count: 0,
            issues: Vec::new(),
        }
    }

    fn add_issue(&mut self, path: impl Into<String>, kind: &str, repairable: bool) {
        self.issue_count += 1;
        if self.issues.len() < MAX_ISSUES_LISTED {
            self.issues.push(VerifyIssue {
                path: path.into(),
                kind: kind.to_string(),
                repairable,
            });
        }
    }

    fn build(self) -> VerifyCategory {
        VerifyCategory {
            id: self.id.to_string(),
            checked: self.checked,
            issue_count: self.issue_count,
            issues: self.issues,
        }
    }
}

fn loader_type_str(loader: &LoaderType) -> &'static str {
    match loader {
        LoaderType::Vanilla => "Vanilla",
        LoaderType::Fabric => "Fabric",
        LoaderType::Forge => "Forge",
        LoaderType::NeoForge => "NeoForge",
    }
}

pub(crate) fn load_local_version_info(versions_dir: &PathBuf, mc_version: &str) -> Option<VersionInfo> {
    let path = versions_dir
        .join(mc_version)
        .join(format!("{}.json", mc_version));
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Scan user-managed content directories for files that are not valid
/// ZIP/JAR archives. Read-only: never modifies. `dirs` selects which of
/// `mods` / `resourcepacks` / `shaderpacks` to inspect.
fn scan_content(instances_dir: &PathBuf, instance: &Instance, dirs: &[&str]) -> Vec<VerifyIssue> {
    let mut issues = Vec::new();
    let mc_dir = instance.minecraft_dir(instances_dir);
    for dir_name in dirs {
        let dir = mc_dir.join(dir_name);
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.ends_with(".disabled") {
                continue;
            }
            let lower = name.to_lowercase();
            if !(lower.ends_with(".jar") || lower.ends_with(".zip")) {
                continue;
            }
            if download::verify_zip_magic(&path).is_err() {
                issues.push(VerifyIssue {
                    path: format!("{}/{}", dir_name, name),
                    kind: "corrupt".to_string(),
                    repairable: false,
                });
            }
        }
    }
    issues
}

fn check_library_file(
    cat: &mut CategoryBuilder,
    path: &PathBuf,
    sha1: &str,
    display: &str,
) {
    cat.checked += 1;
    if !path.exists() {
        cat.add_issue(display, "missing", true);
    } else if !sha1.is_empty() && download::verify_sha1(path, sha1).ok() == Some(false) {
        cat.add_issue(display, "corrupt", true);
    }
}

/// Which parts of an instance a verification run inspects: everything,
/// launcher-managed files only, or one user-content folder. Repair is always
/// launcher-only regardless of scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyScope {
    All,
    Launcher,
    Mods,
    Resourcepacks,
    Shaderpacks,
}

impl VerifyScope {
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "all" => Self::All,
            "launcher" => Self::Launcher,
            "mods" => Self::Mods,
            "resourcepacks" => Self::Resourcepacks,
            "shaderpacks" => Self::Shaderpacks,
            other => return Err(format!("Unknown verify scope: {}", other)),
        })
    }

    /// Content-only scopes: fast path that skips every launcher check.
    fn content_only_dirs(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Mods => Some(&["mods"]),
            Self::Resourcepacks => Some(&["resourcepacks"]),
            Self::Shaderpacks => Some(&["shaderpacks"]),
            Self::All | Self::Launcher => None,
        }
    }
}

/// Report for a content-only scope: no launcher categories, nothing repairable.
fn content_only_report(config: &AppConfig, instance: &Instance, dirs: &[&str]) -> VerifyReport {
    let content_issues = scan_content(&config.instances_dir(), instance, dirs);
    let content_issues_count = content_issues.len() as u32;
    let categories = vec![VerifyCategory {
        id: "content".to_string(),
        checked: 0,
        issue_count: content_issues_count,
        issues: content_issues,
    }];
    VerifyReport {
        categories,
        repairable_count: 0,
        content_issues: content_issues_count,
        healthy: content_issues_count == 0,
    }
}

/// Offline verification of a single instance against its locally stored
/// version metadata. Never touches the network and never modifies files.
pub fn verify_instance(config: &AppConfig, instance: &Instance) -> VerifyReport {
    verify_instance_scoped(config, instance, VerifyScope::All)
}

/// Scoped variant of [`verify_instance`]: `Launcher` skips user content,
/// content-only scopes (mods / resource packs / shader packs) skip every
/// launcher check and cost only a directory scan.
pub fn verify_instance_scoped(
    config: &AppConfig,
    instance: &Instance,
    scope: VerifyScope,
) -> VerifyReport {
    if let Some(dirs) = scope.content_only_dirs() {
        return content_only_report(config, instance, dirs);
    }
    let versions_dir = config.versions_dir();
    let libraries_dir = config.libraries_dir();
    let instances_dir = config.instances_dir();
    let assets_dir = config.assets_dir();

    let version_info = load_local_version_info(&versions_dir, &instance.mc_version);

    // ── metadata ────────────────────────────────────────────────
    let mut metadata = CategoryBuilder::new("metadata");
    metadata.checked = 1;
    let version_json = versions_dir
        .join(&instance.mc_version)
        .join(format!("{}.json", instance.mc_version));
    if !version_json.exists() {
        metadata.add_issue(
            format!("versions/{}/{}.json", instance.mc_version, instance.mc_version),
            "missing",
            true,
        );
    } else if version_info.is_none() {
        metadata.add_issue(
            format!("versions/{}/{}.json", instance.mc_version, instance.mc_version),
            "corrupt",
            true,
        );
    }
    let metadata = metadata.build();

    // ── minecraft (client JAR) ──────────────────────────────────
    let mut minecraft = CategoryBuilder::new("minecraft");
    minecraft.checked = 1;
    let client_path = versions_dir
        .join(&instance.mc_version)
        .join("client.jar");
    let legacy_path = versions_dir
        .join(&instance.mc_version)
        .join(format!("{}.jar", instance.mc_version));
    let client_display = format!("versions/{}/client.jar", instance.mc_version);
    if client_path.exists() {
        if let Some(vi) = &version_info {
            let expected = vi.downloads.client.sha1.as_str();
            if !expected.is_empty() && download::verify_sha1(&client_path, expected).ok() == Some(false)
            {
                minecraft.add_issue(&client_display, "corrupt", true);
            }
        }
    } else if legacy_path.exists() {
        // Legacy `{mc}.jar` is migrated to `client.jar` at launch/repair.
    } else {
        minecraft.add_issue(&client_display, "missing", true);
    }
    let minecraft = minecraft.build();

    // ── libraries (vanilla + loader profile) ────────────────────
    let mut libraries = CategoryBuilder::new("libraries");
    if let Some(vi) = &version_info {
        for lib in &vi.libraries {
            if !versions::should_include_library(lib) {
                continue;
            }
            if let Some(downloads) = &lib.downloads {
                if let Some(artifact) = &downloads.artifact {
                    let p = libraries_dir.join(&artifact.path);
                    check_library_file(
                        &mut libraries,
                        &p,
                        &artifact.sha1,
                        &format!("libraries/{}", artifact.path),
                    );
                }
                if let Some(classifier) = versions::native_classifier(lib) {
                    if let Some(classifiers) = &downloads.classifiers {
                        if let Some(native) = classifiers.get(classifier.as_str()) {
                            if let Some(path) = native.get("path").and_then(|v| v.as_str()) {
                                let sha1 =
                                    native.get("sha1").and_then(|v| v.as_str()).unwrap_or("");
                                let p = libraries_dir.join(path);
                                check_library_file(
                                    &mut libraries,
                                    &p,
                                    sha1,
                                    &format!("libraries/{}", path),
                                );
                            }
                        }
                    }
                }
            } else if lib.url.is_some() {
                let rel = versions::maven_to_path(&lib.name);
                let p = libraries_dir.join(&rel);
                libraries.checked += 1;
                if !p.exists() {
                    libraries.add_issue(format!("libraries/{}", rel), "missing", true);
                }
            }
        }
    }
    if let Some(profile) = &instance.loader_profile {
        for llib in &profile.libraries {
            let p = libraries_dir.join(&llib.path);
            let sha1 = llib.sha1.as_deref().unwrap_or("");
            check_library_file(
                &mut libraries,
                &p,
                sha1,
                &format!("libraries/{}", llib.path),
            );
        }
    }
    let libraries = libraries.build();

    // ── assets (existence + size only — hashing every object would
    //    take seconds; size catches truncation which is the common
    //    failure mode) ───────────────────────────────────────────
    let mut assets = CategoryBuilder::new("assets");
    if let Some(vi) = &version_info {
        let index_path = assets_dir
            .join("indexes")
            .join(format!("{}.json", vi.assets));
        if !index_path.exists() {
            assets.checked = 1;
            assets.add_issue(
                format!("assets/indexes/{}.json", vi.assets),
                "missing",
                true,
            );
        } else {
            match versions::load_asset_index(&index_path) {
                Ok(index) => {
                    let objects_dir = assets_dir.join("objects");
                    for obj in index.objects.values() {
                        if obj.hash.len() < 2 {
                            continue;
                        }
                        assets.checked += 1;
                        let p = objects_dir.join(&obj.hash[..2]).join(&obj.hash);
                        let display = format!("assets/objects/{}/{}", &obj.hash[..2], &obj.hash);
                        match std::fs::metadata(&p) {
                            Err(_) => {
                                if assets.issue_count < MAX_ISSUES_LISTED as u32 {
                                    assets.add_issue(&display, "missing", true);
                                } else {
                                    assets.issue_count += 1;
                                }
                            }
                            Ok(m) if m.len() != obj.size => {
                                if assets.issue_count < MAX_ISSUES_LISTED as u32 {
                                    assets.add_issue(&display, "corrupt", true);
                                } else {
                                    assets.issue_count += 1;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(_) => {
                    assets.checked = 1;
                    assets.add_issue(
                        format!("assets/indexes/{}.json", vi.assets),
                        "corrupt",
                        true,
                    );
                }
            }
        }
    }
    let assets = assets.build();

    // ── loader profile ──────────────────────────────────────────
    let mut loader = CategoryBuilder::new("loader");
    if !matches!(instance.loader, LoaderType::Vanilla) {
        loader.checked = 1;
        if instance.loader_profile.is_none() {
            loader.add_issue(
                format!("{}/profile", loader_type_str(&instance.loader)),
                "missing",
                instance.loader_version.is_some(),
            );
        }
    }
    let loader = loader.build();

    // ── java ────────────────────────────────────────────────────
    let mut java = CategoryBuilder::new("java");
    if let Some(vi) = &version_info {
        let required = vi.required_java_major();
        java.checked = 1;
        let mut resolved: Option<u32> = None;
        let mut explicit_invalid = false;

        for explicit in [instance.java_path.as_ref(), config.java_path.as_ref()]
            .into_iter()
            .flatten()
        {
            if !explicit.exists() {
                explicit_invalid = true;
                continue;
            }
            match crate::jvm::detect_java_major(explicit) {
                Some(major) if major == required => {
                    resolved = Some(major);
                    explicit_invalid = false;
                    break;
                }
                Some(_) => {
                    // Valid binary but wrong major — launch would use it and
                    // fail; report so the user can fix the instance setting.
                    explicit_invalid = true;
                }
                None => explicit_invalid = true,
            }
        }

        if resolved.is_none() && !explicit_invalid {
            let installations = crate::launch::find_all_java_installations(&config.data_dir);
            if installations.iter().any(|j| j.major_version == required) {
                resolved = Some(required);
            }
        }

        if explicit_invalid {
            let path = instance
                .java_path
                .as_ref()
                .or(config.java_path.as_ref())
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            java.add_issue(path, "invalid", false);
        } else if resolved.is_none() {
            java.add_issue(format!("Java {}", required), "missing", true);
        }
    }
    let java = java.build();

    // ── content (read-only) ─────────────────────────────────────
    let mut categories = vec![metadata, minecraft, libraries, assets, loader, java];
    let mut content_issues_count = 0;
    if scope == VerifyScope::All {
        let content_issues =
            scan_content(&instances_dir, instance, &["mods", "resourcepacks", "shaderpacks"]);
        content_issues_count = content_issues.len() as u32;
        categories.push(VerifyCategory {
            id: "content".to_string(),
            checked: 0,
            issue_count: content_issues_count,
            issues: content_issues,
        });
    }
    let repairable_count = categories
        .iter()
        .filter(|c| c.id != "content")
        .map(|c| c.issue_count)
        .sum();
    let healthy = categories.iter().all(|c| c.issue_count == 0);

    VerifyReport {
        categories,
        repairable_count,
        content_issues: content_issues_count,
        healthy,
    }
}

/// Collect client JAR + vanilla libraries that are missing or fail SHA1
/// verification, as `(url, path, sha1, size)` tuples for `download_file`.
fn collect_repair_targets(
    version_info: &VersionInfo,
    libraries_dir: &PathBuf,
    versions_dir: &PathBuf,
) -> Vec<(String, PathBuf, String, u64)> {
    let mut targets = Vec::new();

    // Client JAR (legacy `{mc}.jar` counts as present — renamed below).
    let client_path = versions_dir
        .join(&version_info.id)
        .join("client.jar");
    let legacy_path = versions_dir
        .join(&version_info.id)
        .join(format!("{}.jar", version_info.id));
    let expected_sha1 = version_info.downloads.client.sha1.as_str();
    let client_ok = if client_path.exists() {
        expected_sha1.is_empty()
            || download::verify_sha1(&client_path, expected_sha1).ok() == Some(true)
    } else {
        legacy_path.exists()
    };
    if !client_ok {
        targets.push((
            version_info.downloads.client.url.clone(),
            client_path,
            expected_sha1.to_string(),
            version_info.downloads.client.size,
        ));
    }

    // Vanilla libraries.
    for lib in &version_info.libraries {
        if !versions::should_include_library(lib) {
            continue;
        }
        if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let p = libraries_dir.join(&artifact.path);
                let ok = p.exists()
                    && (artifact.sha1.is_empty()
                        || download::verify_sha1(&p, &artifact.sha1).ok() == Some(true));
                if !ok {
                    targets.push((
                        artifact.url.clone(),
                        p,
                        artifact.sha1.clone(),
                        artifact.size,
                    ));
                }
            }
            if let Some(classifier) = versions::native_classifier(lib) {
                if let Some(classifiers) = &downloads.classifiers {
                    if let Some(native) = classifiers.get(classifier.as_str()) {
                        if let (Some(url), Some(path), Some(sha1), Some(size)) = (
                            native.get("url").and_then(|v| v.as_str()),
                            native.get("path").and_then(|v| v.as_str()),
                            native.get("sha1").and_then(|v| v.as_str()),
                            native.get("size").and_then(|v| v.as_u64()),
                        ) {
                            let p = libraries_dir.join(path);
                            let ok = p.exists()
                                && (sha1.is_empty()
                                    || download::verify_sha1(&p, sha1).ok() == Some(true));
                            if !ok {
                                targets.push((url.to_string(), p, sha1.to_string(), size));
                            }
                        }
                    }
                }
            }
        } else if let Some(url_base) = &lib.url {
            let rel = versions::maven_to_path(&lib.name);
            let p = libraries_dir.join(&rel);
            if !p.exists() {
                targets.push((format!("{}{}", url_base, rel), p, String::new(), 0));
            }
        }
    }

    targets
}

/// Re-download broken launcher-managed files and restore missing metadata.
///
/// Emits `repair_progress` events: `{ stage, percent, message }`.
/// User content (mods, resource packs, shader packs) is never modified.
pub async fn repair_instance(
    app: &AppHandle,
    config: &AppConfig,
    instance: &Instance,
) -> Result<RepairSummary> {
    let mut summary = RepairSummary::default();

    let emit = |stage: &str, percent: f64, message: &str| {
        let _ = app.emit(
            "repair_progress",
            RepairProgress {
                stage: stage.to_string(),
                percent,
                message: message.to_string(),
            },
        );
    };

    let versions_dir = config.versions_dir();
    let libraries_dir = config.libraries_dir();
    let assets_dir = config.assets_dir();
    let instances_dir = config.instances_dir();

    // ── 1. metadata ─────────────────────────────────────────────
    emit("metadata", 2.0, "Checking version metadata...");
    let mut version_info = load_local_version_info(&versions_dir, &instance.mc_version);
    if version_info.is_none() {
        emit("metadata", 4.0, "Fetching version metadata...");
        let manifest = versions::fetch_version_manifest().await?;
        let url = manifest
            .versions
            .iter()
            .find(|v| v.id == instance.mc_version)
            .map(|v| v.url.clone())
            .ok_or_else(|| {
                LauncherError::Version(format!(
                    "Version {} not found in Mojang manifest",
                    instance.mc_version
                ))
            })?;
        let info = versions::fetch_version_info(&url).await?;
        let version_dir = versions_dir.join(&instance.mc_version);
        std::fs::create_dir_all(&version_dir)?;
        let json_path = version_dir.join(format!("{}.json", instance.mc_version));
        std::fs::write(&json_path, serde_json::to_string_pretty(&info)?)?;
        summary.repaired += 1;
        version_info = Some(info);
    }
    let vi = version_info.ok_or_else(|| {
        LauncherError::Version("Version metadata unavailable after fetch".to_string())
    })?;

    // Legacy `{mc}.jar` → `client.jar` migration (same as install check).
    let client_path = versions_dir.join(&instance.mc_version).join("client.jar");
    let legacy_path = versions_dir
        .join(&instance.mc_version)
        .join(format!("{}.jar", instance.mc_version));
    if !client_path.exists() && legacy_path.exists() {
        std::fs::rename(&legacy_path, &client_path).ok();
    }

    // ── 2. client JAR + libraries ───────────────────────────────
    emit("files", 8.0, "Checking game files...");
    let mut targets = collect_repair_targets(&vi, &libraries_dir, &versions_dir);
    if let Some(profile) = &instance.loader_profile {
        for llib in &profile.libraries {
            let p = libraries_dir.join(&llib.path);
            let sha1 = llib.sha1.clone().unwrap_or_default();
            let ok = p.exists()
                && (sha1.is_empty()
                    || download::verify_sha1(&p, &sha1).ok() == Some(true));
            if !ok {
                if llib.url.is_empty() {
                    summary.failed += 1;
                    summary.errors.push(format!("No download URL for {}", llib.path));
                } else {
                    targets.push((format!("{}{}", llib.url, llib.path), p, sha1, llib.size.unwrap_or(0)));
                }
            }
        }
    }

    let total_files = targets.len();
    for (i, (url, path, sha1, _size)) in targets.into_iter().enumerate() {
        let pct = 8.0 + (i as f64 / total_files.max(1) as f64) * 37.0;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        emit(
            "files",
            pct,
            &format!("Downloading {}/{}: {}", i + 1, total_files, name),
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        match download::download_file(&url, &path, &sha1).await {
            Ok(()) => summary.repaired += 1,
            Err(e) => {
                summary.failed += 1;
                summary.errors.push(format!("{}: {}", name, e));
                tracing::warn!(target: "launcher", "Repair download failed for {}: {}", path.display(), e);
            }
        }
    }

    // ── 3. assets ───────────────────────────────────────────────
    emit("assets", 48.0, "Checking assets...");
    let index_path = assets_dir
        .join("indexes")
        .join(format!("{}.json", vi.assets));
    let mut asset_index = if index_path.exists() {
        versions::load_asset_index(&index_path).ok()
    } else {
        None
    };
    if asset_index.is_none() {
        emit("assets", 50.0, "Fetching asset index...");
        if !crate::is_allowed_download_host(&vi.asset_index.url) {
            return Err(LauncherError::Download(
                "Access denied: asset index host not allowed".to_string(),
            ));
        }
        let index = versions::fetch_asset_index(&vi.asset_index.url).await?;
        let indexes_dir = assets_dir.join("indexes");
        std::fs::create_dir_all(&indexes_dir)?;
        std::fs::write(&index_path, serde_json::to_string_pretty(&index)?)?;
        summary.repaired += 1;
        asset_index = Some(index);
    }
    if let Some(index) = &asset_index {
        let objects_dir = assets_dir.join("objects");
        // Remove size-mismatched objects so download_assets re-fetches them
        // (it only looks at existence).
        for obj in index.objects.values() {
            if obj.hash.len() < 2 {
                continue;
            }
            let p = objects_dir.join(&obj.hash[..2]).join(&obj.hash);
            if let Ok(m) = std::fs::metadata(&p) {
                if m.len() != obj.size {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        emit("assets", 55.0, "Downloading assets...");
        let missing_before = index
            .objects
            .values()
            .filter(|o| {
                o.hash.len() >= 2 && !objects_dir.join(&o.hash[..2]).join(&o.hash).exists()
            })
            .count();
        match download::download_assets(index, &assets_dir, |completed, total, _, _, _| {
            let frac = if total > 0 {
                completed as f64 / total as f64
            } else {
                0.0
            };
            emit(
                "assets",
                55.0 + frac * 30.0,
                &format!("Downloading assets ({}/{})", completed, total),
            );
        })
        .await
        {
            Ok(()) => summary.repaired += missing_before as u32,
            Err(e) => {
                summary.failed += 1;
                summary.errors.push(format!("Assets: {}", e));
            }
        }
        let _ = download::ensure_virtual_assets(&vi.assets, index, &assets_dir);
    }

    // ── 4. loader profile ───────────────────────────────────────
    if !matches!(instance.loader, LoaderType::Vanilla) && instance.loader_profile.is_none() {
        emit("loader", 88.0, "Installing mod loader...");
        match &instance.loader_version {
            Some(lv) => {
                let loader_str = loader_type_str(&instance.loader);
                match modloaders::install_loader(
                    loader_str,
                    &instance.mc_version,
                    lv,
                    &libraries_dir,
                    &versions_dir,
                    Some(app),
                )
                .await
                {
                    Ok(profile) => {
                        let mut inst = instance.clone();
                        inst.loader_profile = Some(profile);
                        instances::save_instance(&instances_dir, &inst, None)?;
                        summary.repaired += 1;
                    }
                    Err(e) => {
                        summary.failed += 1;
                        summary.errors.push(format!("Loader install: {}", e));
                        tracing::warn!(target: "launcher", "Loader repair failed: {}", e);
                    }
                }
            }
            None => {
                summary.failed += 1;
                summary.errors.push("No loader version recorded for this instance".to_string());
            }
        }
    }

    // ── 5. java ─────────────────────────────────────────────────
    let required = vi.required_java_major();
    let java_ok = {
        let mut ok = false;
        for explicit in [instance.java_path.as_ref(), config.java_path.as_ref()]
            .into_iter()
            .flatten()
        {
            if crate::jvm::detect_java_major(explicit) == Some(required) {
                ok = true;
                break;
            }
        }
        if !ok {
            let installations = crate::launch::find_all_java_installations(&config.data_dir);
            ok = installations.iter().any(|j| j.major_version == required);
        }
        ok
    };
    if !java_ok {
        emit("java", 92.0, &format!("Downloading Java {}...", required));
        match crate::java_download::download_java_runtime(required, &config.data_dir, app).await {
            Ok(_) => summary.repaired += 1,
            Err(e) => {
                summary.failed += 1;
                summary.errors.push(format!("Java {}: {}", required, e));
                tracing::warn!(target: "launcher", "Java repair failed: {}", e);
            }
        }
    }

    // ── 6. content is report-only ───────────────────────────────
    summary.content_skipped =
        scan_content(&instances_dir, instance, &["mods", "resourcepacks", "shaderpacks"]).len()
            as u32;

    emit("done", 100.0, "Repair complete");
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::Instance;
    use std::io::Write;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_verify_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_config(data_dir: &PathBuf) -> AppConfig {
        let mut config = AppConfig::default();
        config.data_dir = data_dir.clone();
        config
    }

    fn test_instance(mc_version: &str) -> Instance {
        Instance::new("Test", mc_version, 4096, "g1gc", LoaderType::Vanilla, None)
    }

    /// SHA1 of b"abc".
    const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

    fn write_version_json(
        config: &AppConfig,
        mc_version: &str,
        client_sha1: &str,
        client_size: u64,
        client_url: &str,
    ) {
        let version_dir = config.versions_dir().join(mc_version);
        std::fs::create_dir_all(&version_dir).unwrap();
        let json = serde_json::json!({
            "id": mc_version,
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "libraries": [],
            "downloads": {
                "client": {
                    "sha1": client_sha1,
                    "size": client_size,
                    "url": client_url
                }
            },
            "assetIndex": {
                "id": "5",
                "sha1": "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                "size": 0,
                "totalSize": 0,
                "url": "https://piston-meta.mojang.com/fake-index.json"
            },
            "assets": "5",
            "javaVersion": { "component": "java-runtime-delta", "majorVersion": 17 }
        });
        std::fs::write(
            version_dir.join(format!("{}.json", mc_version)),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();
    }

    fn write_file_with_sha1(path: &PathBuf, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    fn cat<'a>(report: &'a VerifyReport, id: &str) -> &'a VerifyCategory {
        report.categories.iter().find(|c| c.id == id).unwrap()
    }

    #[test]
    fn missing_client_jar_is_detected_and_repairable() {
        let dir = temp_dir("missing_jar");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");
        write_version_json(&config, "1.20.1", ABC_SHA1, 3, "https://example.com/client.jar");

        let report = verify_instance(&config, &instance);

        let mc = cat(&report, "minecraft");
        assert_eq!(mc.issue_count, 1);
        assert_eq!(mc.issues[0].kind, "missing");
        assert!(mc.issues[0].repairable);
        assert!(report.repairable_count >= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_client_jar_is_detected() {
        let dir = temp_dir("corrupt_jar");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");
        write_version_json(&config, "1.20.1", ABC_SHA1, 3, "https://example.com/client.jar");
        let client = config.versions_dir().join("1.20.1").join("client.jar");
        write_file_with_sha1(&client, b"not-abc");

        let report = verify_instance(&config, &instance);

        let mc = cat(&report, "minecraft");
        assert_eq!(mc.issue_count, 1);
        assert_eq!(mc.issues[0].kind, "corrupt");
        assert!(mc.issues[0].repairable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_client_jar_passes() {
        let dir = temp_dir("valid_jar");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");
        write_version_json(&config, "1.20.1", ABC_SHA1, 3, "https://example.com/client.jar");
        let client = config.versions_dir().join("1.20.1").join("client.jar");
        write_file_with_sha1(&client, b"abc");

        let report = verify_instance(&config, &instance);

        assert_eq!(cat(&report, "minecraft").issue_count, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_version_json_reports_metadata() {
        let dir = temp_dir("no_meta");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");

        let report = verify_instance(&config, &instance);

        let meta = cat(&report, "metadata");
        assert_eq!(meta.issue_count, 1);
        assert_eq!(meta.issues[0].kind, "missing");
        assert!(meta.issues[0].repairable);
        // Without metadata, libraries/assets/java have nothing to check.
        assert_eq!(cat(&report, "libraries").issue_count, 0);
        assert_eq!(cat(&report, "assets").issue_count, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_asset_index_is_repairable() {
        let dir = temp_dir("no_index");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");
        write_version_json(&config, "1.20.1", ABC_SHA1, 3, "https://example.com/client.jar");

        let report = verify_instance(&config, &instance);

        let assets = cat(&report, "assets");
        assert_eq!(assets.issue_count, 1);
        assert_eq!(assets.issues[0].kind, "missing");
        assert!(assets.issues[0].repairable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_asset_object_is_detected() {
        let dir = temp_dir("missing_obj");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");
        write_version_json(&config, "1.20.1", ABC_SHA1, 3, "https://example.com/client.jar");

        let indexes = config.assets_dir().join("indexes");
        std::fs::create_dir_all(&indexes).unwrap();
        let index = serde_json::json!({
            "objects": {
                "minecraft/sounds/test.ogg": { "hash": "aa".repeat(20), "size": 42 }
            }
        });
        std::fs::write(indexes.join("5.json"), serde_json::to_string(&index).unwrap()).unwrap();

        let report = verify_instance(&config, &instance);

        let assets = cat(&report, "assets");
        assert_eq!(assets.checked, 1);
        assert_eq!(assets.issue_count, 1);
        assert_eq!(assets.issues[0].kind, "missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_content_zip_is_reported_not_repairable() {
        let dir = temp_dir("bad_content");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");
        write_version_json(&config, "1.20.1", ABC_SHA1, 3, "https://example.com/client.jar");
        let client = config.versions_dir().join("1.20.1").join("client.jar");
        write_file_with_sha1(&client, b"abc");
        let indexes = config.assets_dir().join("indexes");
        std::fs::create_dir_all(&indexes).unwrap();
        std::fs::write(indexes.join("5.json"), r#"{"objects":{}}"#).unwrap();

        let mods = instance.mods_dir(&config.instances_dir());
        std::fs::create_dir_all(&mods).unwrap();
        write_file_with_sha1(&mods.join("broken.jar"), b"<html>404</html>");
        write_file_with_sha1(&mods.join("good.jar"), b"PK\x03\x04rest");
        write_file_with_sha1(&mods.join("disabled.jar.disabled"), b"not-a-zip");

        let report = verify_instance(&config, &instance);

        assert_eq!(report.content_issues, 1);
        let content = cat(&report, "content");
        assert_eq!(content.issue_count, 1);
        assert!(!content.issues[0].repairable);
        assert!(content.issues[0].path.contains("broken.jar"));
        // Content issues are never counted as repairable (java may or may
        // not be present on the host, so only assert the content side).
        let content_repairs: u32 = content
            .issues
            .iter()
            .filter(|i| i.repairable)
            .count() as u32;
        assert_eq!(content_repairs, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_scope_launcher_skips_user_content() {
        let dir = temp_dir("scope_launcher");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");
        let mods = instance.mods_dir(&config.instances_dir());
        write_file_with_sha1(&mods.join("broken.jar"), b"<html>404</html>");

        let report = verify_instance_scoped(&config, &instance, VerifyScope::Launcher);

        assert!(report.categories.iter().all(|c| c.id != "content"));
        assert_eq!(report.content_issues, 0);
        // Launcher categories are still checked (metadata is missing here).
        assert!(report.categories.iter().any(|c| c.id == "metadata"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_scope_mods_reports_content_only() {
        let dir = temp_dir("scope_mods");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");
        let mc = instance.minecraft_dir(&config.instances_dir());
        write_file_with_sha1(&mc.join("mods").join("broken.jar"), b"<html>404</html>");
        write_file_with_sha1(&mc.join("resourcepacks").join("broken.zip"), b"<html>404</html>");

        let report = verify_instance_scoped(&config, &instance, VerifyScope::Mods);

        assert_eq!(report.categories.len(), 1, "no launcher categories in a content scope");
        let content = cat(&report, "content");
        assert_eq!(content.issue_count, 1, "resource packs stay outside the mods scope");
        assert!(content.issues[0].path.starts_with("mods/"));
        assert_eq!(report.repairable_count, 0, "content-only scope has nothing to repair");
        assert!(!report.healthy);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_scope_parse_accepts_names_and_rejects_unknown() {
        assert_eq!(VerifyScope::parse("all").unwrap(), VerifyScope::All);
        assert_eq!(VerifyScope::parse("Launcher").unwrap(), VerifyScope::Launcher);
        assert_eq!(
            VerifyScope::parse("resourcepacks").unwrap(),
            VerifyScope::Resourcepacks
        );
        assert_eq!(
            VerifyScope::parse("shaderpacks").unwrap(),
            VerifyScope::Shaderpacks
        );
        assert!(VerifyScope::parse("everything").is_err());
    }

    #[test]
    fn healthy_instance_reports_zero_issues_except_java() {
        let dir = temp_dir("healthy");
        let config = test_config(&dir);
        let instance = test_instance("1.20.1");
        write_version_json(&config, "1.20.1", ABC_SHA1, 3, "https://example.com/client.jar");
        let client = config.versions_dir().join("1.20.1").join("client.jar");
        write_file_with_sha1(&client, b"abc");

        let indexes = config.assets_dir().join("indexes");
        std::fs::create_dir_all(&indexes).unwrap();
        std::fs::write(indexes.join("5.json"), r#"{"objects":{}}"#).unwrap();

        let report = verify_instance(&config, &instance);

        // Java depends on what is installed on the host — skip it.
        for category in &report.categories {
            if category.id == "java" {
                continue;
            }
            assert_eq!(
                category.issue_count, 0,
                "unexpected issues in {}: {:?}",
                category.id, category.issues
            );
        }
        assert_eq!(report.content_issues, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repair_targets_include_corrupt_library() {
        let dir = temp_dir("targets");
        let config = test_config(&dir);
        let libraries_dir = config.libraries_dir();
        let versions_dir = config.versions_dir();

        let json = serde_json::json!({
            "id": "1.20.1",
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "libraries": [
                {
                    "name": "com.example:good:1.0",
                    "downloads": {
                        "artifact": {
                            "path": "com/example/good/1.0/good-1.0.jar",
                            "sha1": ABC_SHA1,
                            "size": 3,
                            "url": "https://example.com/good.jar"
                        }
                    }
                },
                {
                    "name": "com.example:bad:1.0",
                    "downloads": {
                        "artifact": {
                            "path": "com/example/bad/1.0/bad-1.0.jar",
                            "sha1": ABC_SHA1,
                            "size": 3,
                            "url": "https://example.com/bad.jar"
                        }
                    }
                }
            ],
            "downloads": {
                "client": {
                    "sha1": ABC_SHA1,
                    "size": 3,
                    "url": "https://example.com/client.jar"
                }
            },
            "assetIndex": {
                "id": "5",
                "sha1": "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                "size": 0,
                "totalSize": 0,
                "url": "https://piston-meta.mojang.com/fake-index.json"
            },
            "assets": "5"
        });
        let vi: VersionInfo = serde_json::from_value(json).unwrap();

        // good: present with correct sha1; bad: present with wrong sha1;
        // client: missing.
        let good = libraries_dir.join("com/example/good/1.0/good-1.0.jar");
        write_file_with_sha1(&good, b"abc");
        let bad = libraries_dir.join("com/example/bad/1.0/bad-1.0.jar");
        write_file_with_sha1(&bad, b"wrong");

        let targets = collect_repair_targets(&vi, &libraries_dir, &versions_dir);
        let paths: Vec<String> = targets
            .iter()
            .map(|(_, p, _, _)| p.to_string_lossy().to_string())
            .collect();

        assert_eq!(targets.len(), 2, "expected client + bad lib, got {:?}", paths);
        assert!(paths.iter().any(|p| p.contains("client.jar")));
        assert!(paths.iter().any(|p| p.contains("bad-1.0.jar")));
        assert!(!paths.iter().any(|p| p.contains("good-1.0.jar")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

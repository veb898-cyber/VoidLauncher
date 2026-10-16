use crate::error::{LauncherError, Result};
use crate::modloaders::LoaderProfile;
use chrono::Utc;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Instance configuration
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Instance {
    /// Unique instance name
    pub name: String,
    /// Minecraft version ID (e.g., "26.1.2")
    pub mc_version: String,
    /// Mod loader type
    pub loader: LoaderType,
    /// Mod loader version (if applicable)
    pub loader_version: Option<String>,
    /// Cached loader profile (main class override, libraries, args)
    pub loader_profile: Option<LoaderProfile>,
    /// Custom JVM memory in MB (None = use global default)
    pub memory_mb: Option<u32>,
    /// Custom JVM arguments (None = use global default)
    pub jvm_args: Option<Vec<String>>,
    /// GC preset: "standard" | "g1gc" | "zgc" (None = default to "g1gc")
    #[serde(default)]
    pub gc_preset: Option<String>,
    /// Custom Java path (None = use global default / auto-detect)
    pub java_path: Option<PathBuf>,
    /// Custom game resolution
    pub resolution: Option<Resolution>,
    /// Instance icon (base64 or path)
    pub icon: Option<String>,
    /// Instance banner for the home page card (base64 data URL)
    #[serde(default)]
    pub banner: Option<String>,
    /// When the instance was created
    pub created_at: String,
    /// When the instance was last played
    pub last_played: Option<String>,
    /// Total play time in seconds
    pub play_time_seconds: u64,
    /// Notes / description
    pub notes: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum LoaderType {
    Vanilla,
    Fabric,
    Forge,
    NeoForge,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Instance {
    /// Create a new vanilla instance with sensible memory/GC defaults.
    /// `default_memory_mb` should be the tiered recommendation (4/6/8 GB) from
    /// `config::recommended_memory_mb`.
    /// `default_gc_preset` should be "standard" | "g1gc" | "zgc".
    pub fn new(
        name: &str,
        mc_version: &str,
        default_memory_mb: u32,
        default_gc_preset: &str,
        loader: LoaderType,
        loader_version: Option<String>,
    ) -> Self {
        Self {
            name: name.to_string(),
            mc_version: mc_version.to_string(),
            loader,
            loader_version,
            loader_profile: None,
            // Pre-fill with the recommended default so the slider sits at the
            // optimal position the moment the instance is created, even if the
            // user never opens the editor. The user can still override.
            memory_mb: Some(default_memory_mb),
            jvm_args: None,
            gc_preset: Some(default_gc_preset.to_string()),
            java_path: None,
            resolution: None,
            icon: None,
            banner: None,
            created_at: Utc::now().to_rfc3339(),
            last_played: None,
            play_time_seconds: 0,
            notes: String::new(),
        }
    }

    /// Get the instance directory path
    pub fn dir(&self, instances_dir: &Path) -> PathBuf {
        instances_dir.join(&self.name)
    }

    /// Get the .minecraft directory inside the instance
    pub fn minecraft_dir(&self, instances_dir: &Path) -> PathBuf {
        self.dir(instances_dir).join(".minecraft")
    }

    /// Get mods directory
    pub fn mods_dir(&self, instances_dir: &Path) -> PathBuf {
        self.minecraft_dir(instances_dir).join("mods")
    }

    /// Get config file path
    pub fn config_file(&self, instances_dir: &Path) -> PathBuf {
        self.dir(instances_dir).join("instance.json")
    }

    /// Get Prism-compatible instance.cfg path
    pub fn prism_cfg_file(&self, instances_dir: &Path) -> PathBuf {
        self.dir(instances_dir).join("instance.cfg")
    }
}

/// List all instances (supports both instance.json and instance.cfg)
pub fn list_instances(instances_dir: &Path) -> Result<Vec<Instance>> {
    let mut instances = Vec::new();

    if !instances_dir.exists() {
        return Ok(instances);
    }

    for entry in std::fs::read_dir(instances_dir)? {
        let entry = entry?;
        let dir_path = entry.path();
        if !dir_path.is_dir() {
            continue;
        }
        let json_path = dir_path.join("instance.json");
        let cfg_path = dir_path.join("instance.cfg");

        if json_path.exists() {
            match std::fs::read_to_string(&json_path) {
                Ok(contents) => match serde_json::from_str::<Instance>(&contents) {
                    Ok(instance) => instances.push(instance),
                    Err(e) => {
                        tracing::warn!(target: "launcher", "Failed to parse instance at {:?}: {}", json_path, e)
                    }
                },
                Err(e) => {
                    tracing::warn!(target: "launcher", "Failed to read instance at {:?}: {}", json_path, e)
                }
            }
        } else if cfg_path.exists() {
            // Import Prism/MultiMC format on the fly
            if let Some(instance) = parse_prism_cfg(&cfg_path) {
                // Save as instance.json for future fast loading
                let json_path = dir_path.join("instance.json");
                if let Ok(json) = serde_json::to_string_pretty(&instance) {
                    let _ = std::fs::write(&json_path, &json);
                }
                instances.push(instance);
            }
        }
    }

    // Sort by last played, then by name
    instances.sort_by(|a, b| {
        b.last_played
            .as_deref()
            .unwrap_or("")
            .cmp(a.last_played.as_deref().unwrap_or(""))
    });

    Ok(instances)
}

/// Create a new instance
pub fn create_instance(instances_dir: &Path, instance: &Instance) -> Result<()> {
    let dir = instance.dir(instances_dir);
    if dir.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' already exists",
            instance.name
        )));
    }

    // Create directory structure
    std::fs::create_dir_all(instance.minecraft_dir(instances_dir))?;
    std::fs::create_dir_all(instance.mods_dir(instances_dir))?;
    std::fs::create_dir_all(instance.minecraft_dir(instances_dir).join("resourcepacks"))?;
    std::fs::create_dir_all(instance.minecraft_dir(instances_dir).join("shaderpacks"))?;
    std::fs::create_dir_all(instance.minecraft_dir(instances_dir).join("config"))?;

    // Save instance config
    save_instance(instances_dir, instance, None)?;

    Ok(())
}

/// Save instance config to disk, plus Prism-compatible instance.cfg and pack.png.
/// If `old_name` differs from `instance.name`, the instance directory is renamed first.
pub fn save_instance(
    instances_dir: &Path,
    instance: &Instance,
    old_name: Option<&str>,
) -> Result<()> {
    // Rename directory if name changed
    if let Some(prev) = old_name {
        if prev != instance.name {
            let old_dir = instances_dir.join(prev);
            let new_dir = instances_dir.join(&instance.name);
            if old_dir.exists() && !new_dir.exists() {
                // Try rename first; on Windows this may fail with Access Denied
                // if any file handle is still open. Fall back to copy + delete.
                if let Err(rename_err) = std::fs::rename(&old_dir, &new_dir) {
                    tracing::warn!(target: "launcher", "fs::rename failed ({}), falling back to copy+delete", rename_err);
                    copy_dir_recursive(&old_dir, &new_dir)?;
                    std::fs::remove_dir_all(&old_dir)?;
                }
            }
        }
    }

    let config_path = instance.config_file(instances_dir);
    let config_dir = config_path
        .parent()
        .ok_or_else(|| LauncherError::Instance("Instance config has no parent directory".into()))?;
    if !config_dir.exists() {
        std::fs::create_dir_all(config_dir)?;
    }
    let json = serde_json::to_string_pretty(instance)?;
    std::fs::write(&config_path, json)?;

    // Write Prism-compatible instance.cfg
    write_prism_cfg(instance, instances_dir)?;

    // Write Prism-compatible mmc-pack.json
    write_mmc_pack_json(instance, instances_dir)?;

    // Write pack.png if instance has an icon
    if let Some(ref icon) = instance.icon {
        write_pack_png(instances_dir, &instance.name, icon);
    }

    Ok(())
}

/// Delete an instance
pub fn delete_instance(instances_dir: &Path, name: &str) -> Result<()> {
    let dir = instances_dir.join(name);
    if !dir.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

/// Get an instance by name (supports both instance.json and instance.cfg)
pub fn get_instance(instances_dir: &Path, name: &str) -> Result<Instance> {
    let dir = instances_dir.join(name);
    let json_path = dir.join("instance.json");
    let cfg_path = dir.join("instance.cfg");

    if json_path.exists() {
        let contents = std::fs::read_to_string(&json_path)?;
        let instance = serde_json::from_str(&contents)?;
        return Ok(instance);
    }

    if cfg_path.exists() {
        if let Some(instance) = parse_prism_cfg(&cfg_path) {
            // Save as instance.json for future fast loading
            if let Ok(json) = serde_json::to_string_pretty(&instance) {
                let _ = std::fs::write(&json_path, &json);
            }
            return Ok(instance);
        }
    }

    Err(LauncherError::Instance(format!(
        "Instance '{}' not found",
        name
    )))
}

/// What the user chose to leave out of a duplicate (roadmap: duplicate must
/// show what it copies and let saves / screenshots / logs be excluded).
#[derive(Debug, Clone, Copy, Default)]
pub struct DuplicateExclusions {
    pub saves: bool,
    pub screenshots: bool,
    /// Per-launch junk: `.minecraft/logs` and `.minecraft/crash-reports`.
    pub logs: bool,
}

/// Duplicate an instance. Metadata, mods, configs, resource packs and
/// shaders always travel to the copy; the source's backup history (`.backups`)
/// and parked update stashes never do — they belong to the original, and a
/// copy inheriting its backup list would offer restic restores of the wrong
/// instance. `exclude` drops the user's optional picks (saves, screenshots,
/// logs with crash reports).
pub fn duplicate_instance(
    instances_dir: &Path,
    name: &str,
    new_name: &str,
    exclude: DuplicateExclusions,
) -> Result<Instance> {
    let src = instances_dir.join(name);
    let dst = instances_dir.join(new_name);
    if !src.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    if dst.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' already exists",
            new_name
        )));
    }
    copy_dir_filtered(&src, &dst, "", &|rel| duplicate_skip(rel, exclude))?;
    // Read and update the instance config
    let mut instance = get_instance(instances_dir, new_name)?;
    instance.name = new_name.to_string();
    instance.last_played = None;
    instance.play_time_seconds = 0;
    instance.created_at = Utc::now().to_rfc3339();
    save_instance(instances_dir, &instance, None)?;
    Ok(instance)
}

/// Paths (relative to the instance root, `/`-separated) a duplicate leaves
/// behind. Directories matched here are not descended into at all.
fn duplicate_skip(rel: &str, exclude: DuplicateExclusions) -> bool {
    // Backup history and parked stashes always stay with the source.
    if rel == ".backups" || rel.starts_with(".backups/") {
        return true;
    }
    if rel.ends_with(".voidlauncher-old") {
        return true;
    }
    let mc = rel.strip_prefix(".minecraft/").unwrap_or("");
    if mc.is_empty() && rel != ".minecraft" {
        // Top-level entry (metadata, icon, …) — never excluded.
        return false;
    }
    let dir = mc.split('/').next().unwrap_or("");
    match dir {
        "saves" => exclude.saves,
        "screenshots" => exclude.screenshots,
        "logs" | "crash-reports" => exclude.logs,
        _ => false,
    }
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    copy_dir_filtered(src, dst, "", &|_| false)
}

/// Recursive copy that asks `skip` about each entry's path relative to the
/// copy root (`/`-separated). A skipped directory is not descended into.
fn copy_dir_filtered(
    src: &std::path::Path,
    dst: &std::path::Path,
    rel_prefix: &str,
    skip: &dyn Fn(&str) -> bool,
) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = if rel_prefix.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", rel_prefix, name)
        };
        if skip(&rel) {
            continue;
        }
        let dst_path = dst.join(&name);
        if path.is_dir() {
            copy_dir_filtered(&path, &dst_path, &rel, skip)?;
        } else {
            std::fs::copy(&path, &dst_path)?;
        }
    }
    Ok(())
}

/// Save instance icon (base64 data URL)
pub fn save_instance_icon(instances_dir: &Path, name: &str, icon_data: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.icon = Some(icon_data.to_string());
    save_instance(instances_dir, &instance, None)?;
    Ok(())
}

/// Save instance banner (base64 data URL or gradient:name, empty to remove)
pub fn save_instance_banner(instances_dir: &Path, name: &str, banner_data: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.banner = if banner_data.is_empty() {
        None
    } else {
        Some(banner_data.to_string())
    };
    save_instance(instances_dir, &instance, None)?;
    Ok(())
}

/// List saves (worlds)
pub fn list_saves(instances_dir: &Path, name: &str) -> Result<Vec<SaveEntry>> {
    let instance = get_instance(instances_dir, name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let mut saves = Vec::new();
    if !saves_dir.exists() {
        return Ok(saves);
    }
    for entry in std::fs::read_dir(&saves_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let path = entry.path();
            let world_name = entry.file_name().to_string_lossy().to_string();
            let last_modified = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64
                });
            let level = parse_level_dat(&path.join("level.dat"));
            let game_mode = level.game_type.map(|gt| match gt {
                0 => "Survival".to_string(),
                1 => "Creative".to_string(),
                2 => "Adventure".to_string(),
                3 => "Hardcore".to_string(),
                _ => format!("Type {gt}"),
            });
            let icon_data = read_world_icon(instances_dir, name, &world_name);
            saves.push(SaveEntry {
                name: world_name,
                last_modified,
                size_bytes: dir_size(&path),
                game_mode,
                seed: level.seed,
                icon_data,
            });
        }
    }
    saves.sort_by(|a, b| {
        b.last_modified
            .unwrap_or(0)
            .cmp(&a.last_modified.unwrap_or(0))
    });
    Ok(saves)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SaveEntry {
    pub name: String,
    pub last_modified: Option<i64>,
    pub size_bytes: u64,
    #[serde(default)]
    pub game_mode: Option<String>,
    #[serde(default)]
    pub seed: Option<i64>,
    #[serde(default)]
    pub icon_data: Option<String>,
}

/// List screenshots
pub fn list_screenshots(instances_dir: &Path, name: &str) -> Result<Vec<ScreenshotEntry>> {
    let instance = get_instance(instances_dir, name)?;
    let dir = instance.minecraft_dir(instances_dir).join("screenshots");
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext == "png" || ext == "jpg" || ext == "jpeg" {
            let meta = std::fs::metadata(&path)?;
            out.push(ScreenshotEntry {
                filename: entry.file_name().to_string_lossy().to_string(),
                last_modified: meta.modified().ok().map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64
                }),
                size_bytes: meta.len(),
            });
        }
    }
    out.sort_by(|a, b| {
        b.last_modified
            .unwrap_or(0)
            .cmp(&a.last_modified.unwrap_or(0))
    });
    Ok(out)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScreenshotEntry {
    pub filename: String,
    pub last_modified: Option<i64>,
    pub size_bytes: u64,
}

pub fn read_screenshot(
    instances_dir: &Path,
    instance_name: &str,
    filename: &str,
) -> Result<String> {
    let instance = get_instance(instances_dir, instance_name)?;
    let path = instance
        .minecraft_dir(instances_dir)
        .join("screenshots")
        .join(filename);
    let buf = std::fs::read(&path)
        .map_err(|e| LauncherError::Instance(format!("Cannot read {}: {}", path.display(), e)))?;
    if buf.is_empty() {
        return Err(LauncherError::Instance(format!(
            "Screenshot is empty: {}",
            path.display()
        )));
    }
    // Screenshots may be PNG or JPG depending on the in-game setting; sniff the
    // MIME from the file extension so the browser decodes the data URL correctly.
    let mime = match Path::new(filename).extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") => {
            "image/jpeg"
        }
        Some(ext) if ext.eq_ignore_ascii_case("png") => "image/png",
        _ => "image/png",
    };
    Ok(format!("data:{mime};base64,{}", base64_encode(&buf)))
}

pub fn delete_screenshot(instances_dir: &Path, instance_name: &str, filename: &str) -> Result<()> {
    // The filename reaches the filesystem through the command layer; strip
    // any path components so a crafted name cannot escape the screenshots dir.
    let safe_filename = Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| LauncherError::Instance("Invalid screenshot filename".into()))?;
    if safe_filename.is_empty() || safe_filename == "." || safe_filename == ".." {
        return Err(LauncherError::Instance(
            "Invalid screenshot filename".into(),
        ));
    }
    let instance = get_instance(instances_dir, instance_name)?;
    let path = instance
        .minecraft_dir(instances_dir)
        .join("screenshots")
        .join(safe_filename);
    if !path.exists() {
        return Err(LauncherError::Instance(format!(
            "Screenshot not found: {}",
            safe_filename
        )));
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

/// List resource packs or shader packs (returns entries without icon - fetch via cmd_get_pack_icon)
pub fn list_packs(instances_dir: &Path, name: &str, pack_type: &str) -> Result<Vec<PackEntry>> {
    let instance = get_instance(instances_dir, name)?;
    let packs_dir = instance.minecraft_dir(instances_dir).join(pack_type);
    let mut packs = Vec::new();
    if !packs_dir.exists() {
        return Ok(packs);
    }
    // Roll back any file parked by an interrupted update before listing.
    recover_stashed_content_files(&packs_dir);
    for entry in std::fs::read_dir(&packs_dir)? {
        let entry = entry?;
        let path = entry.path();
        let filename = entry.file_name().to_string_lossy().to_string();
        // Skip hidden files (incl. Prism `.index/`), sidecar metadata and
        // Prism/packwiz `.pw.toml` metadata files
        if filename.starts_with('.')
            || filename.ends_with(".voidlauncher.json")
            || filename.ends_with(".pw.toml")
        {
            continue;
        }
        let is_dir = path.is_dir();
        let meta = std::fs::metadata(&path)?;

        // Skip tiny non-zip config presets in shaderpacks (e.g. "Better MC - High.json" at 373 bytes)
        if pack_type == "shaderpacks" && !is_dir && meta.len() < 1024 {
            let lower = filename.to_lowercase();
            if lower.ends_with(".json")
                || lower.ends_with(".txt")
                || lower.ends_with(".cfg")
                || lower.ends_with(".properties")
                || lower.ends_with(".toml")
            {
                continue;
            }
        }

        // Read sidecar metadata (project_name is the Modrinth/CurseForge display name)
        let (provider, mut version, project_id, project_name) =
            read_pack_sidecar(&path).unwrap_or_default();
        // Local packs have no real version — fall back to a version found in
        // the filename, then to the MC version range implied by pack.mcmeta
        // (pack_format), so the Version column is never empty for them.
        if version.is_empty() {
            version = extract_version_from_filename(&filename);
        }
        if version.is_empty() {
            version = pack_format_to_mc_version(&path);
        }
        // Name resolution: sidecar project_name > pack.mcmeta pack.name/description > filename
        let name = if !project_name.is_empty() {
            Some(project_name)
        } else {
            if is_dir {
                read_pack_name_from_dir(&path)
            } else {
                read_pack_name_from_zip(&path)
            }
        }
        .unwrap_or_else(|| {
            let stem = Path::new(&filename)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&filename);
            let stem = stem.strip_suffix(".disabled").unwrap_or(stem);
            strip_minecraft_color_codes(stem)
        });
        packs.push(PackEntry {
            filename,
            name,
            is_dir,
            file_size: meta.len(),
            provider,
            version,
            project_id,
        });
    }
    packs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packs)
}

/// Read the icon for a single pack (mirrors mod icon approach)
pub fn read_pack_icon(
    instances_dir: &Path,
    instance_name: &str,
    pack_type: &str,
    filename: &str,
) -> Result<Option<String>> {
    let instance = get_instance(instances_dir, instance_name)?;
    let pack_path = instance
        .minecraft_dir(instances_dir)
        .join(pack_type)
        .join(filename);
    if !pack_path.exists() {
        return Ok(None);
    }
    if pack_path.is_dir() {
        Ok(read_pack_icon_from_dir(&pack_path))
    } else {
        Ok(read_pack_icon_from_zip(&pack_path))
    }
}

/// Stem of a content filename: strip `.disabled`, then `.jar`/`.zip`.
pub(crate) fn content_stem(filename: &str) -> &str {
    let s = filename.strip_suffix(".disabled").unwrap_or(filename);
    s.strip_suffix(".jar")
        .or_else(|| s.strip_suffix(".zip"))
        .unwrap_or(s)
}

/// Sidecar metadata path (new layout): a single hidden `.index/` folder
/// inside the content directory — same layout as Prism Launcher — instead
/// of a metadata file next to each mod/pack.
pub(crate) fn sidecar_meta_path(content_dir: &std::path::Path, filename: &str) -> PathBuf {
    content_dir
        .join(".index")
        .join(format!("{}.voidlauncher.json", content_stem(filename)))
}

/// Legacy sidecar path (old layout): a metadata file next to the content file.
pub(crate) fn legacy_sidecar_path(content_dir: &std::path::Path, filename: &str) -> PathBuf {
    content_dir.join(format!("{}.voidlauncher.json", content_stem(filename)))
}

/// Read sidecar metadata: new `.index/` layout first, legacy layout as fallback.
///
/// Legacy sidecars historically used two name variants: the content stem
/// (`Faithful 32x.voidlauncher.json`) and the full filename including the
/// extension (`Faithful 32x.zip.voidlauncher.json`). Both are probed.
pub(crate) fn read_sidecar_meta(
    content_dir: &std::path::Path,
    filename: &str,
) -> Option<serde_json::Value> {
    for p in [
        sidecar_meta_path(content_dir, filename),
        legacy_sidecar_path(content_dir, filename),
        content_dir.join(format!("{}.voidlauncher.json", filename)),
    ] {
        if let Ok(contents) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str(&contents) {
                return Some(v);
            }
        }
    }
    None
}

/// Remove sidecar metadata in both layouts (used when a content file is deleted).
pub(crate) fn remove_sidecar_meta(content_dir: &std::path::Path, filename: &str) {
    let _ = std::fs::remove_file(sidecar_meta_path(content_dir, filename));
    let _ = std::fs::remove_file(legacy_sidecar_path(content_dir, filename));
}

/// Suffix for a content file parked aside while its replacement downloads.
/// The parked name no longer ends in `.jar`/`.zip`, so game loaders and the
/// launcher's own content listings ignore it until it is restored or removed.
pub(crate) const STASH_SUFFIX: &str = ".voidlauncher-old";

/// Park the old content file(s) of an update aside by renaming them — never
/// deleting: if the replacement download fails, the old file must still be
/// there. An update replaces the mod regardless of the exact on-disk name, so
/// the exact old filename plus both `.jar`/`.zip` twins of its stem are
/// parked (covers `.disabled` variants and same-stem leftovers).
pub(crate) fn stash_old_content_files(
    content_dir: &std::path::Path,
    old_filename: &str,
) -> std::result::Result<Vec<(PathBuf, PathBuf)>, String> {
    // `old_filename` is untrusted frontend input: strip any path components
    // so it can never escape the content directory.
    let old = std::path::Path::new(old_filename)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or("Invalid old file name")?;
    let base = content_stem(old);
    let mut candidates: Vec<PathBuf> = Vec::new();
    for c in [
        content_dir.join(old),
        content_dir.join(format!("{}.jar", base)),
        content_dir.join(format!("{}.zip", base)),
    ] {
        if !candidates.contains(&c) {
            candidates.push(c);
        }
    }
    let mut stashed = Vec::new();
    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        let file_name = candidate
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid old file name")?;
        let park = content_dir.join(format!("{}{}", file_name, STASH_SUFFIX));
        // Drop a stale park left by an interrupted earlier update of the
        // same file — the live file below is the one the user has right now.
        let _ = std::fs::remove_file(&park);
        match std::fs::rename(&candidate, &park) {
            Ok(()) => stashed.push((candidate, park)),
            Err(e) => {
                // Put back anything already parked so a partial failure
                // never strands files under the park name.
                restore_stashed_files(&stashed);
                return Err(format!(
                    "Failed to park old file {}: {e}",
                    candidate.display()
                ));
            }
        }
    }
    Ok(stashed)
}

/// Put parked files back after a failed download so the user keeps the old
/// content exactly where it was.
pub(crate) fn restore_stashed_files(stashed: &[(PathBuf, PathBuf)]) {
    for (orig, park) in stashed {
        if !park.exists() {
            continue;
        }
        // A bad replacement may already sit at the original name (the
        // zip-magic check runs after the rename into place): drop it first.
        if orig.exists() {
            let _ = std::fs::remove_file(orig);
        }
        let _ = std::fs::rename(park, orig);
    }
}

/// Delete parked files after the replacement has been verified.
pub(crate) fn discard_stashed_files(stashed: &[(PathBuf, PathBuf)]) {
    for (_, park) in stashed {
        let _ = std::fs::remove_file(park);
    }
}

/// Roll back content files left parked by an interrupted update.
///
/// A crash between "park the old file" and "download the replacement" would
/// otherwise leave the mod invisible forever (a parked name is not loaded by
/// the game and is skipped by the content listings). Called from the listings:
/// if the original name is free the park is restored, if it is taken the park
/// is stale and gets removed.
pub(crate) fn recover_stashed_content_files(content_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(content_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some(base) = file_name.strip_suffix(STASH_SUFFIX) else {
            continue;
        };
        if base.is_empty() {
            continue;
        }
        let park = entry.path();
        let orig = content_dir.join(base);
        if orig.exists() {
            let _ = std::fs::remove_file(&park);
        } else {
            let _ = std::fs::rename(&park, &orig);
        }
    }
}

/// After a successful content update, drop the sidecar of the replaced file
/// when the new file has a different stem — otherwise it would describe a
/// file that no longer exists.
pub(crate) fn cleanup_old_sidecar_after_update(
    content_dir: &std::path::Path,
    old_filename: &str,
    new_filename: &str,
) {
    let Some(old) = std::path::Path::new(old_filename)
        .file_name()
        .and_then(|n| n.to_str())
    else {
        return;
    };
    if content_stem(old) != content_stem(new_filename) {
        remove_sidecar_meta(content_dir, old);
    }
}

/// Metadata extracted from a Prism Launcher packwiz `.pw.toml` file.
#[derive(Debug, Clone, Default)]
pub(crate) struct PackwizMeta {
    /// Filename the metadata refers to (from `filename = "..."`).
    pub filename: String,
    /// Display name (from `name = "..."`).
    pub name: String,
    /// Version (from `x-prismlauncher-version-number`, falls back to `version`).
    pub version: String,
    /// Provider: "Modrinth", "CurseForge" or "".
    pub provider: String,
    /// Project id on the provider (mod-id for Modrinth, project-id for CurseForge).
    pub project_id: String,
}

/// Load all packwiz `.pw.toml` metadata from a content dir — scanning both
/// the `.index/` folder (usual Prism layout) and the content dir root
/// (Prism exports shader packs' metadata as `<slug>.pw.toml` in the root),
/// keyed by the declared `filename`.
///
/// Prism Launcher stores per-file metadata (name, version, provider, project id)
/// in `.pw.toml` files next to mods/resourcepacks/shaderpacks. Our own
/// `.index/*.voidlauncher.json` sidecars take priority everywhere, but imported
/// Prism instances only have the `.pw.toml` files, so we read them as a fallback.
pub(crate) fn load_packwiz_index(content_dir: &Path) -> HashMap<String, PackwizMeta> {
    let mut map = HashMap::new();
    let scan_dirs: Vec<PathBuf> = vec![content_dir.join(".index"), content_dir.to_path_buf()];
    for index_dir in scan_dirs {
        let Ok(entries) = std::fs::read_dir(&index_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !file_name.ends_with(".pw.toml") {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(v) = contents.parse::<toml::Table>() else {
                continue;
            };
            let Some(filename) = v.get("filename").and_then(|f| f.as_str()) else {
                continue;
            };
            let mut meta = PackwizMeta {
                filename: filename.to_string(),
                ..Default::default()
            };
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                meta.name = name.to_string();
            }
            if let Some(ver) = v
                .get("x-prismlauncher-version-number")
                .and_then(|n| n.as_str())
            {
                meta.version = ver.to_string();
            } else if let Some(ver) = v.get("version").and_then(|n| n.as_str()) {
                meta.version = ver.to_string();
            }
            if let Some(pid) = v
                .get("update")
                .and_then(|u| u.get("modrinth"))
                .and_then(|m| m.get("mod-id"))
                .and_then(|m| m.as_str())
            {
                meta.provider = "Modrinth".to_string();
                meta.project_id = pid.to_string();
            } else if let Some(pid) = v
                .get("update")
                .and_then(|u| u.get("curseforge"))
                .and_then(|c| c.get("project-id"))
                .and_then(|c| c.as_integer())
            {
                meta.provider = "CurseForge".to_string();
                meta.project_id = pid.to_string();
            }
            map.insert(meta.filename.clone(), meta);
        }
    }
    map
}

/// Look up packwiz metadata for a single content file (if any).
pub(crate) fn read_packwiz_meta(content_dir: &Path, filename: &str) -> Option<PackwizMeta> {
    load_packwiz_index(content_dir).remove(filename)
}

fn read_pack_sidecar(pack_path: &std::path::Path) -> Option<(String, String, String, String)> {
    let filename = pack_path.file_name()?.to_string_lossy().to_string();
    if let Some(val) = read_sidecar_meta(pack_path.parent()?, &filename) {
        let provider = normalize_provider(val["provider"].as_str().unwrap_or(""));
        let version = val["version_number"].as_str().unwrap_or("").to_string();
        let project_id = val["project_id"].as_str().unwrap_or("").to_string();
        let project_name = val["project_name"].as_str().unwrap_or("").to_string();
        return Some((provider, version, project_id, project_name));
    }
    // Fallback: Prism packwiz metadata (.pw.toml in .index/)
    let pw = read_packwiz_meta(pack_path.parent()?, &filename)?;
    Some((pw.provider, pw.version, pw.project_id, pw.name))
}

/// Normalize a raw provider string from sidecar/packwiz metadata to the
/// display form used in the UI (mods and packs alike).
pub(crate) fn normalize_provider(raw: &str) -> String {
    match raw.to_lowercase().as_str() {
        "modrinth" => "Modrinth".to_string(),
        "curseforge" => "CurseForge".to_string(),
        "local" => "Local".to_string(),
        "atlauncher" => "ATLauncher".to_string(),
        other => other.to_string(),
    }
}

/// Strip Minecraft color/formatting codes (§a, §l, §r, etc.) and any underscores used as spaces
pub(crate) fn strip_minecraft_color_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Extract a plausible version from a file name (e.g. "Pack 1.20.1.zip"
/// → "1.20.1", "mod_v5.8.1.jar" → "5.8.1", "foo-1.20.1-4.7.jar" → "4.7",
/// "Hearths v1.0.5.mod.jar" → "1.0.5",
/// "twilightforest-1.20.1-4.3.2508-universal.jar" → "4.3.2508",
/// "curios-forge-5.14.1+1.20.1.jar" → "5.14.1+1.20.1",
/// "CataclysmCompat1.0.zip" → "1.0"). Returns "" when nothing usable.
pub(crate) fn extract_version_from_filename(filename: &str) -> String {
    let mut stem = filename
        .trim_end_matches(".disabled")
        .trim_end_matches(".zip")
        .trim_end_matches(".jar");
    // Strip trailing ".word" junk like ".mod" / ".jar2" that some CurseForge
    // downloads carry ("Hearths v1.0.5.mod.jar"); numeric suffixes are part of
    // the version and must stay.
    loop {
        if let Some(idx) = stem.rfind('.') {
            let suffix = &stem[idx + 1..];
            if !suffix.is_empty()
                && suffix.len() <= 8
                && suffix.chars().all(|c| c.is_ascii_alphanumeric())
                && suffix.chars().any(|c| c.is_ascii_alphabetic())
            {
                stem = &stem[..idx];
                continue;
            }
        }
        break;
    }
    // Strip trailing loader/build markers ("-forge", "-neoforge", "-universal",
    // "-all", ...) so the version right before them is exposed.
    loop {
        let lower = stem.to_lowercase();
        let mut cut = None;
        for marker in [
            "neoforge",
            "forge",
            "universal",
            "universal_jar",
            "fabric",
            "fml",
            "all",
            "mod",
            "srg",
            "dev",
            "official",
            "mapped",
        ] {
            let marker_len = marker.len();
            if lower.ends_with(&format!("-{}", marker)) || lower.ends_with(&format!("_{}", marker))
            {
                cut = Some(stem.len() - 1 - marker_len);
                break;
            }
        }
        match cut {
            Some(idx) => stem = &stem[..idx],
            None => break,
        }
    }
    let bytes = stem.as_bytes();
    let n = bytes.len();
    if n == 0 {
        return String::new();
    }

    // 1) Plain numeric tail: "Pack 1.20.1", "mod_v5.8.1", "foo-1.2.3"
    let mut end = n;
    while end > 0
        && (bytes[end - 1].is_ascii_whitespace()
            || bytes[end - 1] == b'['
            || bytes[end - 1] == b']')
    {
        end -= 1;
    }
    let mut start = end;
    let mut dots = 0;
    let mut seen_digit = false;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_digit() {
            seen_digit = true;
            start -= 1;
        } else if c == b'.' && seen_digit && dots < 3 {
            dots += 1;
            start -= 1;
        } else {
            break;
        }
    }
    if seen_digit {
        if start < n && bytes[start] == b'.' {
            start += 1;
        }
        let mut ver_end = end;
        while ver_end > start && bytes[ver_end - 1].is_ascii_alphabetic() {
            ver_end -= 1;
        }
        if ver_end > start {
            let version = String::from_utf8_lossy(&bytes[start..ver_end]).into_owned();
            if start == 0 {
                return version;
            }
            let prev = bytes[start - 1];
            if prev == b'v' || prev == b'V' || prev == b'r' || prev == b'R' {
                if start == 1
                    || bytes[start - 2].is_ascii_whitespace()
                    || bytes[start - 2] == b'-'
                    || bytes[start - 2] == b'_'
                {
                    return version;
                }
            } else if prev.is_ascii_whitespace() || prev == b'-' || prev == b'_' {
                return version;
            }
        }
    }

    // 2) Version token from the first separator that leads into a number
    //    ("name-1.11.2+1.20.1", "name-1.0.0-beta.49+1.20.1", "mod 5.14.1+1.20.1").
    //    The leftmost candidate wins (longest tail), but loader words like
    //    "forge"/"neoforge" between name and version are skipped.
    let mut best: Option<String> = None;
    for (idx, c) in stem.char_indices() {
        let is_sep = c == ' ' || c == '-' || c == '_' || c == '+';
        if !is_sep {
            continue;
        }
        let after = idx + c.len_utf8();
        let tail = &stem[after..];
        let tb = tail.as_bytes();
        if tb.is_empty() {
            continue;
        }
        let first = tb[0];
        let version_start = first.is_ascii_digit() || matches!(first, b'v' | b'V' | b'r' | b'R');
        if !version_start {
            continue;
        }
        let lower = tail.to_lowercase();
        if lower.starts_with("forge")
            || lower.starts_with("neoforge")
            || lower.starts_with("fabric")
            || lower.starts_with("universal")
        {
            continue;
        }
        if tail.contains('[') || tail.contains(']') || tail.contains('(') || tail.contains(')') {
            continue;
        }
        let has_digit = tail.chars().any(|c| c.is_ascii_digit());
        if !has_digit || (!tail.contains('.') && !tail.contains('+') && tail.len() > 6) {
            continue;
        }
        // Tail must not end on a long word ("...-noStone" → skip)
        let tail_end = tail.len();
        let mut letters = 0;
        let mut k = tail_end;
        while k > 0 && tb[k - 1].is_ascii_alphabetic() {
            letters += 1;
            k -= 1;
        }
        if letters > 4 {
            continue;
        }
        if best.is_none() {
            best = Some(tail.to_string());
        }
    }
    if let Some(v) = best {
        return v;
    }

    // 3) Glued digits at the very end: "CataclysmCompat1.0" → "1.0"
    let mut s = n;
    let mut d = 0;
    let mut gseen = false;
    while s > 0 {
        let c = bytes[s - 1];
        if c.is_ascii_digit() {
            gseen = true;
            s -= 1;
        } else if c == b'.' && gseen && d < 3 {
            d += 1;
            s -= 1;
        } else {
            break;
        }
    }
    if gseen && s == 0 {
        let v = String::from_utf8_lossy(&bytes[s..n]).into_owned();
        if !v.is_empty() && v.len() <= 12 {
            return v;
        }
    }
    if gseen
        && s > 0
        && (bytes[s - 1].is_ascii_alphabetic() || bytes[s - 1] == b'_' || bytes[s - 1] == b'-')
    {
        let v = String::from_utf8_lossy(&bytes[s..n]).into_owned();
        if !v.is_empty() && v.len() <= 12 {
            return v;
        }
    }

    String::new()
}

/// Map a pack.mcmeta `pack_format` to the Minecraft version range it targets,
/// so local resource packs without any metadata can still show a version.
pub(crate) fn pack_format_to_mc_version(pack_path: &std::path::Path) -> String {
    pack_format_to_mc_version_inner(read_pack_format(pack_path))
}

fn pack_format_to_mc_version_inner(pack_format: Option<u64>) -> String {
    let range = match pack_format {
        Some(1..=3) => "MC 1.6-1.12",
        Some(4) => "MC 1.13",
        Some(5) => "MC 1.14",
        Some(6) => "MC 1.15",
        Some(7) => "MC 1.16",
        Some(8) => "MC 1.17-1.18",
        Some(9) => "MC 1.18",
        Some(10) => "MC 1.19-1.19.2",
        Some(12) => "MC 1.19.3",
        Some(13) => "MC 1.19.4-1.20",
        Some(15) => "MC 1.20.1",
        Some(16) => "MC 1.20.2",
        Some(17) => "MC 1.20.3-1.20.4",
        Some(18) => "MC 1.21-1.21.1",
        Some(19) => "MC 1.21.2-1.21.3",
        Some(22) => "MC 1.21.4",
        Some(32) => "MC 1.21.5",
        Some(34) => "MC 1.21.6",
        Some(42) => "MC 1.21.8",
        _ => return String::new(),
    };
    range.to_string()
}

/// Read the `pack.pack_format` integer from a resource pack (zip or folder).
pub(crate) fn read_pack_format(pack_path: &std::path::Path) -> Option<u64> {
    let read_json = |contents: &str| -> Option<u64> {
        let v: serde_json::Value = serde_json::from_str(contents).ok()?;
        v["pack"]["pack_format"].as_u64()
    };
    if pack_path.is_dir() {
        let f = pack_path.join("pack.mcmeta");
        std::fs::read_to_string(f).ok().and_then(|c| read_json(&c))
    } else {
        let file = std::fs::File::open(pack_path).ok()?;
        let mut archive = zip::ZipArchive::new(file).ok()?;
        let mut f = archive.by_name("pack.mcmeta").ok()?;
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut f, &mut contents).ok()?;
        read_json(&contents)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PackEntry {
    pub filename: String,
    pub name: String,
    pub is_dir: bool,
    pub file_size: u64,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub project_id: String,
}

fn read_pack_icon_from_dir(path: &std::path::Path) -> Option<String> {
    // Check root pack.png first
    let icon_path = path.join("pack.png");
    if icon_path.exists() {
        return read_image_as_base64(&icon_path);
    }

    // Recurse ONLY for files literally named pack.png in subfolders.
    // Never fall back to arbitrary images: shader-pack folders are full of
    // internal GLSL textures (noise/dither/LUT maps) that would surface as
    // garbled "static" icons.
    for entry in std::fs::read_dir(path).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(img) = read_pack_icon_from_dir(&path) {
                return Some(img);
            }
        }
    }
    None
}

fn read_pack_icon_from_zip(path: &std::path::Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader).ok()?;
    let total = archive.len();

    // Build a list of entry names first (avoids borrow conflicts)
    let entries: Vec<(String, bool)> = (0..total)
        .filter_map(|i| {
            let e = archive.by_index(i).ok()?;
            let is_dir = e.is_dir();
            let name = e.name().to_string();
            Some((name, is_dir))
        })
        .collect();

    let root_preferred = [
        "pack.png",
        "pack.jpg",
        "pack.jpeg",
        "preview.png",
        "thumb.png",
        "icon.png",
        "logo.png",
    ];

    // Pass 1: root-level preferred names
    for (name, is_dir) in &entries {
        if *is_dir {
            continue;
        }
        let lower = name.to_lowercase();
        if root_preferred.iter().any(|p| lower == *p) {
            if let Some(img) = try_read_zip_image(&mut archive, name) {
                return Some(img);
            }
        }
    }

    // Pass 2: a literal pack.png inside a subfolder (e.g. "<sub>/pack.png").
    // NO arbitrary-image fallback: shader archives are packed with internal
    // GLSL textures (noise, dithering, LUTs) that render as garbled icons.
    for (name, is_dir) in &entries {
        if *is_dir {
            continue;
        }
        if name.to_lowercase().ends_with("/pack.png") {
            if let Some(img) = try_read_zip_image(&mut archive, name) {
                return Some(img);
            }
        }
    }

    None
}

fn try_read_zip_image<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut buf = Vec::new();
    use std::io::Read;
    file.read_to_end(&mut buf).ok()?;
    if buf.is_empty() {
        return None;
    }
    let lower = name.to_lowercase();
    let mime = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "image/png"
    };
    Some(format!("data:{};base64,{}", mime, base64_encode(&buf)))
}

fn extract_pack_name_from_json(json: &serde_json::Value) -> Option<String> {
    // Prefer pack.name (human-readable) over pack.description (often an internal identifier).
    let raw = extract_text_field(&json["pack"]["name"])
        .or_else(|| extract_text_field(&json["pack"]["description"]))?;

    let stripped = strip_minecraft_color_codes(&raw);
    // Strip common HTML tags used in descriptions
    let clean = stripped
        .replace("<br>", " ")
        .replace("<br/>", " ")
        .replace("</br>", " ");
    let clean = clean.trim().to_string();
    if clean.is_empty() {
        None
    } else {
        Some(clean)
    }
}

/// Extract text from a Minecraft JSON text component (string, object with "text", or array).
fn extract_text_field(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        Some(s.to_string())
    } else if let Some(obj) = value.as_object() {
        obj.get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else if let Some(arr) = value.as_array() {
        let mut result = String::new();
        for item in arr {
            if let Some(obj) = item.as_object() {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    result.push_str(text);
                }
            } else if let Some(s) = item.as_str() {
                result.push_str(s);
            }
        }
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    } else {
        None
    }
}

fn read_pack_name_from_dir(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path.join("pack.mcmeta")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    extract_pack_name_from_json(&json)
}

fn read_pack_name_from_zip(path: &std::path::Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mcmeta = archive.by_name("pack.mcmeta").ok()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::BufReader::new(mcmeta), &mut bytes).ok()?;
    let content = String::from_utf8(bytes).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    extract_pack_name_from_json(&json)
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = std::fs::metadata(&p) {
                total += m.len();
            }
        }
    }
    total
}

fn read_image_as_base64(path: &std::path::Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    if buf.is_empty() {
        return None;
    }
    Some(format!("data:image/png;base64,{}", base64_encode(&buf)))
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

/// Minimal base64 decode (decodes standard base64 with padding)
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const DECODE: [i8; 256] = {
        let mut table = [-1i8; 256];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < 64 {
            table[chars[i] as usize] = i as i8;
            i += 1;
        }
        table[b'=' as usize] = 0;
        table
    };

    let clean: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'\r' && b != b'\n' && b != b' ')
        .collect();
    if clean.is_empty() || !clean.len().is_multiple_of(4) {
        return None;
    }

    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for chunk in clean.chunks(4) {
        if chunk.len() != 4 {
            return None;
        }
        let mut vals = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            let v = DECODE.get(b as usize)?;
            if *v == -1 {
                return None;
            }
            vals[i] = *v as u8;
        }
        out.push((vals[0] << 2) | (vals[1] >> 4));
        if chunk[2] != b'=' {
            out.push((vals[1] << 4) | (vals[2] >> 2));
        }
        if chunk[3] != b'=' {
            out.push((vals[2] << 6) | vals[3]);
        }
    }
    Some(out)
}

/// Update instance's last played timestamp
pub fn update_last_played(instances_dir: &Path, name: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.last_played = Some(Utc::now().to_rfc3339());
    save_instance(instances_dir, &instance, None)?;
    Ok(())
}

// ── Prism compatibility helpers ─────────────────────────────────

/// Write a Prism/MultiMC-compatible instance.cfg from our Instance
fn write_prism_cfg(instance: &Instance, instances_dir: &Path) -> Result<()> {
    let cfg_path = instance.prism_cfg_file(instances_dir);
    std::fs::write(&cfg_path, build_prism_cfg_content(instance))?;
    Ok(())
}

/// Build a Prism/MultiMC-compatible `mmc-pack.json` string from our Instance.
/// This is what Prism (and MultiMC-derived launchers) read to determine the
/// Minecraft version and mod loader of an instance.
fn build_mmc_pack_json(instance: &Instance) -> Result<String> {
    let mut components = Vec::new();
    components.push(serde_json::json!({
        "uid": "net.minecraft",
        "version": instance.mc_version,
        "important": true,
    }));
    match instance.loader {
        LoaderType::Vanilla => {}
        LoaderType::Fabric => {
            components.push(serde_json::json!({
                "uid": "net.fabricmc.fabric-loader",
                "version": instance.loader_version.clone().unwrap_or_default(),
            }));
        }
        LoaderType::Forge => {
            components.push(serde_json::json!({
                "uid": "net.minecraftforge",
                "version": instance.loader_version.clone().unwrap_or_default(),
            }));
        }
        LoaderType::NeoForge => {
            components.push(serde_json::json!({
                "uid": "net.neoforged",
                "version": instance.loader_version.clone().unwrap_or_default(),
            }));
        }
    }
    let pack = serde_json::json!({
        "components": components,
        "formatVersion": 1,
    });
    Ok(serde_json::to_string_pretty(&pack)?)
}

/// Write Prism-compatible `mmc-pack.json` into the instance directory so the
/// folder is browsable/importable by Prism-derived launchers at any time.
fn write_mmc_pack_json(instance: &Instance, instances_dir: &Path) -> Result<()> {
    let path = instance.dir(instances_dir).join("mmc-pack.json");
    std::fs::write(&path, build_mmc_pack_json(instance)?)?;
    Ok(())
}

/// Write instance icon as pack.png in the instance root directory
pub fn write_pack_png(instances_dir: &Path, instance_name: &str, icon_data: &str) {
    let instance = match get_instance(instances_dir, instance_name) {
        Ok(i) => i,
        Err(_) => return,
    };
    let png_path = instance.dir(instances_dir).join("pack.png");

    // Decode base64 data URL (data:image/png;base64,...)
    if let Some(b64) = icon_data.split(";base64,").nth(1) {
        if let Some(bytes) = base64_decode(b64) {
            let _ = std::fs::write(&png_path, &bytes);
        }
    }
}

/// Try to parse a Prism instance.cfg into our Instance format
pub fn parse_prism_cfg(cfg_path: &std::path::Path) -> Option<Instance> {
    let content = std::fs::read_to_string(cfg_path).ok()?;
    let dir = cfg_path.parent()?;
    let dir_name = dir.file_name()?.to_string_lossy().to_string();

    let mut name = dir_name.clone();
    let mut icon_data = None;

    // Read pack.png
    let png_path = dir.join("pack.png");
    if png_path.exists() {
        icon_data = read_image_as_base64(&png_path);
    }

    // Parse INI lines
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') || line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim();
            let val = line[eq + 1..].trim();
            if key == "name" {
                name = val.to_string();
            }
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    Some(Instance {
        name,
        mc_version: String::new(),
        loader: LoaderType::Vanilla,
        loader_version: None,
        loader_profile: None,
        memory_mb: None,
        jvm_args: None,
        gc_preset: None,
        java_path: None,
        resolution: None,
        icon: icon_data,
        banner: None,
        created_at: now.clone(),
        last_played: None,
        play_time_seconds: 0,
        notes: String::new(),
    })
}

/// Parse Prism/MultiMC `mmc-pack.json` (components) into our metadata.
/// Returns (mc_version, loader, loader_version). Quilt packs are rejected —
/// Quilt is not supported by VoidLauncher.
fn parse_prism_mmc_pack(content: &str) -> Result<(String, LoaderType, Option<String>)> {
    let mut mc_version = String::new();
    let mut loader = LoaderType::Vanilla;
    let mut loader_version: Option<String> = None;

    let Ok(pack) = serde_json::from_str::<serde_json::Value>(content) else {
        return Ok((mc_version, loader, loader_version));
    };
    let Some(components) = pack["components"].as_array() else {
        return Ok((mc_version, loader, loader_version));
    };

    for comp in components {
        let uid = comp["uid"].as_str().unwrap_or("");
        let ver = comp["version"].as_str().unwrap_or("");
        if comp["dependencyOnly"].as_bool().unwrap_or(false) {
            continue;
        }
        match uid {
            "net.minecraft" | "org.multimc.minecraft" | "org.prismlauncher.minecraft"
                if !ver.is_empty() =>
            {
                mc_version = ver.to_string();
            }
            "net.fabricmc.fabric-loader" => {
                loader = LoaderType::Fabric;
                loader_version = Some(ver.to_string());
            }
            "org.quiltmc.quilt-loader" => {
                return Err(LauncherError::Instance(
                    "This instance requires Quilt, which is not supported by VoidLauncher."
                        .to_string(),
                ));
            }
            "net.minecraftforge" => {
                loader = LoaderType::Forge;
                loader_version = Some(ver.to_string());
            }
            "net.neoforged" => {
                loader = LoaderType::NeoForge;
                loader_version = Some(ver.to_string());
            }
            _ => {}
        }
    }

    Ok((mc_version, loader, loader_version))
}

/// Import a Prism Launcher instance.zip into VoidLauncher instances dir.
///
/// Prism/MultiMC store the game directory as `minecraft/` (no dot) and keep
/// metadata in `instance.cfg` + `mmc-pack.json`. We map the game directory to
/// our canonical `.minecraft/` and translate the loader components, so mods,
/// resourcepacks, shaderpacks, configs and saves land where our launcher
/// expects them. Per-launch junk (.bobby cache, .mixin.out, crash-reports,
/// logs) and Prism metadata files are not carried over.
pub fn import_prism_pack(instances_dir: &Path, zip_path: &str) -> Result<Instance> {
    let zip_path = std::path::Path::new(zip_path);
    if !zip_path.exists() {
        return Err(LauncherError::Instance(format!(
            "ZIP file not found: {}",
            zip_path.display()
        )));
    }

    // Read the ZIP in one pass: instance.cfg (name) + mmc-pack.json (version/loader)
    let zip_bytes = std::fs::read(zip_path)?;
    let (name, mc_version, loader, loader_version) = {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes))
            .map_err(|e| LauncherError::Instance(format!("Invalid ZIP: {}", e)))?;

        let mut found = false;
        let mut cfg_content = String::new();
        let mut pack_content = String::new();

        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            match entry.name() {
                "instance.cfg" => {
                    found = true;
                    entry.read_to_string(&mut cfg_content).ok();
                }
                "mmc-pack.json" => {
                    entry.read_to_string(&mut pack_content).ok();
                }
                _ => {}
            }
        }

        if !found {
            return Err(LauncherError::Instance(
                "Not a valid Prism instance pack: missing instance.cfg".to_string(),
            ));
        }

        let name = cfg_content
            .lines()
            .find_map(|line| {
                let line = line.trim();
                line.strip_prefix("name=").map(|s| s.to_string())
            })
            .unwrap_or_else(|| {
                zip_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("imported")
                    .to_string()
            });
        let (mc_version, loader, loader_version) = parse_prism_mmc_pack(&pack_content)?;
        (name, mc_version, loader, loader_version)
    };

    // Validate the extracted name
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() < 3
        || trimmed.chars().count() > 64
        || trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed.contains('<')
        || trimmed.contains('>')
        || trimmed.contains(':')
        || trimmed.contains('"')
        || trimmed.contains('|')
        || trimmed.contains('?')
        || trimmed.contains('*')
        || trimmed.chars().any(|c| c.is_control())
        || trimmed.starts_with(' ')
        || trimmed.starts_with('.')
    {
        return Err(LauncherError::Instance(format!(
            "Invalid instance name in Prism pack: '{}'",
            name
        )));
    }
    let windows_reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if windows_reserved
        .iter()
        .any(|r| r.eq_ignore_ascii_case(trimmed))
    {
        return Err(LauncherError::Instance(format!(
            "Invalid instance name in Prism pack: '{}'",
            name
        )));
    }

    // Create target directory
    let target_dir = instances_dir.join(&name);
    std::fs::create_dir_all(&target_dir)?;

    // Extract with mapping: Prism `minecraft/` (and our own `.minecraft/`
    // exports) → canonical `.minecraft/`. Skip Prism metadata and launch junk.
    {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
            .map_err(|e| LauncherError::Instance(format!("Invalid ZIP: {}", e)))?;

        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            if entry.is_dir() {
                continue;
            }

            let entry_name = entry.name().to_string();
            // Refuse path traversal: reject any component equal to ".."
            let normalised = entry_name.replace('\\', "/");
            if normalised.contains('\0') || normalised.split('/').any(|c| c == "..") {
                continue;
            }
            if normalised.starts_with('/') {
                continue;
            }

            // Skip per-launch junk we don't need
            fn is_junk(rel: &str) -> bool {
                rel.starts_with(".bobby/")
                    || rel.starts_with(".mixin.out/")
                    || rel.starts_with("crash-reports/")
                    || rel.starts_with("logs/")
            }

            let relative: Option<String> = if let Some(rel) = normalised.strip_prefix("minecraft/")
            {
                if is_junk(rel) {
                    None
                } else {
                    Some(format!(".minecraft/{}", rel))
                }
            } else if let Some(rel) = normalised.strip_prefix(".minecraft/") {
                if is_junk(rel) {
                    None
                } else {
                    Some(format!(".minecraft/{}", rel))
                }
            } else {
                match normalised.as_str() {
                    "instance.cfg" | "instance.json" | "mmc-pack.json" | ".packignore" => None,
                    "icon.png" | "pack.png" => Some(normalised),
                    _ if normalised.starts_with("patches/") => None,
                    _ => None,
                }
            };

            let Some(relative) = relative else { continue };

            let out_path = target_dir.join(&relative);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out_file)?;
        }
    }

    // Root icon → instance icon
    let mut icon = None;
    for ic in ["icon.png", "pack.png"] {
        let p = target_dir.join(ic);
        if p.exists() {
            icon = read_image_as_base64(&p);
            let _ = std::fs::remove_file(&p);
            if icon.is_some() {
                break;
            }
        }
    }

    // Parse metadata and save as instance.json (+ Prism-compatible instance.cfg)
    let now = chrono::Utc::now().to_rfc3339();
    let instance = Instance {
        name,
        mc_version,
        loader,
        loader_version,
        loader_profile: None,
        memory_mb: None,
        jvm_args: None,
        gc_preset: None,
        java_path: None,
        resolution: None,
        icon,
        banner: None,
        created_at: now.clone(),
        last_played: None,
        play_time_seconds: 0,
        notes: String::new(),
    };
    save_instance(instances_dir, &instance, None)?;

    tracing::info!(
        target: "launcher",
        "Imported Prism pack '{}' (MC {}, loader {:?} {:?}) from {:?}",
        instance.name,
        instance.mc_version,
        instance.loader,
        instance.loader_version,
        zip_path
    );
    Ok(instance)
}

/// Export an instance as a Prism/MultiMC-compatible .zip archive.
///
/// The archive layout matches what Prism Launcher produces and expects:
/// ```text
/// MyInstance/
/// ├── instance.cfg
/// ├── mmc-pack.json
/// ├── pack.png          (when the instance has an icon)
/// └── minecraft/        (game dir, no leading dot)
///     ├── mods/
///     ├── config/
///     └── ...
/// ```
/// VoidLauncher-internal files (`instance.json`) and per-launch junk
/// (logs, crash-reports, caches) are excluded. Files are streamed into the
/// archive, so large saves do not load into memory.
pub fn export_instance(instances_dir: &Path, name: &str, output_path: &str) -> Result<()> {
    use zip::write::SimpleFileOptions;

    let instance = get_instance(instances_dir, name)?;
    let instance_dir = instance.dir(instances_dir);
    if !instance_dir.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }

    let out_path = std::path::Path::new(output_path);
    let file = std::fs::File::create(out_path)?;
    let mut zip_w = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    // Prism metadata files, generated fresh from our Instance so the archive
    // is always current even if on-disk copies are stale.
    zip_w.start_file("instance.cfg", options)?;
    let cfg_lines = build_prism_cfg_content(&instance);
    std::io::Write::write_all(&mut zip_w, cfg_lines.as_bytes())?;

    zip_w.start_file("mmc-pack.json", options)?;
    let mmc_pack = build_mmc_pack_json(&instance)?;
    std::io::Write::write_all(&mut zip_w, mmc_pack.as_bytes())?;

    // pack.png — prefer the in-memory icon, fall back to the on-disk copy
    // written by save_instance.
    let icon_bytes: Option<Vec<u8>> = instance
        .icon
        .as_ref()
        .and_then(|icon| icon.split(";base64,").nth(1))
        .and_then(base64_decode)
        .or_else(|| std::fs::read(instance_dir.join("pack.png")).ok());
    if let Some(bytes) = icon_bytes {
        zip_w.start_file("pack.png", options)?;
        std::io::Write::write_all(&mut zip_w, &bytes)?;
    }

    // Game directory: our `.minecraft/` → Prism `minecraft/`.
    let mc_dir = instance.minecraft_dir(instances_dir);
    if mc_dir.exists() {
        add_minecraft_dir_to_zip(&mut zip_w, &mc_dir, "minecraft", options)?;
    }

    zip_w.finish()?;

    tracing::info!(target: "launcher", "Exported instance '{}' to {:?}", name, out_path);
    Ok(())
}

/// Render the `[General]`/`[MultiMC]` body of a Prism instance.cfg as a
/// string (shared by on-disk save and zip export).
fn build_prism_cfg_content(instance: &Instance) -> String {
    let last_launch = instance
        .last_played
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
        .unwrap_or(0);

    let mut lines = Vec::new();
    lines.push("[General]".to_string());
    lines.push("ConfigVersion=1.3".to_string());
    lines.push("InstanceType=OneSix".to_string());
    lines.push(format!("name={}", instance.name));
    lines.push(format!("iconKey={}", instance.name));
    lines.push(format!("notes={}", instance.notes.replace('\n', "\\n")));
    lines.push(format!("lastLaunchTime={}", last_launch));
    lines.push(format!("totalTimePlayed={}", instance.play_time_seconds));
    lines.push(String::new());
    lines.push("[MultiMC]".to_string());
    lines.push("autoCloseMinecraft=false".to_string());
    lines.join("\n")
}

/// Stream a directory tree into the zip under `prefix/`, skipping launch
/// junk (logs, crash-reports, caches) and `.voidlauncher-old` stash files
/// left by transactional content updates. Files are copied in chunks, not
/// buffered whole.
fn add_minecraft_dir_to_zip(
    zip: &mut zip::ZipWriter<std::fs::File>,
    dir: &std::path::Path,
    prefix: &str,
    options: zip::write::SimpleFileOptions,
) -> Result<()> {
    fn is_junk(name: &str) -> bool {
        name == "logs"
            || name == "crash-reports"
            || name == ".bobby"
            || name == ".mixin.out"
            || name.ends_with(".voidlauncher-old")
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if is_junk(&name) {
            continue;
        }
        let zip_path = format!("{}/{}", prefix, name);
        if path.is_dir() {
            zip.add_directory(&zip_path, options)?;
            add_minecraft_dir_to_zip(zip, &path, &zip_path, options)?;
        } else {
            zip.start_file(&zip_path, options)?;
            let mut file = std::fs::File::open(&path)?;
            std::io::copy(&mut file, zip)?;
        }
    }
    Ok(())
}

/// Zip the contents of `dir` under `prefix` (no leading slash). Root files sit
/// at the archive root; subdirectories accumulate their path on recursion.
/// Entries whose file name satisfies `skip` (and their whole subtree) are left
/// out — backups use it to drop logs, screenshots, caches and themselves.
fn add_dir_contents_to_zip(
    zip: &mut zip::ZipWriter<std::fs::File>,
    dir: &std::path::Path,
    prefix: &str,
    options: zip::write::SimpleFileOptions,
    skip: &dyn Fn(&str) -> bool,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if skip(&name) {
            continue;
        }
        let zip_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", prefix, name)
        };
        if path.is_dir() {
            zip.add_directory(&zip_path, options)?;
            add_dir_contents_to_zip(zip, &path, &zip_path, options, skip)?;
        } else {
            zip.start_file(&zip_path, options)?;
            let mut file = std::fs::File::open(&path)?;
            std::io::copy(&mut file, zip)?;
        }
    }
    Ok(())
}

// ── World operations ──────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct LevelDatData {
    pub game_type: Option<i32>,
    pub seed: Option<i64>,
    pub level_name: Option<String>,
    pub last_played: Option<i64>,
}

fn nbt_read_string(r: &mut impl Read) -> Result<String> {
    let mut len_buf = [0u8; 2];
    r.read_exact(&mut len_buf)
        .map_err(|e| LauncherError::Instance(e.to_string()))?;
    let len = u16::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)
        .map_err(|e| LauncherError::Instance(e.to_string()))?;
    String::from_utf8(buf)
        .map_err(|e| LauncherError::Instance(format!("Invalid UTF-8 in NBT: {e}")))
}

fn nbt_skip(r: &mut impl Read, tag: u8) -> Result<()> {
    match tag {
        0 => Ok(()),
        1 => {
            let mut b = [0u8; 1];
            r.read_exact(&mut b)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            Ok(())
        }
        2 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            Ok(())
        }
        3 => {
            let mut b = [0u8; 4];
            r.read_exact(&mut b)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            Ok(())
        }
        4 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            Ok(())
        }
        5 => {
            let mut b = [0u8; 4];
            r.read_exact(&mut b)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            Ok(())
        }
        6 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            Ok(())
        }
        7 | 11 | 12 => {
            let mut len_buf = [0u8; 4];
            r.read_exact(&mut len_buf)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            let elem_size: usize = match tag {
                7 => 1,
                11 => 4,
                12 => 8,
                _ => 1,
            };
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut skip = vec![0u8; len * elem_size];
            r.read_exact(&mut skip)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            Ok(())
        }
        8 => {
            nbt_read_string(r)?;
            Ok(())
        }
        9 => {
            let mut elem_type = [0u8; 1];
            r.read_exact(&mut elem_type)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            let mut len_buf = [0u8; 4];
            r.read_exact(&mut len_buf)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            let len = u32::from_be_bytes(len_buf) as usize;
            for _ in 0..len {
                nbt_skip(r, elem_type[0])?;
            }
            Ok(())
        }
        10 => {
            loop {
                let mut t = [0u8; 1];
                r.read_exact(&mut t)
                    .map_err(|e| LauncherError::Instance(e.to_string()))?;
                if t[0] == 0 {
                    break;
                }
                nbt_read_string(r)?;
                nbt_skip(r, t[0])?;
            }
            Ok(())
        }
        _ => Err(LauncherError::Instance(format!(
            "Unknown NBT tag type: {tag}"
        ))),
    }
}

fn nbt_parse_compound_fields(r: &mut impl Read, data: &mut LevelDatData) -> Result<()> {
    loop {
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if tag[0] == 0 {
            break;
        }
        let name = nbt_read_string(r)?;
        match tag[0] {
            3 => {
                let mut buf = [0u8; 4];
                r.read_exact(&mut buf)
                    .map_err(|e| LauncherError::Instance(e.to_string()))?;
                let val = i32::from_be_bytes(buf);
                if name == "GameType" {
                    data.game_type = Some(val);
                }
            }
            4 => {
                let mut buf = [0u8; 8];
                r.read_exact(&mut buf)
                    .map_err(|e| LauncherError::Instance(e.to_string()))?;
                let val = i64::from_be_bytes(buf);
                match name.as_str() {
                    "Seed" => data.seed = Some(val),
                    "LastPlayed" => data.last_played = Some(val),
                    _ => {}
                }
            }
            8 => {
                let val = nbt_read_string(r)?;
                if name == "LevelName" {
                    data.level_name = Some(val);
                }
            }
            10 => {
                if name == "Data" {
                    nbt_parse_compound_fields(r, data)?;
                } else {
                    nbt_skip(r, 10)?;
                }
            }
            _ => {
                nbt_skip(r, tag[0])?;
            }
        }
    }
    Ok(())
}

/// Parse a GZip-compressed Minecraft level.dat and extract key fields.
pub fn parse_level_dat(path: &std::path::Path) -> LevelDatData {
    let Ok(file) = std::fs::File::open(path) else {
        return LevelDatData::default();
    };
    let mut decoder = GzDecoder::new(file);
    let mut tag = [0u8; 1];
    if decoder.read_exact(&mut tag).is_err() || tag[0] != 10 {
        return LevelDatData::default();
    }
    let _ = nbt_read_string(&mut decoder);
    let mut data = LevelDatData::default();
    let _ = nbt_parse_compound_fields(&mut decoder, &mut data);
    data
}

pub fn rename_world(
    instances_dir: &Path,
    instance_name: &str,
    old_name: &str,
    new_name: &str,
) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let from = saves_dir.join(old_name);
    let to = saves_dir.join(new_name);
    if !from.exists() {
        return Err(LauncherError::Instance(format!(
            "World '{}' not found",
            old_name
        )));
    }
    if to.exists() {
        return Err(LauncherError::Instance(format!(
            "A world named '{}' already exists",
            new_name
        )));
    }
    std::fs::rename(&from, &to).map_err(|e| LauncherError::Instance(e.to_string()))
}

pub fn copy_world(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
    new_name: &str,
) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let src = saves_dir.join(world_name);
    let dst = saves_dir.join(new_name);
    if !src.exists() {
        return Err(LauncherError::Instance(format!(
            "World '{}' not found",
            world_name
        )));
    }
    if dst.exists() {
        return Err(LauncherError::Instance(format!(
            "A world named '{}' already exists",
            new_name
        )));
    }
    copy_dir_recursive(&src, &dst)
}

pub fn delete_world(instances_dir: &Path, instance_name: &str, world_name: &str) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let world_dir = saves_dir.join(world_name);
    if !world_dir.exists() {
        return Err(LauncherError::Instance(format!(
            "World '{}' not found",
            world_name
        )));
    }
    std::fs::remove_dir_all(&world_dir).map_err(|e| LauncherError::Instance(e.to_string()))
}

pub fn read_world_icon(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
) -> Option<String> {
    let instance = get_instance(instances_dir, instance_name).ok()?;
    let icon_path = instance
        .minecraft_dir(instances_dir)
        .join("saves")
        .join(world_name)
        .join("icon.png");
    if !icon_path.exists() {
        return None;
    }
    read_image_as_base64(&icon_path)
}

/// Metadata describing one stored world backup archive.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BackupInfo {
    pub filename: String,
    pub size_bytes: u64,
    pub created_at: i64,
}

/// Where backups of a world's saves are stored: `<instance>/.backups/worlds/<world>/`.
fn world_backups_dir(instances_dir: &Path, instance_name: &str, world_name: &str) -> PathBuf {
    instances_dir
        .join(instance_name)
        .join(".backups")
        .join("worlds")
        .join(world_name)
}

/// Reject any backup filename that is not a plain `.zip` file name inside the
/// backups directory (no traversal, separators, hidden files, or Windows ADS).
fn validate_backup_filename(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 200
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('\0')
        || name.contains("..")
        || name.starts_with('.')
        || !name.to_ascii_lowercase().ends_with(".zip")
    {
        return Err(LauncherError::Instance(format!(
            "Invalid backup filename: '{}'",
            name
        )));
    }
    Ok(())
}

/// Zip a world save into `<instance>/.backups/worlds/<world>/<world>__<ts>.zip`
/// and return the resulting archive's metadata.
pub fn backup_world(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
) -> Result<BackupInfo> {
    use zip::write::SimpleFileOptions;
    let instance = get_instance(instances_dir, instance_name)?;
    let world_dir = instance
        .minecraft_dir(instances_dir)
        .join("saves")
        .join(world_name);
    if !world_dir.is_dir() {
        return Err(LauncherError::Instance(format!(
            "World '{}' not found",
            world_name
        )));
    }
    let backups_dir = world_backups_dir(instances_dir, instance_name, world_name);
    std::fs::create_dir_all(&backups_dir)?;
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut out_path = backups_dir.join(format!("{}__{}.zip", world_name, ts));
    // Two backups in the same second must not clobber each other.
    if out_path.exists() {
        out_path = backups_dir.join(format!("{}__{}_{}.zip", world_name, ts, std::process::id()));
    }
    let filename = out_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let file = std::fs::File::create(&out_path)?;
    let mut zip_w = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    add_dir_contents_to_zip(&mut zip_w, &world_dir, "", options, &|_| false)?;
    zip_w.finish()?;
    let size_bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    tracing::info!(target: "launcher", "Backed up world '{}' of '{}' to {:?}", world_name, instance_name, out_path);
    Ok(BackupInfo {
        filename,
        size_bytes,
        created_at: chrono::Local::now().timestamp(),
    })
}

/// List `*.zip` archives in a backup directory, newest first. A missing
/// directory yields an empty list. Shared by world and instance backups.
fn list_backup_dir(dir: &std::path::Path) -> Result<Vec<BackupInfo>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.to_ascii_lowercase().ends_with(".zip") {
            continue;
        }
        let meta = entry.metadata()?;
        let created_at = meta
            .created()
            .or_else(|_| meta.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(BackupInfo {
            filename: fname,
            size_bytes: meta.len(),
            created_at,
        });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    Ok(out)
}

/// List a world's backups, newest first. Missing directory yields an empty list.
pub fn list_world_backups(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
) -> Result<Vec<BackupInfo>> {
    list_backup_dir(&world_backups_dir(instances_dir, instance_name, world_name))
}

/// Restore a world from a backup archive. `mode` is `"replace"` (overwrite the
/// existing world) or `"as_new"` (extract into a new world named `new_name`).
pub fn restore_world(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
    backup_filename: &str,
    mode: &str,
    new_name: Option<&str>,
) -> Result<()> {
    validate_backup_filename(backup_filename)?;
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let backup_path =
        world_backups_dir(instances_dir, instance_name, world_name).join(backup_filename);
    if !backup_path.is_file() {
        return Err(LauncherError::Instance(format!(
            "Backup '{}' not found",
            backup_filename
        )));
    }

    let target_world = match mode {
        "replace" => world_name.to_string(),
        "as_new" => {
            let n = new_name.unwrap_or("").trim();
            if n.is_empty()
                || n.len() > 64
                || n.contains('/')
                || n.contains('\\')
                || n.contains(':')
                || n.contains('\0')
                || n.contains("..")
            {
                return Err(LauncherError::Instance(format!(
                    "Invalid world name: '{}'",
                    n
                )));
            }
            n.to_string()
        }
        _ => {
            return Err(LauncherError::Instance(format!(
                "Unknown restore mode: '{}'",
                mode
            )))
        }
    };

    let target_dir = saves_dir.join(&target_world);
    // Replace: drop the existing world first (the backup is already on disk).
    if mode == "replace" && target_dir.exists() {
        std::fs::remove_dir_all(&target_dir)?;
    }
    if target_dir.exists() {
        return Err(LauncherError::Instance(format!(
            "A world named '{}' already exists",
            target_world
        )));
    }

    // Extract with zip-slip protection (blocks traversal, separators, and
    // absolute/Windows drive paths via a `starts_with` check on the join).
    extract_zip_to_dir(&backup_path, &target_dir)?;
    tracing::info!(
        target: "launcher",
        "Restored world '{}' of '{}' from '{}' (mode {}) as '{}'",
        world_name, instance_name, backup_filename, mode, target_world
    );
    Ok(())
}

/// Delete one stored world backup archive.
pub fn delete_world_backup(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
    backup_filename: &str,
) -> Result<()> {
    validate_backup_filename(backup_filename)?;
    let path = world_backups_dir(instances_dir, instance_name, world_name).join(backup_filename);
    if !path.is_file() {
        return Err(LauncherError::Instance(format!(
            "Backup '{}' not found",
            backup_filename
        )));
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

// ── Instance backup / restore ────────────────────────────────────

/// Where per-instance backup archives live: `<instance>/.backups/instances/`.
/// Inside the instance so backups travel with it, but never zipped into a
/// backup of that same instance (see `instance_backup_skip`).
fn instance_backups_dir(instances_dir: &Path, instance_name: &str) -> PathBuf {
    instances_dir
        .join(instance_name)
        .join(".backups")
        .join("instances")
}

/// What a whole-instance backup leaves out: per-launch logs, crash reports,
/// screenshots and the backups themselves (a backup must not contain itself).
/// Keeps archives small and avoids recursion into `.backups`.
fn instance_backup_skip(name: &str) -> bool {
    name == "logs"
        || name == "crash-reports"
        || name == "screenshots"
        || name == ".backups"
        || name.ends_with(".voidlauncher-old")
}

/// Extract a zip archive into `dest` with zip-slip protection: entries with
/// NUL, `:` (drive letters / NTFS streams), leading `/` or `..` segments are
/// skipped, and the final join must stay inside `dest`.
fn extract_zip_to_dir(zip_path: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| LauncherError::Instance(format!("Invalid backup archive: {}", e)))?;
    std::fs::create_dir_all(dest)?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        if name.is_empty()
            || name.contains('\0')
            || name.contains(':')
            || name.starts_with('/')
            || name.split('/').any(|c| c == "..")
        {
            continue;
        }
        let out_path = dest.join(&name);
        if !out_path.starts_with(dest) {
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

/// Zip an entire instance (metadata + `.minecraft`) into
/// `<instance>/.backups/instances/<name>__<ts>.zip`. Logs, crash reports,
/// screenshots and the `.backups` directory are excluded.
pub fn backup_instance(instances_dir: &Path, name: &str) -> Result<BackupInfo> {
    use zip::write::SimpleFileOptions;
    let instance = get_instance(instances_dir, name)?;
    let inst_dir = instance.dir(instances_dir);
    if !inst_dir.is_dir() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    let backups_dir = instance_backups_dir(instances_dir, name);
    std::fs::create_dir_all(&backups_dir)?;
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut out_path = backups_dir.join(format!("{}__{}.zip", name, ts));
    // Two backups in the same second must not clobber each other.
    if out_path.exists() {
        out_path = backups_dir.join(format!("{}__{}_{}.zip", name, ts, std::process::id()));
    }
    let filename = out_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let file = std::fs::File::create(&out_path)?;
    let mut zip_w = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    add_dir_contents_to_zip(&mut zip_w, &inst_dir, "", options, &instance_backup_skip)?;
    zip_w.finish()?;
    let size_bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    tracing::info!(target: "launcher", "Backed up instance '{}' to {:?}", name, out_path);
    Ok(BackupInfo {
        filename,
        size_bytes,
        created_at: chrono::Local::now().timestamp(),
    })
}

/// List an instance's backups, newest first. Missing directory yields empty.
pub fn list_instance_backups(instances_dir: &Path, name: &str) -> Result<Vec<BackupInfo>> {
    list_backup_dir(&instance_backups_dir(instances_dir, name))
}

/// Restore an instance from a backup archive. `mode` is `"replace"` (rebuild
/// the instance from the archive, preserving its `.backups` history) or
/// `"as_new"` (extract into a new instance named `new_name`; the original is
/// untouched). The archive is fully extracted to a temp directory first — only
/// a valid instance archive (has `instance.json` or `instance.cfg`) swaps in,
/// so a corrupt backup never destroys the existing instance.
pub fn restore_instance(
    instances_dir: &Path,
    name: &str,
    backup_filename: &str,
    mode: &str,
    new_name: Option<&str>,
) -> Result<()> {
    validate_backup_filename(backup_filename)?;
    // No `get_instance` here on purpose: restore must work even when the
    // live instance's metadata is what got damaged.
    let inst_dir = instances_dir.join(name);
    let backup_path = instance_backups_dir(instances_dir, name).join(backup_filename);
    if !inst_dir.is_dir() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    if !backup_path.is_file() {
        return Err(LauncherError::Instance(format!(
            "Backup '{}' not found",
            backup_filename
        )));
    }

    let target_name = match mode {
        "replace" => name.to_string(),
        "as_new" => {
            let n = new_name.unwrap_or("").trim();
            if n.is_empty()
                || n.len() > 64
                || n.contains('/')
                || n.contains('\\')
                || n.contains(':')
                || n.contains('\0')
                || n.contains("..")
            {
                return Err(LauncherError::Instance(format!(
                    "Invalid instance name: '{}'",
                    n
                )));
            }
            n.to_string()
        }
        _ => {
            return Err(LauncherError::Instance(format!(
                "Unknown restore mode: '{}'",
                mode
            )))
        }
    };
    let target_dir = instances_dir.join(&target_name);
    if mode == "as_new" && target_dir.exists() {
        return Err(LauncherError::Instance(format!(
            "An instance named '{}' already exists",
            target_name
        )));
    }

    // 1. Extract fully to a hidden sibling temp dir — failures here leave the
    //    existing instance untouched. Dot-prefixed and lacking instance.json,
    //    so `list_instances` ignores it even if a crash leaves it behind.
    let pid = std::process::id();
    let ts = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let tmp = instances_dir.join(format!(".void-restore-tmp-{}_{}", pid, ts));
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    if let Err(e) = extract_zip_to_dir(&backup_path, &tmp) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if !tmp.join("instance.json").exists() && !tmp.join("instance.cfg").exists() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(LauncherError::Instance(
            "Archive does not look like a VoidLauncher or Prism instance".into(),
        ));
    }

    // 2. Swap it in.
    if mode == "replace" {
        // The archive never contains `.backups`, so preserve the live backup
        // history across the swap — it holds the archive being restored.
        let old_backups = inst_dir.join(".backups");
        let preserved = instances_dir.join(format!(".void-backups-preserve-{}_{}", pid, ts));
        let had_backups = old_backups.exists();
        if had_backups {
            std::fs::rename(&old_backups, &preserved)?;
        }
        let old_dir = instances_dir.join(format!(".void-old-{}_{}", pid, ts));
        if let Err(e) = std::fs::rename(&inst_dir, &old_dir) {
            if had_backups {
                let _ = std::fs::rename(&preserved, &old_backups);
            }
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e.into());
        }
        if let Err(e) = std::fs::rename(&tmp, &target_dir) {
            let _ = std::fs::rename(&old_dir, &inst_dir);
            if had_backups {
                let _ = std::fs::rename(&preserved, inst_dir.join(".backups"));
            }
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e.into());
        }
        if had_backups {
            let new_backups = target_dir.join(".backups");
            if new_backups.exists() {
                // Extremely unlikely (a backup containing `.backups`), but
                // keep both histories instead of losing one.
                for entry in std::fs::read_dir(&preserved)? {
                    let entry = entry?;
                    let _ = std::fs::rename(entry.path(), new_backups.join(entry.file_name()));
                }
                let _ = std::fs::remove_dir_all(&preserved);
            } else {
                std::fs::rename(&preserved, &new_backups)?;
            }
        }
        let _ = std::fs::remove_dir_all(&old_dir);
    } else {
        if let Err(e) = std::fs::rename(&tmp, &target_dir) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e.into());
        }
        // Re-label the metadata: the archive stores the original instance
        // name, and `Instance.dir()` derives the folder from that field —
        // keeping it would make the copy address the original's folder.
        if let Ok(mut restored) = get_instance(instances_dir, &target_name) {
            restored.name = target_name.clone();
            let _ = save_instance(instances_dir, &restored, None);
        }
    }

    tracing::info!(
        target: "launcher",
        "Restored instance '{}' from '{}' (mode {}) as '{}'",
        name, backup_filename, mode, target_name
    );
    Ok(())
}

/// Delete one stored instance backup archive.
pub fn delete_instance_backup(
    instances_dir: &Path,
    name: &str,
    backup_filename: &str,
) -> Result<()> {
    validate_backup_filename(backup_filename)?;
    let path = instance_backups_dir(instances_dir, name).join(backup_filename);
    if !path.is_file() {
        return Err(LauncherError::Instance(format!(
            "Backup '{}' not found",
            backup_filename
        )));
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn build_prism_zip(path: &Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let cfg = "[General]\nConfigVersion=1.3\nInstanceType=OneSix\nname=Test Pack\n";
        zip_w.start_file("instance.cfg", opts).unwrap();
        zip_w.write_all(cfg.as_bytes()).unwrap();

        let pack = r#"{
            "components": [
                {"uid": "org.lwjgl3", "version": "3.4.1", "dependencyOnly": true},
                {"uid": "net.minecraft", "version": "1.20.1", "important": true},
                {"uid": "net.fabricmc.intermediary", "version": "1.20.1", "dependencyOnly": true},
                {"uid": "net.fabricmc.fabric-loader", "version": "0.15.11"}
            ],
            "formatVersion": 1
        }"#;
        zip_w.start_file("mmc-pack.json", opts).unwrap();
        zip_w.write_all(pack.as_bytes()).unwrap();

        for (name, bytes) in [
            ("minecraft/mods/testmod.jar", b"PK\x03\x04fakejar".to_vec()),
            (
                "minecraft/resourcepacks/rp.zip",
                b"PK\x03\x04fakerp".to_vec(),
            ),
            ("minecraft/shaderpacks/sh.zip", b"PK\x03\x04fakesh".to_vec()),
            ("minecraft/config/x.yml", b"key: value".to_vec()),
            ("minecraft/.bobby/junk.mca", b"junk".to_vec()),
            ("minecraft/.mixin.out/junk.class", b"junk".to_vec()),
            ("minecraft/crash-reports/crash.txt", b"junk".to_vec()),
            ("patches/forge.json", b"{}".to_vec()),
            ("icon.png", b"\x89PNG\r\n\x1a\nfakepng".to_vec()),
        ] {
            zip_w.start_file(name, opts).unwrap();
            zip_w.write_all(&bytes).unwrap();
        }

        zip_w.finish().unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_prism_import_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn delete_screenshot_rejects_path_traversal() {
        let dir = temp_dir("shots");
        let instances_dir = dir.join("instances");
        let instance = Instance {
            name: "Test".into(),
            mc_version: "1.20.1".into(),
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
            created_at: Utc::now().to_rfc3339(),
            last_played: None,
            play_time_seconds: 0,
            notes: String::new(),
        };
        save_instance(&instances_dir, &instance, None).unwrap();

        let screenshots_dir = instance.minecraft_dir(&instances_dir).join("screenshots");
        std::fs::create_dir_all(&screenshots_dir).unwrap();
        std::fs::write(screenshots_dir.join("shot.png"), b"png").unwrap();
        // Decoy files that must NOT be deletable by a crafted screenshot name.
        let decoy = instance.dir(&instances_dir).join("decoy.txt");
        std::fs::write(&decoy, b"keep").unwrap();
        let cfg_decoy = instance.config_file(&instances_dir);
        let cfg_ok = std::fs::read(&cfg_decoy).unwrap();

        for evil in [
            "../decoy.txt",
            "..\\decoy.txt",
            "C:/Windows/decoy.txt",
            "C:\\Windows\\decoy.txt",
            "/decoy.txt",
            "\\decoy.txt",
            "../instance.json",
            "..\\instance.json",
            "..",
            ".",
            "",
        ] {
            assert!(
                delete_screenshot(&instances_dir, "Test", evil).is_err(),
                "name {:?} must be rejected",
                evil
            );
        }
        assert!(decoy.exists(), "decoy must survive");
        assert_eq!(
            std::fs::read(&cfg_decoy).unwrap(),
            cfg_ok,
            "instance.json must survive"
        );

        delete_screenshot(&instances_dir, "Test", "shot.png").unwrap();
        assert!(!screenshots_dir.join("shot.png").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prism_import_maps_game_dir_and_loader() {
        let dir = temp_dir("map");
        let zip_path = dir.join("pack.zip");
        build_prism_zip(&zip_path);

        let instances_dir = dir.join("instances");
        let instance = import_prism_pack(&instances_dir, zip_path.to_str().unwrap()).unwrap();

        assert_eq!(instance.name, "Test Pack");
        assert_eq!(instance.mc_version, "1.20.1");
        assert_eq!(instance.loader, LoaderType::Fabric);
        assert_eq!(instance.loader_version.as_deref(), Some("0.15.11"));
        assert!(instance.icon.is_some(), "root icon should be imported");

        let mc_dir = instances_dir.join("Test Pack").join(".minecraft");
        assert!(
            mc_dir.join("mods/testmod.jar").exists(),
            "mods mapped to .minecraft/mods"
        );
        assert!(mc_dir.join("resourcepacks/rp.zip").exists());
        assert!(mc_dir.join("shaderpacks/sh.zip").exists());
        assert!(mc_dir.join("config/x.yml").exists());
        assert!(!mc_dir.join(".bobby").exists(), "launch junk skipped");
        assert!(!mc_dir.join(".mixin.out").exists(), "mixin dump skipped");
        assert!(
            !mc_dir.join("crash-reports").exists(),
            "crash reports skipped"
        );
        assert!(
            !instances_dir.join("Test Pack").join("patches").exists(),
            "patches skipped"
        );
        assert!(
            instances_dir
                .join("Test Pack")
                .join("instance.json")
                .exists(),
            "instance.json written"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn packwiz_index_parses_modrinth_curseforge_and_local_meta() {
        let dir = temp_dir("pw");
        let index_dir = dir.join(".index");
        std::fs::create_dir_all(&index_dir).unwrap();

        std::fs::write(
            index_dir.join("emf.pw.toml"),
            r#"filename = "entity_model_features-3.2.6-26.2-fabric.jar"
name = "Entity Model Features"
side = "both"
x-prismlauncher-loaders = ["fabric"]
x-prismlauncher-mc-versions = ["26.2"]
x-prismlauncher-version-number = "3.2.6"
x-prismlauncher-release-type = "release"

[download]
hash = "abc"
hash-format = "sha1"
mode = "metadata:modrinth"
url = "https://cdn.modrinth.com/data/P7dR8mSH/versions/0/emf.jar"

[update.modrinth]
mod-id = "P7dR8mSH"
version = "3.2.6"
"#,
        )
        .unwrap();

        std::fs::write(
            index_dir.join("cf.pw.toml"),
            r#"filename = "sodium-0.6.0.jar"
name = "Sodium"
side = "both"

[download]
hash = "def"
hash-format = "sha512"
mode = "metadata:curseforge"
url = "https://www.curseforge.com/minecraft/mc-mods/sodium/download/5123456"

[update.curseforge]
file-id = 5123456
project-id = 394468
"#,
        )
        .unwrap();

        std::fs::write(
            index_dir.join("local.pw.toml"),
            r#"filename = "mymod.jar"
name = "My Mod"
side = "both"

[download]
hash = "123"
hash-format = "sha1"
mode = "url"
url = "https://example.com/mymod.jar"
"#,
        )
        .unwrap();

        // Prism exports shader packs' metadata in the content dir ROOT (not .index/)
        std::fs::write(
            dir.join("bsl-shaders.pw.toml"),
            r#"filename = 'BSL_v10.1.3.zip'
name = 'BSL Shaders'
side = 'client'
x-prismlauncher-version-number = '10.1.3'
x-prismlauncher-release-type = 'release'

[download]
hash = 'abc'
hash-format = 'sha512'
mode = 'url'
url = 'https://cdn.modrinth.com/data/Q1vvjJYV/versions/hIibTfxn/BSL_v10.1.3.zip'

[update.modrinth]
mod-id = 'Q1vvjJYV'
version = 'hIibTfxn'
"#,
        )
        .unwrap();

        let index = load_packwiz_index(&dir);
        assert_eq!(index.len(), 4);

        let bsl = index.get("BSL_v10.1.3.zip").unwrap();
        assert_eq!(bsl.name, "BSL Shaders");
        assert_eq!(bsl.version, "10.1.3");
        assert_eq!(bsl.provider, "Modrinth");
        assert_eq!(bsl.project_id, "Q1vvjJYV");

        let emf = index
            .get("entity_model_features-3.2.6-26.2-fabric.jar")
            .unwrap();
        assert_eq!(emf.name, "Entity Model Features");
        assert_eq!(emf.version, "3.2.6");
        assert_eq!(emf.provider, "Modrinth");
        assert_eq!(emf.project_id, "P7dR8mSH");

        let cf = index.get("sodium-0.6.0.jar").unwrap();
        assert_eq!(cf.name, "Sodium");
        assert_eq!(cf.provider, "CurseForge");
        assert_eq!(cf.project_id, "394468");

        let local = index.get("mymod.jar").unwrap();
        assert_eq!(local.provider, "");
        assert_eq!(local.project_id, "");
        assert_eq!(local.version, "");

        // Our own .voidlauncher.json sidecars in the same folder are ignored
        std::fs::write(index_dir.join("sodium-0.6.0.voidlauncher.json"), "{}").unwrap();
        let index2 = load_packwiz_index(&dir);
        assert_eq!(index2.len(), 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_import_handles_packs_without_mmc_pack_json() {
        let dir = temp_dir("nocfg");
        let zip_path = dir.join("pack.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip_w.start_file("instance.cfg", opts).unwrap();
        zip_w.write_all(b"[General]\nname=Old Pack\n").unwrap();
        zip_w.start_file("minecraft/mods/old.jar", opts).unwrap();
        zip_w.write_all(b"PK\x03\x04old").unwrap();
        zip_w.finish().unwrap();

        let instances_dir = dir.join("instances");
        let instance = import_prism_pack(&instances_dir, zip_path.to_str().unwrap()).unwrap();

        assert_eq!(instance.name, "Old Pack");
        assert_eq!(instance.mc_version, "");
        assert_eq!(instance.loader, LoaderType::Vanilla);
        assert!(instances_dir
            .join("Old Pack")
            .join(".minecraft/mods/old.jar")
            .exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_instance(name: &str, mc: &str, loader: LoaderType, lv: Option<&str>) -> Instance {
        Instance {
            name: name.into(),
            mc_version: mc.into(),
            loader,
            loader_version: lv.map(String::from),
            loader_profile: None,
            memory_mb: None,
            jvm_args: None,
            gc_preset: None,
            java_path: None,
            resolution: None,
            icon: None,
            banner: None,
            created_at: Utc::now().to_rfc3339(),
            last_played: None,
            play_time_seconds: 0,
            notes: String::new(),
        }
    }

    #[test]
    fn save_instance_writes_prism_metadata_files() {
        let dir = temp_dir("save_meta");
        let instances_dir = dir.join("instances");
        let instance = test_instance("MetaPack", "1.21.1", LoaderType::NeoForge, Some("21.1.135"));
        save_instance(&instances_dir, &instance, None).unwrap();

        let root = instances_dir.join("MetaPack");
        assert!(root.join("instance.cfg").exists());
        assert!(root.join("mmc-pack.json").exists());

        let pack: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("mmc-pack.json")).unwrap())
                .unwrap();
        let comps = pack["components"].as_array().unwrap();
        assert_eq!(comps[0]["uid"], "net.minecraft");
        assert_eq!(comps[0]["version"], "1.21.1");
        assert_eq!(comps[1]["uid"], "net.neoforged");
        assert_eq!(comps[1]["version"], "21.1.135");

        let cfg = std::fs::read_to_string(root.join("instance.cfg")).unwrap();
        assert!(
            cfg.contains("InstanceType=OneSix"),
            "cfg must mark modern instance: {}",
            cfg
        );
        assert!(cfg.contains("name=MetaPack"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_export_writes_prism_layout() {
        let dir = temp_dir("export_layout");
        let instances_dir = dir.join("instances");
        let instance = test_instance("ExportPack", "1.20.1", LoaderType::Fabric, Some("0.15.11"));
        save_instance(&instances_dir, &instance, None).unwrap();

        let mc_dir = instance.minecraft_dir(&instances_dir);
        std::fs::create_dir_all(mc_dir.join("mods")).unwrap();
        std::fs::write(mc_dir.join("mods/testmod.jar"), b"PK\x03\x04jar").unwrap();
        std::fs::create_dir_all(mc_dir.join("logs")).unwrap();
        std::fs::write(mc_dir.join("logs/latest.log"), b"junk").unwrap();
        std::fs::create_dir_all(mc_dir.join("crash-reports")).unwrap();
        std::fs::write(mc_dir.join("crash-reports/crash.txt"), b"junk").unwrap();

        let out = dir.join("export.zip");
        export_instance(&instances_dir, "ExportPack", out.to_str().unwrap()).unwrap();

        let file = std::fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();

        assert!(names.contains(&"instance.cfg".to_string()), "{:?}", names);
        assert!(names.contains(&"mmc-pack.json".to_string()), "{:?}", names);
        assert!(
            names.contains(&"minecraft/mods/testmod.jar".to_string()),
            "game dir must be minecraft/ (no dot): {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.starts_with(".minecraft")),
            "no dotted dir: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("instance.json")),
            "internal metadata excluded: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("logs")),
            "logs excluded: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("crash-reports")),
            "crash reports excluded: {:?}",
            names
        );

        let mut mmc = String::new();
        archive
            .by_name("mmc-pack.json")
            .unwrap()
            .read_to_string(&mut mmc)
            .unwrap();
        let pack: serde_json::Value = serde_json::from_str(&mmc).unwrap();
        assert_eq!(pack["formatVersion"], 1);
        let comps = pack["components"].as_array().unwrap();
        assert_eq!(comps[0]["uid"], "net.minecraft");
        assert_eq!(comps[0]["important"], true);
        assert_eq!(comps[1]["uid"], "net.fabricmc.fabric-loader");
        assert_eq!(comps[1]["version"], "0.15.11");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_export_round_trip_preserves_instance() {
        let dir = temp_dir("round_trip");
        let src_dir = dir.join("src_instances");
        let instance = test_instance("RoundTrip", "1.20.1", LoaderType::Forge, Some("47.2.0"));
        save_instance(&src_dir, &instance, None).unwrap();
        let mc_dir = instance.minecraft_dir(&src_dir);
        std::fs::create_dir_all(mc_dir.join("mods")).unwrap();
        std::fs::write(mc_dir.join("mods/sodium.jar"), b"PK\x03\x04jar").unwrap();

        let out = dir.join("round.zip");
        export_instance(&src_dir, "RoundTrip", out.to_str().unwrap()).unwrap();

        let dst_dir = dir.join("dst_instances");
        let imported = import_prism_pack(&dst_dir, out.to_str().unwrap()).unwrap();
        assert_eq!(imported.name, "RoundTrip");
        assert_eq!(imported.mc_version, "1.20.1");
        assert_eq!(imported.loader, LoaderType::Forge);
        assert_eq!(imported.loader_version.as_deref(), Some("47.2.0"));
        assert!(dst_dir
            .join("RoundTrip/.minecraft/mods/sodium.jar")
            .exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_import_rejects_quilt_packs() {
        let dir = temp_dir("quilt_reject");
        let zip_path = dir.join("quilt.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip_w.start_file("instance.cfg", opts).unwrap();
        zip_w.write_all(b"[General]\nname=Quilt Pack\n").unwrap();
        zip_w.start_file("mmc-pack.json", opts).unwrap();
        zip_w.write_all(
            br#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"org.quiltmc.quilt-loader","version":"0.20.2"}],"formatVersion":1}"#,
        )
        .unwrap();
        zip_w.finish().unwrap();

        let instances_dir = dir.join("instances");
        let err = import_prism_pack(&instances_dir, zip_path.to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("Quilt"),
            "error must name Quilt: {}",
            err
        );
        assert!(
            !instances_dir.join("Quilt Pack").exists(),
            "no instance written on rejection"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_meta_uses_index_folder_with_legacy_fallback() {
        let dir = temp_dir("sidecar");
        let mods = dir.join("mods");
        std::fs::create_dir_all(&mods).unwrap();

        // New layout: hidden .index/ folder next to the content dir
        std::fs::create_dir_all(mods.join(".index")).unwrap();
        std::fs::write(
            mods.join(".index").join("foo.voidlauncher.json"),
            r#"{"provider":"modrinth","project_id":"abc"}"#,
        )
        .unwrap();
        let meta = read_sidecar_meta(&mods, "foo.jar").unwrap();
        assert_eq!(meta["project_id"], "abc");
        assert_eq!(
            sidecar_meta_path(&mods, "foo.jar"),
            mods.join(".index/foo.voidlauncher.json")
        );

        // .disabled file resolves to the same stem
        let meta = read_sidecar_meta(&mods, "foo.jar.disabled").unwrap();
        assert_eq!(meta["project_id"], "abc");

        // Legacy layout (file next to the content) still works as fallback
        std::fs::write(
            mods.join("bar.voidlauncher.json"),
            r#"{"provider":"curseforge"}"#,
        )
        .unwrap();
        let meta = read_sidecar_meta(&mods, "bar.zip").unwrap();
        assert_eq!(meta["provider"], "curseforge");

        // Removal cleans both layouts
        std::fs::write(mods.join(".index").join("baz.voidlauncher.json"), "{}").unwrap();
        std::fs::write(mods.join("baz.voidlauncher.json"), "{}").unwrap();
        remove_sidecar_meta(&mods, "baz.jar");
        assert!(!mods.join(".index").join("baz.voidlauncher.json").exists());
        assert!(!mods.join("baz.voidlauncher.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_packs_hides_prism_metadata_and_index_folder() {
        let dir = temp_dir("packs");
        let instances_dir = dir.join("instances");
        let now = chrono::Utc::now().to_rfc3339();
        let instance = Instance {
            name: "P".to_string(),
            mc_version: "1.20.1".to_string(),
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
        save_instance(&instances_dir, &instance, None).unwrap();

        let shaders = instances_dir
            .join("P")
            .join(".minecraft")
            .join("shaderpacks");
        std::fs::create_dir_all(&shaders).unwrap();
        std::fs::write(shaders.join("BSL_v10.zip"), b"PK\x03\x04x").unwrap();
        std::fs::write(shaders.join("bsl-shaders.pw.toml"), b"meta").unwrap();
        std::fs::create_dir_all(shaders.join(".index")).unwrap();
        std::fs::write(shaders.join(".index").join("bsl.voidlauncher.json"), b"{}").unwrap();

        let packs = list_packs(&instances_dir, "P", "shaderpacks").unwrap();
        assert_eq!(packs.len(), 1, "Prism .pw.toml and .index must be hidden");
        assert_eq!(packs[0].filename, "BSL_v10.zip");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_version_from_filename_handles_common_shapes() {
        assert_eq!(
            extract_version_from_filename("SE Vanilla Consistency 1.20.1.zip"),
            "1.20.1"
        );
        assert_eq!(
            extract_version_from_filename("Aether Regenerated v1.3.1.zip"),
            "1.3.1"
        );
        assert_eq!(
            extract_version_from_filename("recipeessentials-1.20.1-4.7.jar"),
            "4.7"
        );
        assert_eq!(
            extract_version_from_filename("Ping-Wheel-1.12.1-forge-1.20.1.jar"),
            "1.20.1"
        );
        assert_eq!(extract_version_from_filename("mod_v5.8.1.jar"), "5.8.1");
        assert_eq!(
            extract_version_from_filename("Hearths v1.0.5.mod.jar"),
            "1.0.5"
        );
        assert_eq!(
            extract_version_from_filename("twilightforest-1.20.1-4.3.2508-universal.jar"),
            "4.3.2508"
        );
        assert_eq!(
            extract_version_from_filename("aether-1.20.1-1.5.2-neoforge.jar"),
            "1.5.2"
        );
        assert_eq!(
            extract_version_from_filename("Patchouli-1.20.1-85-FORGE.jar"),
            "85"
        );
        assert_eq!(
            extract_version_from_filename("TerraBlender-forge-1.20.1-3.0.1.10.jar"),
            "3.0.1.10"
        );
        assert_eq!(
            extract_version_from_filename("kotlinforforge-4.12.0-all.jar"),
            "4.12.0"
        );
        assert_eq!(
            extract_version_from_filename("ConnectorExtras-1.11.2+1.20.1.jar"),
            "1.11.2+1.20.1"
        );
        assert_eq!(
            extract_version_from_filename("Connector-1.0.0-beta.49+1.20.1.jar"),
            "1.0.0-beta.49+1.20.1"
        );
        assert_eq!(
            extract_version_from_filename("curios-forge-5.14.1+1.20.1.jar"),
            "5.14.1+1.20.1"
        );
        assert_eq!(
            extract_version_from_filename("CataclysmCompat1.0.zip"),
            "1.0"
        );
        assert_eq!(
            extract_version_from_filename("BoP x FD Bark Cutting Compat.zip"),
            ""
        );
        assert_eq!(extract_version_from_filename("Better_Modded_GUI.zip"), "");
        assert_eq!(extract_version_from_filename("NoBushyLeaves.zip"), "");
        assert_eq!(extract_version_from_filename("fresh_waystones.zip"), "");
        assert_eq!(
            extract_version_from_filename("BetterBetterX-v1.1-noStone.zip"),
            ""
        );
        assert_eq!(
            extract_version_from_filename("Geophilic v3.6.mod.jar"),
            "3.6"
        );
        assert_eq!(
            extract_version_from_filename("LessStructures-SpacingTweaks-1.20.1-2.1.56.zip"),
            "2.1.56"
        );
        assert_eq!(
            extract_version_from_filename("Modded Omelet [120] [162].zip"),
            ""
        );
    }

    // --- Transactional content update (park → download → swap / rollback) ---

    #[test]
    fn stash_parks_old_jar_and_restore_puts_it_back() {
        let dir = temp_dir("stash_restore");
        std::fs::write(dir.join("sodium-0.5.8.jar"), b"old-bytes").unwrap();

        let stashed = stash_old_content_files(&dir, "sodium-0.5.8.jar").unwrap();
        assert_eq!(stashed.len(), 1);
        assert!(
            !dir.join("sodium-0.5.8.jar").exists(),
            "old file must be parked, not live"
        );
        assert!(dir.join("sodium-0.5.8.jar.voidlauncher-old").exists());

        // Simulated failed download: no new file, rollback the park.
        restore_stashed_files(&stashed);
        assert!(
            dir.join("sodium-0.5.8.jar").exists(),
            "old file must survive a failed download"
        );
        assert_eq!(
            std::fs::read(dir.join("sodium-0.5.8.jar")).unwrap(),
            b"old-bytes"
        );
        assert!(!dir.join("sodium-0.5.8.jar.voidlauncher-old").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stash_parks_disabled_and_enabled_twins() {
        let dir = temp_dir("stash_twins");
        std::fs::write(dir.join("sodium.jar.disabled"), b"disabled").unwrap();
        std::fs::write(dir.join("sodium.jar"), b"enabled").unwrap();

        let stashed = stash_old_content_files(&dir, "sodium.jar.disabled").unwrap();
        assert_eq!(
            stashed.len(),
            2,
            "both the disabled file and its enabled twin must be parked"
        );
        assert!(!dir.join("sodium.jar.disabled").exists());
        assert!(!dir.join("sodium.jar").exists());
        assert!(dir.join("sodium.jar.disabled.voidlauncher-old").exists());
        assert!(dir.join("sodium.jar.voidlauncher-old").exists());

        discard_stashed_files(&stashed);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "discard removes both parks"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_name_update_replaces_old_file() {
        let dir = temp_dir("stash_same_name");
        std::fs::write(dir.join("sodium.jar"), b"old-bytes").unwrap();

        let stashed = stash_old_content_files(&dir, "sodium.jar").unwrap();
        assert_eq!(stashed.len(), 1);
        // The name is free now: the "download" writes the replacement.
        std::fs::write(dir.join("sodium.jar"), b"new-bytes").unwrap();
        discard_stashed_files(&stashed);

        assert_eq!(std::fs::read(dir.join("sodium.jar")).unwrap(), b"new-bytes");
        assert!(
            !dir.join("sodium.jar.voidlauncher-old").exists(),
            "no leftovers after success"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_download_with_bad_replacement_rolls_back() {
        let dir = temp_dir("stash_bad_replacement");
        std::fs::write(dir.join("sodium-0.5.8.jar"), b"old-bytes").unwrap();

        let stashed = stash_old_content_files(&dir, "sodium-0.5.8.jar").unwrap();
        // Simulate a download that renamed into place but failed verification
        // (zip-magic failure happens after the rename).
        std::fs::write(dir.join("sodium-0.6.0.jar"), b"<html>error page</html>").unwrap();
        let dest = dir.join("sodium-0.6.0.jar");
        let _ = std::fs::remove_file(&dest);
        restore_stashed_files(&stashed);

        assert!(dir.join("sodium-0.5.8.jar").exists(), "old file restored");
        assert!(!dest.exists(), "bad replacement dropped");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stash_sanitizes_traversal_and_rejects_dotdot() {
        let dir = temp_dir("stash_traversal");
        // A decoy outside the content dir that must never be touched.
        let outside = dir.parent().unwrap().join(format!(
            "voidlauncher_stash_outside_{}_evil.jar",
            std::process::id()
        ));
        std::fs::write(&outside, b"outside").unwrap();

        // Path components are stripped: only files inside `dir` are candidates.
        let stashed = stash_old_content_files(&dir, "../evil.jar").unwrap();
        assert!(
            stashed.is_empty(),
            "sanitized name must not resolve outside the dir"
        );
        assert!(
            outside.exists(),
            "decoy outside the content dir must be untouched"
        );

        assert!(stash_old_content_files(&dir, "..").is_err());
        assert!(stash_old_content_files(&dir, ".").is_err());

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recover_restores_park_when_original_missing_and_drops_stale_park() {
        let dir = temp_dir("stash_recover");
        // Interrupted update: only the park survived → restore it.
        std::fs::write(dir.join("sodium.jar.voidlauncher-old"), b"old-bytes").unwrap();
        recover_stashed_content_files(&dir);
        assert!(
            dir.join("sodium.jar").exists(),
            "park must be restored when original is missing"
        );
        assert!(!dir.join("sodium.jar.voidlauncher-old").exists());

        // Completed update with a leftover park → drop the stale park.
        std::fs::write(dir.join("sodium.jar"), b"new-bytes").unwrap();
        std::fs::write(dir.join("sodium.jar.voidlauncher-old"), b"old-bytes").unwrap();
        recover_stashed_content_files(&dir);
        assert_eq!(std::fs::read(dir.join("sodium.jar")).unwrap(), b"new-bytes");
        assert!(!dir.join("sodium.jar.voidlauncher-old").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_old_sidecar_only_on_stem_change() {
        let dir = temp_dir("stash_sidecar");
        std::fs::create_dir_all(dir.join(".index")).unwrap();
        std::fs::write(dir.join(".index").join("sodium.voidlauncher.json"), b"{}").unwrap();
        std::fs::write(
            dir.join(".index").join("sodium-0.5.8.voidlauncher.json"),
            b"{}",
        )
        .unwrap();
        std::fs::write(
            dir.join(".index").join("sodium-0.6.0.voidlauncher.json"),
            b"{}",
        )
        .unwrap();

        // Same stem (re-download under the same file name): keep the sidecar.
        cleanup_old_sidecar_after_update(&dir, "sodium.jar", "sodium.jar");
        assert!(
            dir.join(".index").join("sodium.voidlauncher.json").exists(),
            "same-stem sidecar must survive"
        );

        // Stem changed: the old sidecar would describe a file that is gone.
        cleanup_old_sidecar_after_update(&dir, "sodium-0.5.8.jar", "sodium-0.6.0.jar");
        assert!(
            !dir.join(".index")
                .join("sodium-0.5.8.voidlauncher.json")
                .exists(),
            "stale sidecar must be removed when the stem changes"
        );
        assert!(
            dir.join(".index")
                .join("sodium-0.6.0.voidlauncher.json")
                .exists(),
            "new sidecar must survive"
        );
        assert!(
            dir.join(".index").join("sodium.voidlauncher.json").exists(),
            "unrelated sidecar must survive"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_world(instances_dir: &Path, instance: &Instance, world: &str, marker: &str) {
        let world_dir = instance
            .minecraft_dir(instances_dir)
            .join("saves")
            .join(world);
        std::fs::create_dir_all(world_dir.join("region")).unwrap();
        std::fs::write(world_dir.join("level.dat"), format!("level-{}", marker)).unwrap();
        std::fs::write(
            world_dir.join("region").join("r.0.0.mca"),
            format!("region-{}", marker),
        )
        .unwrap();
    }

    #[test]
    fn backup_world_creates_archive_and_restore_replace_overwrites() {
        let dir = temp_dir("world_backup");
        let instances_dir = dir.join("instances");
        let instance = test_instance("WorldPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_world(&instances_dir, &instance, "MyWorld", "v1");

        let info = backup_world(&instances_dir, "WorldPack", "MyWorld").unwrap();
        assert!(info.filename.starts_with("MyWorld__") && info.filename.ends_with(".zip"));
        assert!(info.size_bytes > 0);

        let list = list_world_backups(&instances_dir, "WorldPack", "MyWorld").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].filename, info.filename);

        // Damage the live world, then restore by replacing it.
        let world_dir = instance
            .minecraft_dir(&instances_dir)
            .join("saves")
            .join("MyWorld");
        std::fs::write(world_dir.join("level.dat"), "level-corrupted").unwrap();
        restore_world(
            &instances_dir,
            "WorldPack",
            "MyWorld",
            &info.filename,
            "replace",
            None,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(world_dir.join("level.dat")).unwrap(),
            "level-v1"
        );
        assert_eq!(
            std::fs::read_to_string(world_dir.join("region").join("r.0.0.mca")).unwrap(),
            "region-v1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_world_as_new_creates_distinct_world() {
        let dir = temp_dir("world_restore_new");
        let instances_dir = dir.join("instances");
        let instance = test_instance("NewWorldPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_world(&instances_dir, &instance, "Src", "orig");

        let info = backup_world(&instances_dir, "NewWorldPack", "Src").unwrap();
        restore_world(
            &instances_dir,
            "NewWorldPack",
            "Src",
            &info.filename,
            "as_new",
            Some("Restored Copy"),
        )
        .unwrap();

        let saves = instance.minecraft_dir(&instances_dir).join("saves");
        assert_eq!(
            std::fs::read_to_string(saves.join("Restored Copy").join("level.dat")).unwrap(),
            "level-orig"
        );
        // The source world is untouched.
        assert_eq!(
            std::fs::read_to_string(saves.join("Src").join("level.dat")).unwrap(),
            "level-orig"
        );

        // as_new onto an existing name must fail (no overwrite outside "replace").
        assert!(restore_world(
            &instances_dir,
            "NewWorldPack",
            "Src",
            &info.filename,
            "as_new",
            Some("Src")
        )
        .is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn world_backup_filename_traversal_rejected() {
        let dir = temp_dir("world_traversal");
        let instances_dir = dir.join("instances");
        let instance = test_instance("TravPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_world(&instances_dir, &instance, "W", "x");
        let decoy = instance.dir(&instances_dir).join("decoy.txt");
        std::fs::write(&decoy, b"keep").unwrap();

        for evil in [
            "../decoy.txt.zip",
            "..\\decoy.txt.zip",
            "C:/Windows/x.zip",
            "/abs.zip",
            "no-extension",
            ".hidden.zip",
            "sub/name.zip",
        ] {
            assert!(
                restore_world(&instances_dir, "TravPack", "W", evil, "replace", None).is_err(),
                "filename {:?} must be rejected",
                evil
            );
            assert!(
                delete_world_backup(&instances_dir, "TravPack", "W", evil).is_err(),
                "delete filename {:?} must be rejected",
                evil
            );
        }
        assert!(decoy.exists(), "decoy must survive");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_world_backups_sorts_newest_first_and_delete_removes() {
        let dir = temp_dir("world_backup_list");
        let instances_dir = dir.join("instances");
        let instance = test_instance("ListPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_world(&instances_dir, &instance, "LW", "a");

        // No backups yet → empty list, no error.
        assert!(list_world_backups(&instances_dir, "ListPack", "LW")
            .unwrap()
            .is_empty());

        let a = backup_world(&instances_dir, "ListPack", "LW").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let b = backup_world(&instances_dir, "ListPack", "LW").unwrap();
        assert_ne!(
            a.filename, b.filename,
            "same-second backups must not collide"
        );

        let list = list_world_backups(&instances_dir, "ListPack", "LW").unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].created_at >= list[1].created_at, "newest first");

        delete_world_backup(&instances_dir, "ListPack", "LW", &a.filename).unwrap();
        let after = list_world_backups(&instances_dir, "ListPack", "LW").unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].filename, b.filename);
        assert!(delete_world_backup(&instances_dir, "ListPack", "LW", &a.filename).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Instance backup / restore ────────────────────────────────

    fn make_instance_files(instances_dir: &Path, instance: &Instance) {
        let mc = instance.minecraft_dir(instances_dir);
        std::fs::create_dir_all(mc.join("mods")).unwrap();
        std::fs::write(mc.join("mods").join("sodium.jar"), b"jar-bytes").unwrap();
        std::fs::create_dir_all(mc.join("logs")).unwrap();
        std::fs::write(mc.join("logs").join("latest.log"), b"log").unwrap();
        std::fs::create_dir_all(mc.join("screenshots")).unwrap();
        std::fs::write(mc.join("screenshots").join("shot.png"), b"png").unwrap();
        std::fs::create_dir_all(mc.join("crash-reports")).unwrap();
        std::fs::write(mc.join("crash-reports").join("crash.txt"), b"crash").unwrap();
    }

    fn zip_entry_names(path: &std::path::Path) -> Vec<String> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn backup_instance_excludes_junk_and_lists() {
        let dir = temp_dir("inst_backup");
        let instances_dir = dir.join("instances");
        let instance = test_instance("BackupMe", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        let info = backup_instance(&instances_dir, "BackupMe").unwrap();
        assert!(info.filename.starts_with("BackupMe__") && info.filename.ends_with(".zip"));
        assert!(info.size_bytes > 0);

        let zip_path = instances_dir
            .join("BackupMe")
            .join(".backups")
            .join("instances")
            .join(&info.filename);
        let names = zip_entry_names(&zip_path);
        assert!(names.iter().any(|n| n == "instance.json"), "{:?}", names);
        assert!(names.iter().any(|n| n == "instance.cfg"), "{:?}", names);
        assert!(
            names.iter().any(|n| n == ".minecraft/mods/sodium.jar"),
            "{:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("logs")),
            "logs leaked: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("screenshots")),
            "screenshots leaked: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("crash-reports")),
            "crash reports leaked: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains(".backups")),
            "backups leaked into themselves: {:?}",
            names
        );

        assert_eq!(
            list_instance_backups(&instances_dir, "BackupMe")
                .unwrap()
                .len(),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_instance_replace_recovers_content_and_keeps_backup_history() {
        let dir = temp_dir("inst_restore_replace");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Victim", "1.21.1", LoaderType::Fabric, Some("0.16.0"));
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        let info = backup_instance(&instances_dir, "Victim").unwrap();

        // Corrupt: break the metadata AND delete the mod — restore must not
        // require the damaged metadata to run.
        let inst = instances_dir.join("Victim");
        std::fs::remove_file(inst.join("instance.json")).unwrap();
        std::fs::remove_file(inst.join(".minecraft").join("mods").join("sodium.jar")).unwrap();

        restore_instance(&instances_dir, "Victim", &info.filename, "replace", None).unwrap();

        assert!(inst.join("instance.json").exists(), "metadata restored");
        assert_eq!(
            std::fs::read(inst.join(".minecraft").join("mods").join("sodium.jar")).unwrap(),
            b"jar-bytes",
            "mod restored"
        );
        assert!(
            !inst.join(".minecraft").join("logs").exists(),
            "junk stays excluded"
        );
        // The swap must not lose the backup history that lives in `.backups`.
        let list = list_instance_backups(&instances_dir, "Victim").unwrap();
        assert_eq!(list.len(), 1, "backup history preserved across restore");
        assert_eq!(list[0].filename, info.filename);
        // And it still parses as a launchable instance.
        assert_eq!(
            get_instance(&instances_dir, "Victim").unwrap().name,
            "Victim"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_instance_as_new_creates_second_instance() {
        let dir = temp_dir("inst_restore_asnew");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Original", "1.21.1", LoaderType::NeoForge, Some("21.1.135"));
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        let info = backup_instance(&instances_dir, "Original").unwrap();
        restore_instance(
            &instances_dir,
            "Original",
            &info.filename,
            "as_new",
            Some("Restored Copy"),
        )
        .unwrap();

        let restored = instances_dir.join("Restored Copy");
        assert!(restored.join("instance.json").exists());
        assert_eq!(
            std::fs::read(restored.join(".minecraft").join("mods").join("sodium.jar")).unwrap(),
            b"jar-bytes"
        );
        assert_eq!(
            get_instance(&instances_dir, "Restored Copy").unwrap().name,
            "Restored Copy"
        );

        // Original untouched, its backups stay with it; the copy has none.
        assert!(instances_dir
            .join("Original")
            .join("instance.json")
            .exists());
        assert_eq!(
            list_instance_backups(&instances_dir, "Original")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_instance_backups(&instances_dir, "Restored Copy")
                .unwrap()
                .len(),
            0
        );

        // as_new onto an existing name must fail (no silent overwrite).
        assert!(restore_instance(
            &instances_dir,
            "Original",
            &info.filename,
            "as_new",
            Some("Restored Copy")
        )
        .is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn instance_backup_filename_traversal_rejected() {
        let dir = temp_dir("inst_backup_traversal");
        let instances_dir = dir.join("instances");
        let instance = test_instance("TravI", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);
        let decoy = instance.dir(&instances_dir).join("decoy.txt");
        std::fs::write(&decoy, b"keep").unwrap();

        for evil in [
            "../decoy.txt.zip",
            "..\\decoy.txt.zip",
            "C:/Windows/x.zip",
            "/abs.zip",
            "no-extension",
            ".hidden.zip",
            "sub/name.zip",
        ] {
            assert!(
                restore_instance(&instances_dir, "TravI", evil, "replace", None).is_err(),
                "restore filename {:?} must be rejected",
                evil
            );
            assert!(
                delete_instance_backup(&instances_dir, "TravI", evil).is_err(),
                "delete filename {:?} must be rejected",
                evil
            );
        }
        assert!(decoy.exists(), "decoy must survive");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_instance_rejects_archive_without_metadata_and_cleans_temp() {
        let dir = temp_dir("inst_restore_invalid");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Brokee", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        // A zip that is not an instance (no instance.json / instance.cfg).
        let backups = instances_dir
            .join("Brokee")
            .join(".backups")
            .join("instances");
        std::fs::create_dir_all(&backups).unwrap();
        let bad = backups.join("not-instance.zip");
        let file = std::fs::File::create(&bad).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zw.start_file("junk.txt", opts).unwrap();
        zw.write_all(b"hi").unwrap();
        zw.finish().unwrap();

        let mod_before = std::fs::read(
            instances_dir
                .join("Brokee")
                .join(".minecraft")
                .join("mods")
                .join("sodium.jar"),
        )
        .unwrap();
        assert!(restore_instance(
            &instances_dir,
            "Brokee",
            "not-instance.zip",
            "replace",
            None
        )
        .is_err());

        // The live instance must be untouched and no temp dirs left behind.
        assert!(instances_dir.join("Brokee").join("instance.json").exists());
        assert_eq!(
            std::fs::read(
                instances_dir
                    .join("Brokee")
                    .join(".minecraft")
                    .join("mods")
                    .join("sodium.jar")
            )
            .unwrap(),
            mod_before
        );
        for entry in std::fs::read_dir(&instances_dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with(".void-restore-tmp"),
                "temp dir leaked: {}",
                name
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Duplicate ────────────────────────────────────────────────

    #[test]
    fn duplicate_instance_copies_everything_except_backup_history() {
        let dir = temp_dir("dup_default");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Src", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);
        let backups = instances_dir.join("Src").join(".backups").join("instances");
        std::fs::create_dir_all(&backups).unwrap();
        std::fs::write(backups.join("Src__old.zip"), b"zip-bytes").unwrap();

        let copy = duplicate_instance(&instances_dir, "Src", "Dst", DuplicateExclusions::default())
            .unwrap();

        assert_eq!(copy.name, "Dst");
        assert_eq!(copy.play_time_seconds, 0);
        assert!(copy.last_played.is_none());
        let dst = instances_dir.join("Dst");
        assert!(dst.join("instance.json").exists());
        assert!(dst
            .join(".minecraft")
            .join("mods")
            .join("sodium.jar")
            .exists());
        assert!(dst
            .join(".minecraft")
            .join("logs")
            .join("latest.log")
            .exists());
        assert!(
            !dst.join(".backups").exists(),
            "source backup history must stay with the original"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_instance_excludes_picked_content() {
        let dir = temp_dir("dup_exclude");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Src", "1.21.1", LoaderType::Fabric, Some("0.16.0"));
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);
        let mc = instance.minecraft_dir(&instances_dir);
        std::fs::create_dir_all(mc.join("saves").join("World")).unwrap();
        std::fs::write(mc.join("saves").join("World").join("level.dat"), b"lvl").unwrap();

        let exclude = DuplicateExclusions {
            saves: true,
            screenshots: true,
            logs: true,
        };
        duplicate_instance(&instances_dir, "Src", "Dst", exclude).unwrap();

        let dst_mc = instances_dir.join("Dst").join(".minecraft");
        // Metadata + mods always travel.
        assert!(instances_dir.join("Dst").join("instance.json").exists());
        assert!(dst_mc.join("mods").join("sodium.jar").exists());
        assert!(!dst_mc.join("saves").exists(), "saves excluded");
        assert!(!dst_mc.join("screenshots").exists(), "screenshots excluded");
        assert!(!dst_mc.join("logs").exists(), "logs excluded");
        assert!(
            !dst_mc.join("crash-reports").exists(),
            "crash reports follow logs"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

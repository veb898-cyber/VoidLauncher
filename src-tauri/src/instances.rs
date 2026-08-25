use serde::{Deserialize, Serialize};
use crate::error::{LauncherError, Result};
use crate::modloaders::LoaderProfile;
use std::path::{Path, PathBuf};
use std::io::Read;
use std::collections::HashMap;
use chrono::Utc;
use flate2::read::GzDecoder;

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
    pub fn dir(&self, instances_dir: &PathBuf) -> PathBuf {
        instances_dir.join(&self.name)
    }

    /// Get the .minecraft directory inside the instance
    pub fn minecraft_dir(&self, instances_dir: &PathBuf) -> PathBuf {
        self.dir(instances_dir).join(".minecraft")
    }

    /// Get mods directory
    pub fn mods_dir(&self, instances_dir: &PathBuf) -> PathBuf {
        self.minecraft_dir(instances_dir).join("mods")
    }

/// Get config file path
pub fn config_file(&self, instances_dir: &PathBuf) -> PathBuf {
    self.dir(instances_dir).join("instance.json")
}

/// Get Prism-compatible instance.cfg path
pub fn prism_cfg_file(&self, instances_dir: &PathBuf) -> PathBuf {
    self.dir(instances_dir).join("instance.cfg")
}

}

/// List all instances (supports both instance.json and instance.cfg)
pub fn list_instances(instances_dir: &PathBuf) -> Result<Vec<Instance>> {
    let mut instances = Vec::new();

    if !instances_dir.exists() {
        return Ok(instances);
    }

    for entry in std::fs::read_dir(instances_dir)? {
        let entry = entry?;
        let dir_path = entry.path();
        if !dir_path.is_dir() { continue; }
        let json_path = dir_path.join("instance.json");
        let cfg_path = dir_path.join("instance.cfg");

        if json_path.exists() {
            match std::fs::read_to_string(&json_path) {
                Ok(contents) => match serde_json::from_str::<Instance>(&contents) {
                    Ok(instance) => instances.push(instance),
                    Err(e) => tracing::warn!(target: "launcher", "Failed to parse instance at {:?}: {}", json_path, e),
                },
                Err(e) => tracing::warn!(target: "launcher", "Failed to read instance at {:?}: {}", json_path, e),
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
            .cmp(&a.last_played.as_deref().unwrap_or(""))
    });

    Ok(instances)
}

/// Create a new instance
pub fn create_instance(instances_dir: &PathBuf, instance: &Instance) -> Result<()> {
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
    std::fs::create_dir_all(
        instance.minecraft_dir(instances_dir).join("resourcepacks"),
    )?;
    std::fs::create_dir_all(
        instance.minecraft_dir(instances_dir).join("shaderpacks"),
    )?;
    std::fs::create_dir_all(
        instance.minecraft_dir(instances_dir).join("config"),
    )?;

    // Save instance config
    save_instance(instances_dir, instance, None)?;

    Ok(())
}

/// Save instance config to disk, plus Prism-compatible instance.cfg and pack.png.
/// If `old_name` differs from `instance.name`, the instance directory is renamed first.
pub fn save_instance(instances_dir: &PathBuf, instance: &Instance, old_name: Option<&str>) -> Result<()> {
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
    let config_dir = config_path.parent().ok_or_else(|| {
        LauncherError::Instance("Instance config has no parent directory".into())
    })?;
    if !config_dir.exists() {
        std::fs::create_dir_all(config_dir)?;
    }
    let json = serde_json::to_string_pretty(instance)?;
    std::fs::write(&config_path, json)?;

    // Write Prism-compatible instance.cfg
    write_prism_cfg(instance, instances_dir)?;

    // Write pack.png if instance has an icon
    if let Some(ref icon) = instance.icon {
        write_pack_png(instances_dir, &instance.name, icon);
    }

    Ok(())
}

/// Delete an instance
pub fn delete_instance(instances_dir: &PathBuf, name: &str) -> Result<()> {
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
pub fn get_instance(instances_dir: &PathBuf, name: &str) -> Result<Instance> {
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

/// Duplicate an instance
pub fn duplicate_instance(instances_dir: &PathBuf, name: &str, new_name: &str) -> Result<Instance> {
    let src = instances_dir.join(name);
    let dst = instances_dir.join(new_name);
    if !src.exists() {
        return Err(LauncherError::Instance(format!("Instance '{}' not found", name)));
    }
    if dst.exists() {
        return Err(LauncherError::Instance(format!("Instance '{}' already exists", new_name)));
    }
    // Copy entire directory recursively
    copy_dir_recursive(&src, &dst)?;
    // Read and update the instance config
    let mut instance = get_instance(instances_dir, new_name)?;
    instance.name = new_name.to_string();
    instance.last_played = None;
    instance.play_time_seconds = 0;
    instance.created_at = Utc::now().to_rfc3339();
    save_instance(instances_dir, &instance, None)?;
    Ok(instance)
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dst_path)?;
        } else {
            std::fs::copy(&path, &dst_path)?;
        }
    }
    Ok(())
}

/// Save instance icon (base64 data URL)
pub fn save_instance_icon(instances_dir: &PathBuf, name: &str, icon_data: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.icon = Some(icon_data.to_string());
    save_instance(instances_dir, &instance, None)?;
    Ok(())
}

/// Save instance banner (base64 data URL or gradient:name, empty to remove)
pub fn save_instance_banner(instances_dir: &PathBuf, name: &str, banner_data: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.banner = if banner_data.is_empty() { None } else { Some(banner_data.to_string()) };
    save_instance(instances_dir, &instance, None)?;
    Ok(())
}

/// List saves (worlds)
pub fn list_saves(instances_dir: &PathBuf, name: &str) -> Result<Vec<SaveEntry>> {
    let instance = get_instance(instances_dir, name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let mut saves = Vec::new();
    if !saves_dir.exists() { return Ok(saves); }
    for entry in std::fs::read_dir(&saves_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let path = entry.path();
            let world_name = entry.file_name().to_string_lossy().to_string();
            let last_modified = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok())
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64);
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
    saves.sort_by(|a, b| b.last_modified.unwrap_or(0).cmp(&a.last_modified.unwrap_or(0)));
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
pub fn list_screenshots(instances_dir: &PathBuf, name: &str) -> Result<Vec<ScreenshotEntry>> {
    let instance = get_instance(instances_dir, name)?;
    let dir = instance.minecraft_dir(instances_dir).join("screenshots");
    let mut out = Vec::new();
    if !dir.exists() { return Ok(out); }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if ext == "png" || ext == "jpg" || ext == "jpeg" {
            let meta = std::fs::metadata(&path)?;
            out.push(ScreenshotEntry {
                filename: entry.file_name().to_string_lossy().to_string(),
                last_modified: meta.modified().ok()
                    .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64),
                size_bytes: meta.len(),
            });
        }
    }
    out.sort_by(|a, b| b.last_modified.unwrap_or(0).cmp(&a.last_modified.unwrap_or(0)));
    Ok(out)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScreenshotEntry {
    pub filename: String,
    pub last_modified: Option<i64>,
    pub size_bytes: u64,
}

pub fn read_screenshot(instances_dir: &PathBuf, instance_name: &str, filename: &str) -> Result<String> {
    let instance = get_instance(instances_dir, instance_name)?;
    let path = instance.minecraft_dir(instances_dir).join("screenshots").join(filename);
    let buf = std::fs::read(&path)
        .map_err(|e| LauncherError::Instance(format!("Cannot read {}: {}", path.display(), e)))?;
    if buf.is_empty() {
        return Err(LauncherError::Instance(format!("Screenshot is empty: {}", path.display())));
    }
    Ok(format!("data:image/png;base64,{}", base64_encode(&buf)))
}

pub fn delete_screenshot(instances_dir: &PathBuf, instance_name: &str, filename: &str) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let path = instance.minecraft_dir(instances_dir).join("screenshots").join(filename);
    if !path.exists() {
        return Err(LauncherError::Instance(format!("Screenshot not found: {}", filename)));
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

/// List resource packs or shader packs (returns entries without icon - fetch via cmd_get_pack_icon)
pub fn list_packs(instances_dir: &PathBuf, name: &str, pack_type: &str) -> Result<Vec<PackEntry>> {
    let instance = get_instance(instances_dir, name)?;
    let packs_dir = instance.minecraft_dir(instances_dir).join(pack_type);
    let mut packs = Vec::new();
    if !packs_dir.exists() { return Ok(packs); }
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
            if lower.ends_with(".json") || lower.ends_with(".txt") || lower.ends_with(".cfg")
                || lower.ends_with(".properties") || lower.ends_with(".toml")
            {
                continue;
            }
        }

        // Read sidecar metadata (project_name is the Modrinth/CurseForge display name)
        let (provider, mut version, project_id, project_name) = read_pack_sidecar(&path).unwrap_or_default();
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
            if is_dir { read_pack_name_from_dir(&path) } else { read_pack_name_from_zip(&path) }
        }.unwrap_or_else(|| {
            let stem = Path::new(&filename).file_stem().and_then(|s| s.to_str()).unwrap_or(&filename);
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
pub fn read_pack_icon(instances_dir: &PathBuf, instance_name: &str, pack_type: &str, filename: &str) -> Result<Option<String>> {
    let instance = get_instance(instances_dir, instance_name)?;
    let pack_path = instance.minecraft_dir(instances_dir).join(pack_type).join(filename);
    if !pack_path.exists() { return Ok(None); }
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
pub(crate) fn read_sidecar_meta(content_dir: &std::path::Path, filename: &str) -> Option<serde_json::Value> {
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
    let mut scan_dirs: Vec<PathBuf> = Vec::with_capacity(2);
    scan_dirs.push(content_dir.join(".index"));
    scan_dirs.push(content_dir.to_path_buf());
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
            if let Some(ver) = v.get("x-prismlauncher-version-number").and_then(|n| n.as_str()) {
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
        let provider = val["provider"].as_str().unwrap_or("").to_string();
        let version = val["version_number"].as_str().unwrap_or("").to_string();
        let project_id = val["project_id"].as_str().unwrap_or("").to_string();
        let project_name = val["project_name"].as_str().unwrap_or("").to_string();
        return Some((provider, version, project_id, project_name));
    }
    // Fallback: Prism packwiz metadata (.pw.toml in .index/)
    let pw = read_packwiz_meta(pack_path.parent()?, &filename)?;
    Some((pw.provider, pw.version, pw.project_id, pw.name))
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
        match stem.rfind('.') {
            Some(idx) => {
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
            None => {}
        }
        break;
    }
    // Strip trailing loader/build markers ("-forge", "-neoforge", "-universal",
    // "-all", ...) so the version right before them is exposed.
    loop {
        let lower = stem.to_lowercase();
        let mut cut = None;
        for marker in [
            "neoforge", "forge", "universal", "universal_jar", "fabric", "fml", "all", "mod", "srg", "dev", "official", "mapped",
        ] {
            let marker_len = marker.len();
            if lower.ends_with(&format!("-{}", marker)) || lower.ends_with(&format!("_{}", marker)) {
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
    while end > 0 && (bytes[end - 1].is_ascii_whitespace() || bytes[end - 1] == b'[' || bytes[end - 1] == b']') {
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
    let mut chars = stem.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
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
        if lower.starts_with("forge") || lower.starts_with("neoforge") || lower.starts_with("fabric") || lower.starts_with("universal") {
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
    if gseen && s > 0 && (bytes[s - 1].is_ascii_alphabetic() || bytes[s - 1] == b'_' || bytes[s - 1] == b'-') {
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
    if icon_path.exists() { return read_image_as_base64(&icon_path); }

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

    let root_preferred = ["pack.png", "pack.jpg", "pack.jpeg", "preview.png", "thumb.png", "icon.png", "logo.png"];

    // Pass 1: root-level preferred names
    for (name, is_dir) in &entries {
        if *is_dir { continue; }
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
        if *is_dir { continue; }
        if name.to_lowercase().ends_with("/pack.png") {
            if let Some(img) = try_read_zip_image(&mut archive, name) {
                return Some(img);
            }
        }
    }

    None
}

fn try_read_zip_image<R: std::io::Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut buf = Vec::new();
    use std::io::Read;
    file.read_to_end(&mut buf).ok()?;
    if buf.is_empty() { return None; }
    let lower = name.to_lowercase();
    let mime = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") { "image/jpeg" } else { "image/png" };
    Some(format!("data:{};base64,{}", mime, base64_encode(&buf)))
}

fn extract_pack_name_from_json(json: &serde_json::Value) -> Option<String> {
    // Prefer pack.name (human-readable) over pack.description (often an internal identifier).
    let raw = extract_text_field(&json["pack"]["name"])
        .or_else(|| extract_text_field(&json["pack"]["description"]))?;

    let stripped = strip_minecraft_color_codes(&raw);
    // Strip common HTML tags used in descriptions
    let clean = stripped.replace("<br>", " ").replace("<br/>", " ").replace("</br>", " ");
    let clean = clean.trim().to_string();
    if clean.is_empty() { None } else { Some(clean) }
}

/// Extract text from a Minecraft JSON text component (string, object with "text", or array).
fn extract_text_field(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        Some(s.to_string())
    } else if let Some(obj) = value.as_object() {
        obj.get("text").and_then(|v| v.as_str()).map(|s| s.to_string())
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
        if result.is_empty() { None } else { Some(result) }
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
    let bytes: Vec<u8> = mcmeta.bytes().filter_map(|b| b.ok()).collect();
    let content = String::from_utf8(bytes).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    extract_pack_name_from_json(&json)
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() { total += dir_size(&p); }
            else if let Ok(m) = std::fs::metadata(&p) { total += m.len(); }
        }
    }
    total
}

fn read_image_as_base64(path: &std::path::Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    if buf.is_empty() { return None }
    Some(format!("data:image/png;base64,{}", base64_encode(&buf)))
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
        if chunk.len() > 1 { result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char); } else { result.push('='); }
        if chunk.len() > 2 { result.push(CHARS[(triple & 0x3F) as usize] as char); } else { result.push('='); }
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

    let clean: Vec<u8> = input.bytes().filter(|&b| b != b'\r' && b != b'\n' && b != b' ').collect();
    if clean.is_empty() || clean.len() % 4 != 0 { return None; }

    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for chunk in clean.chunks(4) {
        if chunk.len() != 4 { return None; }
        let mut vals = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            let v = DECODE.get(b as usize)?;
            if *v == -1 { return None; }
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
pub fn update_last_played(instances_dir: &PathBuf, name: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.last_played = Some(Utc::now().to_rfc3339());
    save_instance(instances_dir, &instance, None)?;
    Ok(())
}

// ── Prism compatibility helpers ─────────────────────────────────

/// Write a Prism/MultiMC-compatible instance.cfg from our Instance
fn write_prism_cfg(instance: &Instance, instances_dir: &PathBuf) -> Result<()> {
    let cfg_path = instance.prism_cfg_file(instances_dir);
    let last_launch = instance.last_played.as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
        .unwrap_or(0);

    let mut lines = Vec::new();
    lines.push("[General]".to_string());
    lines.push(format!("name={}", instance.name));
    lines.push(format!("iconKey={}", instance.name));
    lines.push("notes=".to_string());
    lines.push(format!("lastLaunchTime={}", last_launch));
    lines.push(format!("totalTimePlayed={}", instance.play_time_seconds));
    lines.push(String::new());
    lines.push("[MultiMC]".to_string());
    lines.push("autoCloseMinecraft=false".to_string());

    std::fs::write(&cfg_path, lines.join("\n"))?;
    Ok(())
}

/// Write instance icon as pack.png in the instance root directory
pub fn write_pack_png(instances_dir: &PathBuf, instance_name: &str, icon_data: &str) {
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
        if line.starts_with('[') || line.starts_with('#') || line.is_empty() { continue; }
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim();
            let val = line[eq + 1..].trim();
            match key {
                "name" => name = val.to_string(),
                _ => {}
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
/// Returns (mc_version, loader, loader_version).
fn parse_prism_mmc_pack(content: &str) -> (String, LoaderType, Option<String>) {
    let mut mc_version = String::new();
    let mut loader = LoaderType::Vanilla;
    let mut loader_version: Option<String> = None;

    let Ok(pack) = serde_json::from_str::<serde_json::Value>(content) else {
        return (mc_version, loader, loader_version);
    };
    let Some(components) = pack["components"].as_array() else {
        return (mc_version, loader, loader_version);
    };

    for comp in components {
        let uid = comp["uid"].as_str().unwrap_or("");
        let ver = comp["version"].as_str().unwrap_or("");
        if comp["dependencyOnly"].as_bool().unwrap_or(false) {
            continue;
        }
        match uid {
            "net.minecraft" | "org.multimc.minecraft" | "org.prismlauncher.minecraft" => {
                if !ver.is_empty() {
                    mc_version = ver.to_string();
                }
            }
            "net.fabricmc.fabric-loader" | "org.quiltmc.quilt-loader" => {
                loader = LoaderType::Fabric;
                loader_version = Some(ver.to_string());
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

    (mc_version, loader, loader_version)
}

/// Import a Prism Launcher instance.zip into VoidLauncher instances dir.
///
/// Prism/MultiMC store the game directory as `minecraft/` (no dot) and keep
/// metadata in `instance.cfg` + `mmc-pack.json`. We map the game directory to
/// our canonical `.minecraft/` and translate the loader components, so mods,
/// resourcepacks, shaderpacks, configs and saves land where our launcher
/// expects them. Per-launch junk (.bobby cache, .mixin.out, crash-reports,
/// logs) and Prism metadata files are not carried over.
pub fn import_prism_pack(instances_dir: &PathBuf, zip_path: &str) -> Result<Instance> {
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
            let mut entry = archive.by_index(i).map_err(|e| LauncherError::Instance(e.to_string()))?;
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

        let name = cfg_content.lines()
            .find_map(|line| {
                let line = line.trim();
                if line.starts_with("name=") { Some(line[5..].to_string()) } else { None }
            })
            .unwrap_or_else(|| {
                zip_path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("imported")
                    .to_string()
            });
        let (mc_version, loader, loader_version) = parse_prism_mmc_pack(&pack_content);
        (name, mc_version, loader, loader_version)
    };

    // Validate the extracted name
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() < 3 || trimmed.chars().count() > 64
        || trimmed.contains("..") || trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0')
        || trimmed.contains('<') || trimmed.contains('>') || trimmed.contains(':') || trimmed.contains('"')
        || trimmed.contains('|') || trimmed.contains('?') || trimmed.contains('*')
        || trimmed.chars().any(|c| c.is_control())
        || trimmed.starts_with(' ') || trimmed.starts_with('.')
    {
        return Err(LauncherError::Instance(
            format!("Invalid instance name in Prism pack: '{}'", name)
        ));
    }
    let windows_reserved = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4",
        "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4",
        "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
    if windows_reserved.iter().any(|r| r.eq_ignore_ascii_case(trimmed)) {
        return Err(LauncherError::Instance(
            format!("Invalid instance name in Prism pack: '{}'", name)
        ));
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
            let mut entry = archive.by_index(i)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            if entry.is_dir() { continue; }

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

            let relative: Option<String> = if let Some(rel) = normalised.strip_prefix("minecraft/") {
                if is_junk(rel) { None } else { Some(format!(".minecraft/{}", rel)) }
            } else if let Some(rel) = normalised.strip_prefix(".minecraft/") {
                if is_junk(rel) { None } else { Some(format!(".minecraft/{}", rel)) }
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
            if icon.is_some() { break; }
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

/// Export an instance as a .zip archive (compatible with Prism/MultiMC format)
pub fn export_instance(instances_dir: &PathBuf, name: &str, output_path: &str) -> Result<()> {
    use zip::write::SimpleFileOptions;

    let instance = get_instance(instances_dir, name)?;
    let instance_dir = instance.dir(instances_dir);
    if !instance_dir.exists() {
        return Err(LauncherError::Instance(format!("Instance '{}' not found", name)));
    }

    let out_path = std::path::Path::new(output_path);
    let file = std::fs::File::create(out_path)?;
    let mut zip_w = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    fn add_dir_to_zip(
        zip: &mut zip::ZipWriter<std::fs::File>,
        dir: &std::path::Path,
        base_prefix: &str,
        options: SimpleFileOptions,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let zip_path = if base_prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", base_prefix, name)
            };

            if path.is_dir() {
                zip.add_directory(&zip_path, options)?;
                add_dir_to_zip(zip, &path, &zip_path, options)?;
            } else {
                let mut file = std::fs::File::open(&path)?;
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)?;
                zip.start_file(&zip_path, options)?;
                std::io::Write::write_all(zip, &buf)?;
            }
        }
        Ok(())
    }

    add_dir_to_zip(&mut zip_w, &instance_dir, "", options)?;
    zip_w.finish()?;

    tracing::info!(target: "launcher", "Exported instance '{}' to {:?}", name, out_path);
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
    r.read_exact(&mut len_buf).map_err(|e| LauncherError::Instance(e.to_string()))?;
    let len = u16::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).map_err(|e| LauncherError::Instance(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| LauncherError::Instance(format!("Invalid UTF-8 in NBT: {e}")))
}

fn nbt_skip(r: &mut impl Read, tag: u8) -> Result<()> {
    match tag {
        0 => Ok(()),
        1 => { let mut b = [0u8; 1]; r.read_exact(&mut b).map_err(|e| LauncherError::Instance(e.to_string()))?; Ok(()) }
        2 => { let mut b = [0u8; 2]; r.read_exact(&mut b).map_err(|e| LauncherError::Instance(e.to_string()))?; Ok(()) }
        3 => { let mut b = [0u8; 4]; r.read_exact(&mut b).map_err(|e| LauncherError::Instance(e.to_string()))?; Ok(()) }
        4 => { let mut b = [0u8; 8]; r.read_exact(&mut b).map_err(|e| LauncherError::Instance(e.to_string()))?; Ok(()) }
        5 => { let mut b = [0u8; 4]; r.read_exact(&mut b).map_err(|e| LauncherError::Instance(e.to_string()))?; Ok(()) }
        6 => { let mut b = [0u8; 8]; r.read_exact(&mut b).map_err(|e| LauncherError::Instance(e.to_string()))?; Ok(()) }
        7 | 11 | 12 => {
            let mut len_buf = [0u8; 4];
            r.read_exact(&mut len_buf).map_err(|e| LauncherError::Instance(e.to_string()))?;
            let elem_size: usize = match tag { 7 => 1, 11 => 4, 12 => 8, _ => 1 };
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut skip = vec![0u8; len * elem_size];
            r.read_exact(&mut skip).map_err(|e| LauncherError::Instance(e.to_string()))?;
            Ok(())
        }
        8 => { nbt_read_string(r)?; Ok(()) }
        9 => {
            let mut elem_type = [0u8; 1];
            r.read_exact(&mut elem_type).map_err(|e| LauncherError::Instance(e.to_string()))?;
            let mut len_buf = [0u8; 4];
            r.read_exact(&mut len_buf).map_err(|e| LauncherError::Instance(e.to_string()))?;
            let len = u32::from_be_bytes(len_buf) as usize;
            for _ in 0..len { nbt_skip(r, elem_type[0])?; }
            Ok(())
        }
        10 => {
            loop {
                let mut t = [0u8; 1];
                r.read_exact(&mut t).map_err(|e| LauncherError::Instance(e.to_string()))?;
                if t[0] == 0 { break; }
                nbt_read_string(r)?;
                nbt_skip(r, t[0])?;
            }
            Ok(())
        }
        _ => Err(LauncherError::Instance(format!("Unknown NBT tag type: {tag}"))),
    }
}

fn nbt_parse_compound_fields(r: &mut impl Read, data: &mut LevelDatData) -> Result<()> {
    loop {
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag).map_err(|e| LauncherError::Instance(e.to_string()))?;
        if tag[0] == 0 { break; }
        let name = nbt_read_string(r)?;
        match tag[0] {
            3 => {
                let mut buf = [0u8; 4];
                r.read_exact(&mut buf).map_err(|e| LauncherError::Instance(e.to_string()))?;
                let val = i32::from_be_bytes(buf);
                if name == "GameType" { data.game_type = Some(val); }
            }
            4 => {
                let mut buf = [0u8; 8];
                r.read_exact(&mut buf).map_err(|e| LauncherError::Instance(e.to_string()))?;
                let val = i64::from_be_bytes(buf);
                match name.as_str() {
                    "Seed" => data.seed = Some(val),
                    "LastPlayed" => data.last_played = Some(val),
                    _ => {}
                }
            }
            8 => {
                let val = nbt_read_string(r)?;
                if name == "LevelName" { data.level_name = Some(val); }
            }
            10 => {
                if name == "Data" {
                    nbt_parse_compound_fields(r, data)?;
                } else {
                    nbt_skip(r, 10)?;
                }
            }
            _ => { nbt_skip(r, tag[0])?; }
        }
    }
    Ok(())
}

/// Parse a GZip-compressed Minecraft level.dat and extract key fields.
pub fn parse_level_dat(path: &std::path::Path) -> LevelDatData {
    let Ok(file) = std::fs::File::open(path) else { return LevelDatData::default() };
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

pub fn rename_world(instances_dir: &PathBuf, instance_name: &str, old_name: &str, new_name: &str) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let from = saves_dir.join(old_name);
    let to = saves_dir.join(new_name);
    if !from.exists() {
        return Err(LauncherError::Instance(format!("World '{}' not found", old_name)));
    }
    if to.exists() {
        return Err(LauncherError::Instance(format!("A world named '{}' already exists", new_name)));
    }
    std::fs::rename(&from, &to).map_err(|e| LauncherError::Instance(e.to_string()))
}

pub fn copy_world(instances_dir: &PathBuf, instance_name: &str, world_name: &str, new_name: &str) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let src = saves_dir.join(world_name);
    let dst = saves_dir.join(new_name);
    if !src.exists() {
        return Err(LauncherError::Instance(format!("World '{}' not found", world_name)));
    }
    if dst.exists() {
        return Err(LauncherError::Instance(format!("A world named '{}' already exists", new_name)));
    }
    copy_dir_recursive(&src, &dst)
}

pub fn delete_world(instances_dir: &PathBuf, instance_name: &str, world_name: &str) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let world_dir = saves_dir.join(world_name);
    if !world_dir.exists() {
        return Err(LauncherError::Instance(format!("World '{}' not found", world_name)));
    }
    std::fs::remove_dir_all(&world_dir).map_err(|e| LauncherError::Instance(e.to_string()))
}

pub fn read_world_icon(instances_dir: &PathBuf, instance_name: &str, world_name: &str) -> Option<String> {
    let instance = get_instance(instances_dir, instance_name).ok()?;
    let icon_path = instance.minecraft_dir(instances_dir)
        .join("saves").join(world_name).join("icon.png");
    if !icon_path.exists() { return None; }
    read_image_as_base64(&icon_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn build_prism_zip(path: &Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

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
            ("minecraft/resourcepacks/rp.zip", b"PK\x03\x04fakerp".to_vec()),
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
        assert!(mc_dir.join("mods/testmod.jar").exists(), "mods mapped to .minecraft/mods");
        assert!(mc_dir.join("resourcepacks/rp.zip").exists());
        assert!(mc_dir.join("shaderpacks/sh.zip").exists());
        assert!(mc_dir.join("config/x.yml").exists());
        assert!(!mc_dir.join(".bobby").exists(), "launch junk skipped");
        assert!(!mc_dir.join(".mixin.out").exists(), "mixin dump skipped");
        assert!(!mc_dir.join("crash-reports").exists(), "crash reports skipped");
        assert!(!instances_dir.join("Test Pack").join("patches").exists(), "patches skipped");
        assert!(instances_dir.join("Test Pack").join("instance.json").exists(), "instance.json written");

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

        let emf = index.get("entity_model_features-3.2.6-26.2-fabric.jar").unwrap();
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
        assert!(instances_dir.join("Old Pack").join(".minecraft/mods/old.jar").exists());

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
        assert_eq!(sidecar_meta_path(&mods, "foo.jar"), mods.join(".index/foo.voidlauncher.json"));

        // .disabled file resolves to the same stem
        let meta = read_sidecar_meta(&mods, "foo.jar.disabled").unwrap();
        assert_eq!(meta["project_id"], "abc");

        // Legacy layout (file next to the content) still works as fallback
        std::fs::write(mods.join("bar.voidlauncher.json"), r#"{"provider":"curseforge"}"#).unwrap();
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

        let shaders = instances_dir.join("P").join(".minecraft").join("shaderpacks");
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
        assert_eq!(extract_version_from_filename("SE Vanilla Consistency 1.20.1.zip"), "1.20.1");
        assert_eq!(extract_version_from_filename("Aether Regenerated v1.3.1.zip"), "1.3.1");
        assert_eq!(extract_version_from_filename("recipeessentials-1.20.1-4.7.jar"), "4.7");
        assert_eq!(extract_version_from_filename("Ping-Wheel-1.12.1-forge-1.20.1.jar"), "1.20.1");
        assert_eq!(extract_version_from_filename("mod_v5.8.1.jar"), "5.8.1");
        assert_eq!(extract_version_from_filename("Hearths v1.0.5.mod.jar"), "1.0.5");
        assert_eq!(extract_version_from_filename("twilightforest-1.20.1-4.3.2508-universal.jar"), "4.3.2508");
        assert_eq!(extract_version_from_filename("aether-1.20.1-1.5.2-neoforge.jar"), "1.5.2");
        assert_eq!(extract_version_from_filename("Patchouli-1.20.1-85-FORGE.jar"), "85");
        assert_eq!(extract_version_from_filename("TerraBlender-forge-1.20.1-3.0.1.10.jar"), "3.0.1.10");
        assert_eq!(extract_version_from_filename("kotlinforforge-4.12.0-all.jar"), "4.12.0");
        assert_eq!(extract_version_from_filename("ConnectorExtras-1.11.2+1.20.1.jar"), "1.11.2+1.20.1");
        assert_eq!(extract_version_from_filename("Connector-1.0.0-beta.49+1.20.1.jar"), "1.0.0-beta.49+1.20.1");
        assert_eq!(extract_version_from_filename("curios-forge-5.14.1+1.20.1.jar"), "5.14.1+1.20.1");
        assert_eq!(extract_version_from_filename("CataclysmCompat1.0.zip"), "1.0");
        assert_eq!(extract_version_from_filename("BoP x FD Bark Cutting Compat.zip"), "");
        assert_eq!(extract_version_from_filename("Better_Modded_GUI.zip"), "");
        assert_eq!(extract_version_from_filename("NoBushyLeaves.zip"), "");
        assert_eq!(extract_version_from_filename("fresh_waystones.zip"), "");
        assert_eq!(extract_version_from_filename("BetterBetterX-v1.1-noStone.zip"), "");
        assert_eq!(extract_version_from_filename("Geophilic v3.6.mod.jar"), "3.6");
        assert_eq!(extract_version_from_filename("LessStructures-SpacingTweaks-1.20.1-2.1.56.zip"), "2.1.56");
        assert_eq!(extract_version_from_filename("Modded Omelet [120] [162].zip"), "");
    }
}

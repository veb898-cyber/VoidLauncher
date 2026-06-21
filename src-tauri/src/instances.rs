use serde::{Deserialize, Serialize};
use crate::error::{LauncherError, Result};
use crate::modloaders::LoaderProfile;
use std::path::{Path, PathBuf};
use std::io::Read;
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
    Quilt,
    Forge,
    NeoForge,
    LiteLoader,
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
    ) -> Self {
        Self {
            name: name.to_string(),
            mc_version: mc_version.to_string(),
            loader: LoaderType::Vanilla,
            loader_version: None,
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
    save_instance(instances_dir, instance)?;

    Ok(())
}

/// Save instance config to disk, plus Prism-compatible instance.cfg and pack.png
pub fn save_instance(instances_dir: &PathBuf, instance: &Instance) -> Result<()> {
    let config_path = instance.config_file(instances_dir);
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
    save_instance(instances_dir, &instance)?;
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
    save_instance(instances_dir, &instance)?;
    Ok(())
}

/// Save instance banner (base64 data URL or gradient:name, empty to remove)
pub fn save_instance_banner(instances_dir: &PathBuf, name: &str, banner_data: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.banner = if banner_data.is_empty() { None } else { Some(banner_data.to_string()) };
    save_instance(instances_dir, &instance)?;
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
        // Skip hidden files and sidecar metadata
        if filename.starts_with('.') || filename.ends_with(".voidlauncher.json") { continue; }
        let is_dir = path.is_dir();
        let meta = std::fs::metadata(&path)?;

        // Skip tiny non-zip config presets in shaderpacks (e.g. "Better MC - High.json" at 373 bytes)
        if pack_type == "shaderpacks" && !is_dir && meta.len() < 1024 {
            let lower = filename.to_lowercase();
            if lower.ends_with(".json") || lower.ends_with(".txt") || lower.ends_with(".cfg") || lower.ends_with(".properties") {
                continue;
            }
        }

        // Read sidecar metadata (project_name is the Modrinth/CurseForge display name)
        let (provider, version, project_id, project_name) = read_pack_sidecar(&path).unwrap_or_default();
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

fn read_pack_sidecar(pack_path: &std::path::Path) -> Option<(String, String, String, String)> {
    let filename = pack_path.file_name()?.to_string_lossy().to_string();
    // Handle .disabled suffix: if file is foo.jar.disabled, look for both
    // foo.jar.disabled.voidlauncher.json and foo.jar.voidlauncher.json
    let meta_disabled = format!("{}.voidlauncher.json", filename);
    let meta_filename = if let Some(stripped) = filename.strip_suffix(".disabled") {
        let meta_original = format!("{}.voidlauncher.json", stripped);
        let meta_path = pack_path.parent()?.join(&meta_original);
        if meta_path.exists() { meta_original } else { meta_disabled }
    } else {
        meta_disabled
    };
    let meta_path = pack_path.parent()?.join(&meta_filename);
    let contents = std::fs::read_to_string(&meta_path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let provider = val["provider"].as_str().unwrap_or("").to_string();
    let version = val["version_number"].as_str().unwrap_or("").to_string();
    let project_id = val["project_id"].as_str().unwrap_or("").to_string();
    let project_name = val["project_name"].as_str().unwrap_or("").to_string();
    Some((provider, version, project_id, project_name))
}

/// Strip Minecraft color/formatting codes (§a, §l, §r, etc.) and any underscores used as spaces
fn strip_minecraft_color_codes(s: &str) -> String {
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

    // Recursively scan for any image file (shader packs often have screenshots/ in subdirs)
    let exts = [".png", ".jpg", ".jpeg"];
    for entry in std::fs::read_dir(path).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(img) = read_pack_icon_from_dir(&path) {
                return Some(img);
            }
        } else {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if exts.iter().any(|e| name.ends_with(e)) {
                if let Some(img) = read_image_as_base64(&path) {
                    return Some(img);
                }
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

    let exts_img = [".png", ".jpg", ".jpeg"];
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

    // Pass 2: pack.png in any subdir, then fallback to first image
    let mut fallback_name: Option<String> = None;
    for (name, is_dir) in &entries {
        if *is_dir { continue; }
        let lower = name.to_lowercase();

        if lower.ends_with("pack.png") {
            if let Some(img) = try_read_zip_image(&mut archive, name) {
                return Some(img);
            }
        } else if fallback_name.is_none() && exts_img.iter().any(|e| lower.ends_with(e)) {
            fallback_name = Some(name.clone());
        }
    }

    if let Some(ref name) = fallback_name {
        if let Some(img) = try_read_zip_image(&mut archive, name) {
            return Some(img);
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
    save_instance(instances_dir, &instance)?;
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

/// Import a Prism Launcher instance.zip into VoidLauncher instances dir
pub fn import_prism_pack(instances_dir: &PathBuf, zip_path: &str) -> Result<Instance> {
    let zip_path = std::path::Path::new(zip_path);
    if !zip_path.exists() {
        return Err(LauncherError::Instance(format!(
            "ZIP file not found: {}",
            zip_path.display()
        )));
    }

    // Read the ZIP in one pass: detect instance.cfg, read name, then extract
    let zip_bytes = std::fs::read(zip_path)?;
    let name = {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes))
            .map_err(|e| LauncherError::Instance(format!("Invalid ZIP: {}", e)))?;

        let mut found = false;
        let mut cfg_content = String::new();

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| LauncherError::Instance(e.to_string()))?;
            if entry.name() == "instance.cfg" {
                found = true;
                entry.read_to_string(&mut cfg_content).ok();
                break;
            }
        }

        if !found {
            return Err(LauncherError::Instance(
                "Not a valid Prism instance pack: missing instance.cfg".to_string(),
            ));
        }

        cfg_content.lines()
            .find_map(|line| {
                let line = line.trim();
                if line.starts_with("name=") { Some(line[5..].to_string()) } else { None }
            })
            .unwrap_or_else(|| {
                zip_path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("imported")
                    .to_string()
            })
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

    // Extract all files from the ZIP to the target directory
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

            let out_path = target_dir.join(&entry_name);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out_file = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out_file)?;
        }
    }

    // Parse the extracted instance.cfg and save as instance.json
    let extracted_cfg = target_dir.join("instance.cfg");
    let instance = parse_prism_cfg(&extracted_cfg).ok_or_else(|| {
        LauncherError::Instance("Failed to parse imported instance.cfg".to_string())
    })?;

    // Write instance.json
    let json_path = target_dir.join("instance.json");
    let json = serde_json::to_string_pretty(&instance)?;
    std::fs::write(&json_path, json)?;

    tracing::info!(target: "launcher", "Imported Prism pack '{}' from {:?}", name, zip_path);
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

use serde::{Deserialize, Serialize};
use crate::error::{LauncherError, Result};
use crate::modloaders::LoaderProfile;
use std::path::{Path, PathBuf};
use std::io::Read;
use chrono::Utc;

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
}

/// List all instances
pub fn list_instances(instances_dir: &PathBuf) -> Result<Vec<Instance>> {
    let mut instances = Vec::new();

    if !instances_dir.exists() {
        return Ok(instances);
    }

    for entry in std::fs::read_dir(instances_dir)? {
        let entry = entry?;
        let config_path = entry.path().join("instance.json");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(contents) => match serde_json::from_str::<Instance>(&contents) {
                    Ok(instance) => instances.push(instance),
                    Err(e) => {
                        eprintln!("Failed to parse instance at {:?}: {}", config_path, e)
                    }
                },
                Err(e) => eprintln!("Failed to read instance at {:?}: {}", config_path, e),
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

/// Save instance config to disk
pub fn save_instance(instances_dir: &PathBuf, instance: &Instance) -> Result<()> {
    let config_path = instance.config_file(instances_dir);
    let json = serde_json::to_string_pretty(instance)?;
    std::fs::write(config_path, json)?;
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

/// Get an instance by name
pub fn get_instance(instances_dir: &PathBuf, name: &str) -> Result<Instance> {
    let config_path = instances_dir.join(name).join("instance.json");
    if !config_path.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    let contents = std::fs::read_to_string(&config_path)?;
    let instance = serde_json::from_str(&contents)?;
    Ok(instance)
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
            saves.push(SaveEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                last_modified: std::fs::metadata(&path).ok().and_then(|m| m.modified().ok())
                    .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64),
                size_bytes: dir_size(&path),
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
        let name = if is_dir { read_pack_name_from_dir(&path) } else { read_pack_name_from_zip(&path) }
            .unwrap_or_else(|| {
                let stem = Path::new(&filename).file_stem().and_then(|s| s.to_str()).unwrap_or(&filename);
                let stem = stem.strip_suffix(".disabled").unwrap_or(stem);
                strip_minecraft_color_codes(stem)
            });
        // Read sidecar metadata
        let (provider, version, project_id) = read_pack_sidecar(&path).unwrap_or_default();
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

fn read_pack_sidecar(pack_path: &std::path::Path) -> Option<(String, String, String)> {
    let filename = pack_path.file_name()?.to_string_lossy().to_string();
    let meta_filename = format!("{}.voidlauncher.json", filename);
    let meta_path = pack_path.parent()?.join(&meta_filename);
    let contents = std::fs::read_to_string(&meta_path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let provider = val["provider"].as_str().unwrap_or("").to_string();
    let version = val["version_number"].as_str().unwrap_or("").to_string();
    let project_id = val["project_id"].as_str().unwrap_or("").to_string();
    Some((provider, version, project_id))
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
    let icon_path = path.join("pack.png");
    if icon_path.exists() { read_image_as_base64(&icon_path) } else { None }
}

fn read_pack_icon_from_zip(path: &std::path::Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader).ok()?;

    // Try common icon filenames first (in root)
    let candidates_root = ["pack.png", "pack.jpg", "pack.jpeg", "preview.png", "thumb.png", "icon.png", "logo.png"];
    for name in &candidates_root {
        if let Some(img) = try_read_zip_image(&mut archive, name) {
            return Some(img);
        }
    }
    // Try "pack.png" in any subdir
    let pack_candidates: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .filter(|n| n.to_lowercase().ends_with("pack.png"))
        .collect();
    for name in &pack_candidates {
        if let Some(img) = try_read_zip_image(&mut archive, name) {
            return Some(img);
        }
    }

    // Fallback: find any image in the zip
    let exts = [".png", ".jpg", ".jpeg"];
    let image_candidates: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            archive.by_index(i).ok().and_then(|e| {
                if !e.is_dir() {
                    let n = e.name();
                    let lower = n.to_lowercase();
                    if exts.iter().any(|e| lower.ends_with(e)) {
                        Some(n.to_string())
                    } else { None }
                } else { None }
            })
        })
        .collect();
    for name in &image_candidates {
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
    let desc = &json["pack"]["description"];
    let raw = if let Some(s) = desc.as_str() {
        Some(s.to_string())
    } else if let Some(obj) = desc.as_object() {
        obj.get("text").and_then(|v| v.as_str()).map(|s| s.to_string())
    } else if let Some(arr) = desc.as_array() {
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
    }?;

    let stripped = strip_minecraft_color_codes(&raw);
    // Strip common HTML tags used in descriptions
    let clean = stripped.replace("<br>", " ").replace("<br/>", " ").replace("</br>", " ");
    let clean = clean.trim().to_string();
    if clean.is_empty() { None } else { Some(clean) }
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

/// Update instance's last played timestamp
pub fn update_last_played(instances_dir: &PathBuf, name: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.last_played = Some(Utc::now().to_rfc3339());
    save_instance(instances_dir, &instance)?;
    Ok(())
}

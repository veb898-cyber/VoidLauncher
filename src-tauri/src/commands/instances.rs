//! Instance management commands: CRUD on instances, worlds, screenshots,
//! resource packs, modpack import/export, and the pack-folder watcher.

use crate::events;
use crate::instances;
use crate::playtime;
use crate::AppState;
use tauri::{AppHandle, Emitter, State};

/// Maximum size for a Base64-encoded instance icon.
/// 3 MB of Base64 ≈ 2.2 MB decoded — enough for any reasonable image.
const MAX_ICON_DATA_BYTES: usize = 9 * 1024 * 1024;
const MAX_BANNER_DATA_BYTES: usize = 18 * 1024 * 1024;

/// Validate an instance name. Used by every Tauri command that takes an
/// `instance_name` parameter, to prevent path-traversal attacks where a
/// malicious frontend sends `..` or other escape characters that would
/// otherwise be joined onto `instances_dir` and traverse the filesystem.
pub(crate) fn validate_instance_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Instance name is required.".to_string());
    }
    if trimmed.chars().count() < 3 {
        return Err("Instance name is too short (min 3 characters).".to_string());
    }
    if trimmed.chars().count() > 64 {
        return Err("Instance name is too long (max 64 characters).".to_string());
    }
    if trimmed.starts_with('.') {
        return Err("Instance name may not start with a dot.".to_string());
    }
    validate_safe_folder_name(trimmed, "Instance name")
}

/// Validates a raw folder name for path safety only (no length/leading-dot
/// constraints). Used for Minecraft world/save names, which are free-form:
/// a single character is valid, and names may legitimately start with a dot.
/// The only enforced rules are the ones that keep the name from escaping the
/// containing directory (traversal, separators, NUL) or from breaking on
/// Windows (control chars, `<>:"|?*`, reserved device names).
fn validate_safe_folder_name(trimmed: &str, what: &str) -> Result<(), String> {
    if trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
    {
        return Err(format!("{} contains invalid path characters.", what));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(format!("Invalid {}: '{}'.", what, trimmed));
    }
    // Reject Windows reserved device names (CON, PRN, AUX, NUL, COM1..9, LPT1..9)
    let upper = trimmed.to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.iter().any(|r| *r == upper.as_str()) {
        return Err(format!("'{}' is a reserved name.", trimmed));
    }
    // Allow any printable Unicode character (including Cyrillic, CJK, emoji)
    // — the folder can be called whatever the user wants. The only thing we
    // block here are control characters and characters that would break path
    // joining on any major platform.
    for ch in trimmed.chars() {
        if ch.is_control() {
            return Err(format!(
                "{} contains a control character (U+{:04X}).",
                what,
                ch as u32
            ));
        }
        // Reject the few characters that are special on Windows filenames
        // even when escaped: < > : " | ? *
        if matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            return Err(format!(
                "{} contains an invalid character: {:?}",
                what, ch
            ));
        }
    }
    Ok(())
}

/// Path-safety validation for free-form world/save folder names.
pub(crate) fn validate_world_name(name: &str) -> Result<(), String> {
    validate_safe_folder_name(name.trim(), "World name")
}

// ==================== Instance Commands ====================

#[tauri::command]
pub fn cmd_list_instances(state: State<'_, AppState>) -> Result<Vec<instances::Instance>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut insts =
        instances::list_instances(&config.instances_dir()).map_err(|e| e.to_string())?;
    // Merge playtime from playtime.json into each instance
    let playtime_map = playtime::load_playtime(&config.data_dir);
    for inst in &mut insts {
        if let Some(entry) = playtime_map.get(&inst.name) {
            inst.play_time_seconds = entry.minutes * 60;
        }
    }
    Ok(insts)
}

#[tauri::command]
pub fn cmd_create_instance(
    state: State<'_, AppState>,
    name: String,
    mc_version: String,
    loader: Option<instances::LoaderType>,
    loader_version: Option<String>,
) -> Result<instances::Instance, String> {
    validate_instance_name(&name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let loader = loader.unwrap_or(instances::LoaderType::Vanilla);
    let instance = instances::Instance::new(
        &name,
        &mc_version,
        config.default_memory_mb,
        &config.default_gc_preset,
        loader.clone(),
        if loader == instances::LoaderType::Vanilla { None } else { loader_version },
    );
    instances::create_instance(&config.instances_dir(), &instance).map_err(|e| e.to_string())?;
    Ok(instance)
}

#[tauri::command]
pub fn cmd_delete_instance(state: State<'_, AppState>, name: String) -> Result<(), String> {
    validate_instance_name(&name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::delete_instance(&config.instances_dir(), &name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_get_instance(
    state: State<'_, AppState>,
    name: String,
) -> Result<instances::Instance, String> {
    validate_instance_name(&name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut inst =
        instances::get_instance(&config.instances_dir(), &name).map_err(|e| e.to_string())?;
    // Merge playtime from playtime.json
    let playtime_map = playtime::load_playtime(&config.data_dir);
    if let Some(entry) = playtime_map.get(&inst.name) {
        inst.play_time_seconds = entry.minutes * 60;
    }
    Ok(inst)
}

#[tauri::command]
pub fn cmd_save_instance(
    state: State<'_, AppState>,
    instance: instances::Instance,
    old_name: Option<String>,
) -> Result<(), String> {
    validate_instance_name(&instance.name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::save_instance(&config.instances_dir(), &instance, old_name.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_duplicate_instance(
    state: State<'_, AppState>,
    name: String,
    new_name: String,
) -> Result<instances::Instance, String> {
    validate_instance_name(&name)?;
    validate_instance_name(&new_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::duplicate_instance(&config.instances_dir(), &name, &new_name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_import_prism_instance(
    state: State<'_, AppState>,
    zip_path: String,
) -> Result<instances::Instance, String> {
    let zip = std::path::PathBuf::from(&zip_path);
    if !zip.exists() || !zip.is_file() {
        return Err("Invalid zip path".to_string());
    }
    if !zip.extension().map_or(false, |e| e == "zip") {
        return Err("File must be a .zip archive".to_string());
    }
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::import_prism_pack(&config.instances_dir(), &zip_path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_export_instance(
    state: State<'_, AppState>,
    name: String,
    output_path: String,
) -> Result<(), String> {
    validate_instance_name(&name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::export_instance(&config.instances_dir(), &name, &output_path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_probe_modpack(path: String) -> Result<crate::import::ModpackMetadata, String> {
    if !std::path::PathBuf::from(&path).exists() {
        return Err("File not found".to_string());
    }
    crate::import::probe_modpack(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_import_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    instance_name: String,
) -> Result<instances::Instance, String> {
    validate_instance_name(&instance_name)?;
    if !std::path::PathBuf::from(&path).exists() {
        return Err("File not found".to_string());
    }
    let (instances_dir, curseforge_api_key, libraries_dir) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        (
            config.instances_dir().clone(),
            config.curseforge_api_key.clone(),
            config.libraries_dir(),
        )
    };
    crate::import::import_modpack(
        &instances_dir,
        &path,
        &instance_name,
        &curseforge_api_key,
        &libraries_dir,
        Some(&app),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_set_instance_icon(
    state: State<'_, AppState>,
    instance_name: String,
    icon_data: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    if icon_data.len() > MAX_ICON_DATA_BYTES {
        return Err(format!(
            "Icon is too large ({} KB). Maximum allowed size is 2 MB.",
            icon_data.len() / 1024
        ));
    }
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::save_instance_icon(&config.instances_dir(), &instance_name, &icon_data)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_set_instance_banner(
    state: State<'_, AppState>,
    instance_name: String,
    banner_data: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    if banner_data.len() > MAX_BANNER_DATA_BYTES {
        return Err(format!(
            "Banner is too large ({} KB). Maximum allowed size is 4 MB.",
            banner_data.len() / 1024
        ));
    }
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::save_instance_banner(&config.instances_dir(), &instance_name, &banner_data)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_log_toast(app: AppHandle, level: String, message: String) {
    let log_level = match level.as_str() {
        "error" => "error",
        "warning" => "warn",
        _ => "info",
    };
    events::emit_log(&app, log_level, "toast", &message);
}

#[tauri::command]
pub fn cmd_list_saves(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<instances::SaveEntry>, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::list_saves(&config.instances_dir(), &instance_name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_rename_world(
    state: State<'_, AppState>,
    instance_name: String,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    validate_world_name(&old_name)?;
    validate_world_name(&new_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::rename_world(
        &config.instances_dir(),
        &instance_name,
        &old_name,
        &new_name,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_copy_world(
    state: State<'_, AppState>,
    instance_name: String,
    world_name: String,
    new_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    validate_world_name(&world_name)?;
    validate_world_name(&new_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::copy_world(
        &config.instances_dir(),
        &instance_name,
        &world_name,
        &new_name,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_delete_world(
    state: State<'_, AppState>,
    instance_name: String,
    world_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    validate_world_name(&world_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::delete_world(&config.instances_dir(), &instance_name, &world_name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_list_screenshots(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<instances::ScreenshotEntry>, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::list_screenshots(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_delete_screenshot(
    state: State<'_, AppState>,
    instance_name: String,
    filename: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let safe_filename = std::path::Path::new(&filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?
        .to_string();
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::delete_screenshot(&config.instances_dir(), &instance_name, &safe_filename)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_read_screenshot(
    state: State<'_, AppState>,
    instance_name: String,
    filename: String,
) -> Result<String, String> {
    validate_instance_name(&instance_name)?;
    let safe_filename = std::path::Path::new(&filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?
        .to_string();
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::read_screenshot(&config.instances_dir(), &instance_name, &safe_filename)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_list_packs(
    state: State<'_, AppState>,
    instance_name: String,
    pack_type: String,
) -> Result<Vec<instances::PackEntry>, String> {
    validate_instance_name(&instance_name)?;
    if !["mods", "resourcepacks", "shaderpacks", "config"].contains(&pack_type.as_str()) {
        return Err("Invalid pack_type".to_string());
    }
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::list_packs(&config.instances_dir(), &instance_name, &pack_type)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_get_pack_icon(
    state: State<'_, AppState>,
    instance_name: String,
    pack_type: String,
    filename: String,
) -> Result<Option<String>, String> {
    validate_instance_name(&instance_name)?;
    if !["mods", "resourcepacks", "shaderpacks", "config"].contains(&pack_type.as_str()) {
        return Err("Invalid pack_type".to_string());
    }
    let safe_filename = std::path::Path::new(&filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?
        .to_string();
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::read_pack_icon(
        &config.instances_dir(),
        &instance_name,
        &pack_type,
        &safe_filename,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_watch_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    use notify::RecursiveMode;
    use notify_debouncer_mini::new_debouncer;

    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let mc_dir = instance.minecraft_dir(&config.instances_dir());
    drop(config);

    // Watch each content subfolder
    let watch_paths: Vec<std::path::PathBuf> = ["mods", "resourcepacks", "shaderpacks"]
        .iter()
        .map(|s| mc_dir.join(s))
        .filter(|p| p.exists())
        .collect();

    let app_clone = app.clone();
    let instance_for_closure = instance_name.clone();
    let mut debouncer = new_debouncer(
        std::time::Duration::from_millis(300),
        move |res: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            if let Ok(events) = res {
                if events.is_empty() {
                    return;
                }
                let mut changed: Vec<String> = Vec::new();
                for ev in events {
                    let path_str = ev.path.to_string_lossy().to_string();
                    if path_str.contains("resourcepacks") {
                        changed.push("resourcepacks".to_string());
                    } else if path_str.contains("shaderpacks") {
                        changed.push("shaderpacks".to_string());
                    } else if path_str.contains("mods") {
                        changed.push("mods".to_string());
                    }
                }
                changed.sort();
                changed.dedup();
                for sub in changed {
                    let _ = app_clone.emit(
                        "instance_dir_changed",
                        serde_json::json!({
                            "instance": instance_for_closure.clone(),
                            "subfolder": sub,
                        }),
                    );
                }
            }
        },
    )
    .map_err(|e| e.to_string())?;

    for path in &watch_paths {
        debouncer
            .watcher()
            .watch(path, RecursiveMode::NonRecursive)
            .map_err(|e| e.to_string())?;
    }

    // Replace any existing watcher
    let mut watcher_slot = state.pack_watcher.lock().map_err(|e| e.to_string())?;
    *watcher_slot = Some(crate::PackWatcherHandle {
        instance_name: instance_name.clone(),
        _debouncer: debouncer,
    });

    Ok(())
}

#[tauri::command]
pub fn cmd_unwatch_instance(state: State<'_, AppState>) -> Result<(), String> {
    let mut watcher_slot = state.pack_watcher.lock().map_err(|e| e.to_string())?;
    *watcher_slot = None; // Drop the debouncer, stopping the watcher
    Ok(())
}

#[tauri::command]
pub fn cmd_open_instance_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
    subfolder: Option<String>,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    const ALLOWED: &[&str] = &[
        "mods",
        "resourcepacks",
        "shaderpacks",
        "config",
        "screenshots",
        "saves",
        "logs",
        "",
    ];
    let sub = subfolder
        .as_deref()
        .unwrap_or("")
        .trim_matches('/')
        .trim_matches('\\');
    if !ALLOWED.iter().any(|s| s.eq_ignore_ascii_case(sub)) {
        return Err(format!("Subfolder '{}' is not allowed.", sub));
    }
    if sub.contains("..") || sub.contains('\0') {
        return Err("Subfolder contains invalid characters.".to_string());
    }
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let dir = if sub.is_empty() {
        instance.dir(&config.instances_dir())
    } else {
        instance.minecraft_dir(&config.instances_dir()).join(sub)
    };
    let _ = std::fs::create_dir_all(&dir);
    let path_str = dir.to_string_lossy().to_string();
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(&path_str, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cmd_get_instance_dir(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<String, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    Ok(instance
        .minecraft_dir(&config.instances_dir())
        .to_string_lossy()
        .to_string())
}

#[tauri::command]
pub fn cmd_check_instance_installed(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<bool, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;

    let version_jar = config
        .versions_dir()
        .join(&instance.mc_version)
        .join("client.jar");
    // Fallback: check for old {version}.jar
    if !version_jar.exists() {
        let old_jar = config
            .versions_dir()
            .join(&instance.mc_version)
            .join(format!("{}.jar", instance.mc_version));
        if old_jar.exists() {
            std::fs::rename(&old_jar, &version_jar).ok();
        }
    }

    let version_json = config
        .versions_dir()
        .join(&instance.mc_version)
        .join(format!("{}.json", instance.mc_version));

    Ok(version_jar.exists() && version_json.exists())
}

#[cfg(test)]
mod instance_name_tests {
    use super::validate_instance_name;
    use super::validate_world_name;

    #[test]
    fn accepts_valid_names() {
        assert!(validate_instance_name("MyInstance").is_ok());
        assert!(validate_instance_name("survival-2024").is_ok());
        assert!(validate_instance_name("modded_1.20").is_ok());
        assert!(validate_instance_name("My World").is_ok());
    }

    #[test]
    fn accepts_unicode_names() {
        // Cyrillic
        assert!(validate_instance_name("Мой Мир").is_ok());
        assert!(validate_instance_name("Выживание").is_ok());
        // CJK
        assert!(validate_instance_name("我的世界").is_ok());
        // Emoji
        assert!(validate_instance_name("Craft ⛏").is_ok());
    }

    #[test]
    fn rejects_windows_special_chars() {
        assert!(validate_instance_name("foo<bar").is_err());
        assert!(validate_instance_name("foo>bar").is_err());
        assert!(validate_instance_name("foo:bar").is_err());
        assert!(validate_instance_name("foo\"bar").is_err());
        assert!(validate_instance_name("foo|bar").is_err());
        assert!(validate_instance_name("foo?bar").is_err());
        assert!(validate_instance_name("foo*bar").is_err());
    }

    #[test]
    fn rejects_control_chars() {
        assert!(validate_instance_name("foo\nbar").is_err());
        assert!(validate_instance_name("foo\tbar").is_err());
        assert!(validate_instance_name("foo\rbar").is_err());
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(validate_instance_name("..").is_err());
        assert!(validate_instance_name("../etc/passwd").is_err());
        assert!(validate_instance_name("a/..").is_err());
        assert!(validate_instance_name("a\\..").is_err());
        assert!(validate_instance_name("foo/bar").is_err());
        assert!(validate_instance_name("foo\\bar").is_err());
        assert!(validate_instance_name("foo\0bar").is_err());
    }

    #[test]
    fn rejects_dot_prefix() {
        assert!(validate_instance_name(".hidden").is_err());
        assert!(validate_instance_name(".").is_err());
    }

    #[test]
    fn rejects_reserved_windows_names() {
        assert!(validate_instance_name("CON").is_err());
        assert!(validate_instance_name("con").is_err());
        assert!(validate_instance_name("COM1").is_err());
        assert!(validate_instance_name("LPT9").is_err());
    }

    #[test]
    fn rejects_too_long() {
        let long = "a".repeat(65);
        assert!(validate_instance_name(&long).is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_instance_name("").is_err());
        assert!(validate_instance_name("   ").is_err());
    }

    #[test]
    fn world_names_allow_freeform_but_still_reject_traversal() {
        // World/save names are free-form: a single char and a leading dot are
        // both legitimate (unlike instance names which need 3+ and no dot).
        assert!(validate_world_name("A").is_ok());
        assert!(validate_world_name(".hidden").is_ok());
        assert!(validate_world_name("My World").is_ok());
        assert!(validate_world_name("1").is_ok());
        // Security invariants must still hold: never escape the saves dir.
        assert!(validate_world_name("..").is_err());
        assert!(validate_world_name(".").is_err());
        assert!(validate_world_name("../escape").is_err());
        assert!(validate_world_name("a\\..").is_err());
        assert!(validate_world_name("a/b").is_err());
        assert!(validate_world_name("a\\b").is_err());
        assert!(validate_world_name("a\0b").is_err());
        assert!(validate_world_name("foo:bar").is_err());
        assert!(validate_world_name("CON").is_err());
    }
}

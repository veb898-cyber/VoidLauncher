mod accounts;
mod auth;
mod config;
mod curseforge;
mod download;
mod error;
mod events;
mod instances;
mod java;
mod jvm;
mod launch;
mod logger;
mod modloaders;
mod modrinth;
mod playtime;
mod versions;

use config::AppConfig;
use events::{InstallProgressPayload, ProgressSender};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::io::Read as _;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

/// Global app state shared across all Tauri commands
pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub auth_state: Mutex<auth::AuthState>,
    pub running_instance_id: Mutex<Option<String>>,
    pub pack_watcher: Mutex<Option<PackWatcherHandle>>,
    /// Persistent icon cache: key (project_id / filename) → data URL or HTTPS URL
    pub icon_cache: Mutex<HashMap<String, String>>,
    /// Active playtime-tracking session, if a game is running
    pub active_session: Mutex<Option<playtime::ActiveSession>>,
}

/// Load the icon cache from disk; returns empty map on any error
fn load_icon_cache_from_disk(config: &AppConfig) -> HashMap<String, String> {
    let path = config.icon_cache_file();
    let Ok(contents) = std::fs::read_to_string(&path) else { return HashMap::new(); };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// Persist the icon cache to disk (best-effort; ignores errors)
fn save_icon_cache_to_disk(config: &AppConfig, cache: &HashMap<String, String>) {
    let _ = std::fs::create_dir_all(&config.data_dir);
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(config.icon_cache_file(), json);
    }
}

/// Handle to an active file system watcher; dropping stops the watcher
pub struct PackWatcherHandle {
    pub instance_name: String,
    _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
}

/// Returns true if the URL's host is in the allowlist of trusted
/// download mirrors (Modrinth, CurseForge, Mojang). Used by every
/// command that downloads a file from a URL supplied by the frontend
/// or by an upstream API, to prevent SSRF / file:// / mixed-content
/// downgrade attacks.
fn is_allowed_download_host(url: &str) -> bool {
    const ALLOWED: &[&str] = &[
        "cdn.modrinth.com",
        "github.com",
        "raw.githubusercontent.com",
        "edge.forgecdn.net",
        "mediafilez.forgecdn.net",
        "piston-data.mojang.com",
        "piston-meta.mojang.com",
        "launchermeta.mojang.com",
        "libraries.minecraft.net",
        "resources.download.minecraft.net",
        "maven.fabricmc.net",
        "maven.quiltmc.org",
        "files.minecraftforge.net",
        "maven.minecraftforge.net",
        "maven.neoforged.net",
    ];
    let url = url.trim();
    // Require https (no http, no file://, no javascript:, etc.)
    if !url.to_ascii_lowercase().starts_with("https://") {
        return false;
    }
    // Extract host: text between "https://" and the next "/", ":", "?", or "#".
    let after_scheme = &url[8..];
    let host_end = after_scheme
        .find(|c: char| c == '/' || c == ':' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let host = after_scheme[..host_end].to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    ALLOWED.iter().any(|h| host == *h || host.ends_with(&format!(".{}", h)))
}

// ==================== Auth Commands ====================

#[tauri::command]
async fn cmd_start_login(app: AppHandle, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    events::emit_log(&app, "info", "auth", "Starting Microsoft login flow...");
    let client_id = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.client_id.clone()
    };

    if client_id.is_empty() {
        return Err("Client ID not configured. Please set it in Settings.".into());
    }

    auth::start_device_code_flow(&client_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_poll_login(
    app: AppHandle,
    state: State<'_, AppState>,
    device_code: String,
) -> Result<auth::MinecraftProfile, String> {
    let client_id = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.client_id.clone()
    };

    let ms_token = auth::poll_device_code(&client_id, &device_code)
        .await
        .map_err(|e| e.to_string())?;

    let (mc_token, profile) = auth::full_auth_flow(&ms_token)
        .await
        .map_err(|e| e.to_string())?;

    // Save auth state
    {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let new_auth = auth::AuthState {
            microsoft_token: Some(ms_token),
            minecraft_token: Some(mc_token),
            profile: Some(profile.clone()),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            offline_mode: false,
        };
        auth::save_auth_state(&config.auth_file(), &new_auth).map_err(|e| e.to_string())?;
        let mut auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
        *auth_state = new_auth;
    }

    // Save Microsoft account entry to accounts.json
    {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let _ = accounts::upsert_microsoft_account(&config.data_dir, &profile.name, &profile.id);
    }

    events::emit_log(&app, "info", "auth", &format!("Login successful: {}", profile.name));
    Ok(profile)
}

#[tauri::command]
fn cmd_get_auth_state(state: State<'_, AppState>) -> Result<auth::AuthState, String> {
    let auth = state.auth_state.lock().map_err(|e| e.to_string())?;
    Ok(auth.clone())
}

/// Check if we can launch in offline mode using cached credentials
#[tauri::command]
fn cmd_can_launch_offline(state: State<'_, AppState>) -> Result<bool, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(auth::can_launch_offline(&config.auth_file()))
}

/// Get offline mode credentials (username and UUID) from cached auth state
#[tauri::command]
fn cmd_get_offline_credentials(state: State<'_, AppState>) -> Result<Option<(String, String)>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(auth::get_offline_credentials(&config.auth_file()))
}

#[tauri::command]
fn cmd_logout(state: State<'_, AppState>) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
    // Remove Microsoft account from accounts.json
    if let Some(ref profile) = auth_state.profile {
        let _ = accounts::remove_microsoft_account(&config.data_dir, &profile.id);
    }
    drop(auth_state);
    let mut auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
    *auth_state = auth::AuthState::default();
    let _ = std::fs::remove_file(config.auth_file());
    Ok(())
}

// ==================== Account Management Commands ====================

#[tauri::command]
fn cmd_list_accounts(state: State<'_, AppState>) -> Result<Vec<accounts::PublicAccountEntry>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    // Strip access_token / elby_token before crossing the bridge so secrets
    // never enter the renderer process. The launch flow reads tokens from
    // disk via accounts::list_accounts (which still returns AccountEntry).
    Ok(accounts::list_accounts(&config.data_dir)
        .into_iter()
        .map(accounts::PublicAccountEntry::from)
        .collect())
}

#[tauri::command]
fn cmd_add_offline_account(
    state: State<'_, AppState>,
    username: String,
) -> Result<Vec<accounts::AccountEntry>, String> {
    validate_offline_username(&username)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let entry = accounts::AccountEntry::new_offline(&username);
    accounts::add_account(&config.data_dir, entry)
}

/// Validate an offline-account username.
/// Rules:
///   * 3-16 characters
///   * only ASCII letters, digits, and underscores
///   * must NOT contain any Cyrillic (Russian) letters
fn validate_offline_username(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Username is required.".to_string());
    }
    if trimmed.len() < 3 || trimmed.len() > 16 {
        return Err("Username must be 3-16 characters long.".to_string());
    }
    for ch in trimmed.chars() {
        // Reject any character in the Cyrillic block (U+0400..U+04FF),
        // Cyrillic Supplement (U+0500..U+052F), and Cyrillic Extended.
        if matches!(ch,
            '\u{0400}'..='\u{04FF}'
            | '\u{0500}'..='\u{052F}'
            | '\u{2DE0}'..='\u{2DFF}'
            | '\u{A640}'..='\u{A69F}'
        ) {
            return Err("Username must not contain Cyrillic characters.".to_string());
        }
        if !(ch.is_ascii_alphanumeric() || ch == '_') {
            return Err(
                "Username may only contain Latin letters, digits, and underscores.".to_string(),
            );
        }
    }
    Ok(())
}

/// Validate an instance name. Used by every Tauri command that takes an
/// `instance_name` parameter, to prevent path-traversal attacks where a
/// malicious frontend sends `..` or other escape characters that would
/// otherwise be joined onto `instances_dir` and traverse the filesystem.
fn validate_instance_name(name: &str) -> Result<(), String> {
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
    if trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
    {
        return Err("Instance name contains invalid path characters.".to_string());
    }
    if trimmed.starts_with('.') {
        return Err("Instance name may not start with a dot.".to_string());
    }
    // Reject Windows reserved device names (CON, PRN, AUX, NUL, COM1..9, LPT1..9)
    let upper = trimmed.to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL",
        "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.iter().any(|r| *r == upper.as_str()) {
        return Err(format!("'{}' is a reserved name.", trimmed));
    }
    // Allow any printable Unicode character (including Cyrillic, CJK, emoji)
    // — instance names are folder names and the user can call them whatever
    // they want. The only thing we block here are control characters and
    // characters that would break path joining on any major platform.
    for ch in trimmed.chars() {
        if ch.is_control() {
            return Err(format!(
                "Instance name contains a control character (U+{:04X}).",
                ch as u32
            ));
        }
        // Reject the few characters that are special on Windows filenames
        // even when escaped: < > : " | ? *
        if matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            return Err(format!(
                "Instance name contains an invalid character: {:?}",
                ch
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod offline_username_tests {
    use super::validate_offline_username;

    #[test]
    fn accepts_latin_names() {
        assert!(validate_offline_username("Steve").is_ok());
        assert!(validate_offline_username("player_123").is_ok());
        assert!(validate_offline_username("ABC").is_ok());
    }

    #[test]
    fn rejects_cyrillic_names() {
        assert!(validate_offline_username("Вася").is_err());
        assert!(validate_offline_username("Иван").is_err());
        assert!(validate_offline_username("PlayerИван").is_err());
        // Cyrillic 'а' (U+0430) vs Latin 'a' (U+0061)
        assert!(validate_offline_username("аdm1n").is_err());
    }

    #[test]
    fn rejects_invalid_length() {
        assert!(validate_offline_username("ab").is_err());       // too short
        assert!(validate_offline_username("a".repeat(17).as_str()).is_err()); // too long
    }

    #[test]
    fn rejects_special_chars() {
        assert!(validate_offline_username("hello world").is_err());
        assert!(validate_offline_username("user@name").is_err());
        assert!(validate_offline_username("user!").is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_offline_username("").is_err());
        assert!(validate_offline_username("   ").is_err());
    }
}

#[cfg(test)]
mod instance_name_tests {
    use super::validate_instance_name;

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
}

#[tauri::command]
async fn cmd_add_elyby_account(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<Vec<accounts::AccountEntry>, String> {
    let data_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.data_dir.clone()
    };
    let (name, uuid, access_token) = auth::elyby_login(&username, &password)
        .await
        .map_err(|e| e.to_string())?;
    let entry = accounts::AccountEntry::new_elyby(&name, &uuid, &access_token);
    accounts::add_account(&data_dir, entry)
}

#[tauri::command]
fn cmd_remove_account(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<accounts::AccountEntry>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    accounts::remove_account(&config.data_dir, &id)
}

#[tauri::command]
fn cmd_set_default_account(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<accounts::AccountEntry>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    accounts::set_default_account(&config.data_dir, &id)
}

#[tauri::command]
async fn cmd_change_skin(
    state: State<'_, AppState>,
    account_id: String,
    skin_path: String,
    variant: String,
) -> Result<(), String> {
    let (data_dir, account_type, mc_token) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let accounts_list = accounts::list_accounts(&config.data_dir);
        let account = accounts_list.iter().find(|a| a.id == account_id)
            .ok_or("Account not found")?.clone();
        let mc_token = if account.account_type == accounts::AccountType::Microsoft {
            let auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
            auth_state.minecraft_token.as_ref().map(|t| t.access_token.clone())
        } else {
            None
        };
        (config.data_dir.clone(), account.account_type, mc_token)
    };

    if account_type == accounts::AccountType::Microsoft {
        let token = mc_token.ok_or("Not logged in with Microsoft")?;
        auth::change_microsoft_skin(&token, std::path::Path::new(&skin_path), &variant)
            .await
            .map_err(|e| e.to_string())?;
    }

    // Store skin variant in account
    let mut accounts_list = accounts::list_accounts(&data_dir);
    if let Some(a) = accounts_list.iter_mut().find(|a| a.id == account_id) {
        a.skin_variant = Some(variant);
    }
    accounts::save_accounts(&data_dir, &accounts_list).map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
fn cmd_get_skin_path(state: State<'_, AppState>, account_id: String) -> Result<Option<String>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let accounts_list = accounts::list_accounts(&config.data_dir);
    let account = accounts_list.iter().find(|a| a.id == account_id)
        .ok_or("Account not found")?;

    // Check if there's a skin file for this account
    let skin_path = config.data_dir.join("skins").join(format!("{}.png", account.id));
    if skin_path.exists() {
        Ok(Some(skin_path.to_string_lossy().to_string()))
    } else {
        Ok(None)
    }
}

// ==================== Version Commands ====================

#[tauri::command]
async fn cmd_get_versions() -> Result<versions::VersionManifest, String> {
    versions::fetch_version_manifest()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_version_info(url: String) -> Result<versions::VersionInfo, String> {
    versions::fetch_version_info(&url)
        .await
        .map_err(|e| e.to_string())
}

// ==================== Instance Commands ====================

#[tauri::command]
fn cmd_list_instances(state: State<'_, AppState>) -> Result<Vec<instances::Instance>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut insts = instances::list_instances(&config.instances_dir()).map_err(|e| e.to_string())?;
    // Merge playtime from playtime.json into each instance
    let playtime_map = crate::playtime::load_playtime(&config.data_dir);
    for inst in &mut insts {
        if let Some(entry) = playtime_map.get(&inst.name) {
            inst.play_time_seconds = entry.minutes * 60;
        }
    }
    Ok(insts)
}

#[tauri::command]
fn cmd_create_instance(
    state: State<'_, AppState>,
    name: String,
    mc_version: String,
) -> Result<instances::Instance, String> {
    validate_instance_name(&name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::Instance::new(
        &name,
        &mc_version,
        config.default_memory_mb,
        &config.default_gc_preset,
    );
    instances::create_instance(&config.instances_dir(), &instance).map_err(|e| e.to_string())?;
    Ok(instance)
}

#[tauri::command]
fn cmd_delete_instance(state: State<'_, AppState>, name: String) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::delete_instance(&config.instances_dir(), &name).map_err(|e| e.to_string())
}

#[tauri::command]
fn cmd_get_instance(
    state: State<'_, AppState>,
    name: String,
) -> Result<instances::Instance, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut inst = instances::get_instance(&config.instances_dir(), &name).map_err(|e| e.to_string())?;
    // Merge playtime from playtime.json
    let playtime_map = crate::playtime::load_playtime(&config.data_dir);
    if let Some(entry) = playtime_map.get(&inst.name) {
        inst.play_time_seconds = entry.minutes * 60;
    }
    Ok(inst)
}

#[tauri::command]
fn cmd_save_instance(
    state: State<'_, AppState>,
    instance: instances::Instance,
) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::save_instance(&config.instances_dir(), &instance).map_err(|e| e.to_string())
}

#[tauri::command]
fn cmd_duplicate_instance(
    state: State<'_, AppState>,
    name: String,
    new_name: String,
) -> Result<instances::Instance, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::duplicate_instance(&config.instances_dir(), &name, &new_name).map_err(|e| e.to_string())
}

#[tauri::command]
fn cmd_set_instance_icon(
    state: State<'_, AppState>,
    instance_name: String,
    icon_data: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::save_instance_icon(&config.instances_dir(), &instance_name, &icon_data).map_err(|e| e.to_string())
}

#[tauri::command]
fn cmd_list_saves(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<instances::SaveEntry>, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::list_saves(&config.instances_dir(), &instance_name).map_err(|e| e.to_string())
}

#[tauri::command]
fn cmd_list_screenshots(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<instances::ScreenshotEntry>, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::list_screenshots(&config.instances_dir(), &instance_name).map_err(|e| e.to_string())
}

#[tauri::command]
fn cmd_list_packs(
    state: State<'_, AppState>,
    instance_name: String,
    pack_type: String,
) -> Result<Vec<instances::PackEntry>, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::list_packs(&config.instances_dir(), &instance_name, &pack_type).map_err(|e| e.to_string())
}

#[tauri::command]
fn cmd_get_pack_icon(
    state: State<'_, AppState>,
    instance_name: String,
    pack_type: String,
    filename: String,
) -> Result<Option<String>, String> {
    let safe_filename = std::path::Path::new(&filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?
        .to_string();
    let config = state.config.lock().map_err(|e| e.to_string())?;
    instances::read_pack_icon(&config.instances_dir(), &instance_name, &pack_type, &safe_filename)
        .map_err(|e| e.to_string())
}

// ==================== Icon Cache ====================

#[tauri::command]
fn cmd_get_icon_cache(state: State<'_, AppState>) -> Result<HashMap<String, String>, String> {
    let cache = state.icon_cache.lock().map_err(|e| e.to_string())?;
    Ok(cache.clone())
}

#[tauri::command]
fn cmd_set_icon_cache_entry(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut cache = state.icon_cache.lock().map_err(|e| e.to_string())?;
    cache.insert(key, value);
    save_icon_cache_to_disk(&config, &cache);
    Ok(())
}

#[tauri::command]
fn cmd_watch_instance(
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
                if events.is_empty() { return; }
                let mut changed: Vec<String> = Vec::new();
                for ev in events {
                    let path_str = ev.path.to_string_lossy().to_string();
                    if path_str.contains("resourcepacks") { changed.push("resourcepacks".to_string()); }
                    else if path_str.contains("shaderpacks") { changed.push("shaderpacks".to_string()); }
                    else if path_str.contains("mods") { changed.push("mods".to_string()); }
                }
                changed.sort();
                changed.dedup();
                for sub in changed {
                    let _ = app_clone.emit("instance_dir_changed", serde_json::json!({
                        "instance": instance_for_closure.clone(),
                        "subfolder": sub,
                    }));
                }
            }
        },
    ).map_err(|e| e.to_string())?;

    for path in &watch_paths {
        debouncer.watcher().watch(path, RecursiveMode::NonRecursive)
            .map_err(|e| e.to_string())?;
    }

    // Replace any existing watcher
    let mut watcher_slot = state.pack_watcher.lock().map_err(|e| e.to_string())?;
    *watcher_slot = Some(PackWatcherHandle {
        instance_name: instance_name.clone(),
        _debouncer: debouncer,
    });

    Ok(())
}

#[tauri::command]
fn cmd_unwatch_instance(state: State<'_, AppState>) -> Result<(), String> {
    let mut watcher_slot = state.pack_watcher.lock().map_err(|e| e.to_string())?;
    *watcher_slot = None; // Drop the debouncer, stopping the watcher
    Ok(())
}

// ==================== Java Commands ====================

#[tauri::command]
fn cmd_detect_java() -> Vec<java::JavaInstallation> {
    java::detect_java_installations()
}

// ==================== Launch Commands ====================

#[tauri::command]
async fn cmd_install_version(
    app: AppHandle,
    state: State<'_, AppState>,
    version_url: String,
    instance_id: String,
) -> Result<String, String> {
    let config = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.clone()
    };

    // Create progress channel and bridge
    let (progress_tx, rx) = ProgressSender::new();
    events::spawn_event_bridge(app.clone(), rx, instance_id.clone());

    let send_progress = |percent: f64, stage: &str, message: &str| {
        progress_tx.send(InstallProgressPayload {
            instance_id: instance_id.clone(),
            percent,
            downloaded_bytes: 0,
            total_bytes: 0,
            stage: stage.to_string(),
            message: message.to_string(),
        });
    };

    events::emit_log(&app, "info", "install", "Fetching version info...");
    send_progress(0.0, "manifest", "Fetching version info...");

    // Fetch version info
    let version_info = versions::fetch_version_info(&version_url)
        .await
        .map_err(|e| e.to_string())?;

    events::emit_log(&app, "info", "install", &format!("Version {} fetched", version_info.id));
    send_progress(5.0, "manifest", "Collecting files to download...");

    // Collect files to download
    let files = versions::collect_downloads(
        &version_info,
        &config.libraries_dir(),
        &config.versions_dir(),
    );

    let progress_tx_clone = progress_tx.clone();
    let instance_id_clone = instance_id.clone();

    // Download all files with real progress
    events::emit_log(&app, "info", "install", &format!("Downloading {} libraries...", files.len()));
    send_progress(10.0, "libraries", "Downloading libraries...");
    download::download_files(files, move |completed, total, _msg| {
        let pct = 10.0 + (completed as f64 / total as f64) * 60.0;
        progress_tx_clone.send(InstallProgressPayload {
            instance_id: instance_id_clone.clone(),
            percent: pct,
            downloaded_bytes: completed as u64,
            total_bytes: total as u64,
            stage: "libraries".to_string(),
            message: format!("Downloading libraries ({}/{})", completed, total),
        });
    })
    .await
    .map_err(|e| e.to_string())?;

    send_progress(70.0, "libraries", "Saving version metadata...");

    // Save version JSON
    let version_dir = config.versions_dir().join(&version_info.id);
    std::fs::create_dir_all(&version_dir).map_err(|e| e.to_string())?;
    let version_json_path = version_dir.join(format!("{}.json", version_info.id));
    let json = serde_json::to_string_pretty(&version_info).map_err(|e| e.to_string())?;
    std::fs::write(version_json_path, json).map_err(|e| e.to_string())?;

    send_progress(75.0, "assets", "Downloading asset index...");

    // Download and save asset index
    let asset_index = versions::fetch_asset_index(&version_info.asset_index.url)
        .await
        .map_err(|e| e.to_string())?;

    let indexes_dir = config.assets_dir().join("indexes");
    std::fs::create_dir_all(&indexes_dir).map_err(|e| e.to_string())?;
    let index_path = indexes_dir.join(format!("{}.json", version_info.assets));
    let index_json = serde_json::to_string_pretty(&asset_index).map_err(|e| e.to_string())?;
    std::fs::write(index_path, index_json).map_err(|e| e.to_string())?;

    events::emit_log(&app, "info", "install", &format!("Downloading assets for {}...", version_info.assets));
    send_progress(78.0, "assets", "Downloading assets...");

    // Download assets with progress
    let progress_tx_assets = progress_tx.clone();
    let instance_id_assets = instance_id.clone();
    download::download_assets(&asset_index, &config.assets_dir(), move |completed, total, _msg| {
        let pct = 78.0 + (completed as f64 / total as f64) * 20.0;
        progress_tx_assets.send(InstallProgressPayload {
            instance_id: instance_id_assets.clone(),
            percent: pct,
            downloaded_bytes: completed as u64,
            total_bytes: total as u64,
            stage: "assets".to_string(),
            message: format!("Downloading assets ({}/{})", completed, total),
        });
    })
    .await
    .map_err(|e| e.to_string())?;

    events::emit_log(&app, "info", "install", "Installation complete!");
    send_progress(100.0, "done", "Installation complete!");

    Ok(version_info.id)
}

#[tauri::command]
async fn cmd_launch_game(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    events::emit_log(&app, "info", "launch", &format!("Preparing to launch: {}", instance_name));

    let (config, mut auth, data_dir) = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        let a = state.auth_state.lock().map_err(|e| e.to_string())?;
        (c.clone(), a.clone(), c.data_dir.clone())
    };

    // Try Microsoft account first, then check accounts list for offline/Ely.by
    //
    // Pre-check: if the cached Minecraft access token has expired, try a
    // full re-authentication (Microsoft -> Xbox -> XSTS -> Minecraft) using
    // the stored OAuth refresh_token.  Without this, Hypixel and other
    // online servers reject the session once the initial ~24-hour token
    // lifetime runs out (Mojang's error: HTTP 401 "Invalid session").
    //
    // If the whole OAuth chain fails, `auth` keeps the stale token and
    // the outer `if let …?` below will fall through to the accounts-list
    // fallback (offline / Ely.by / cached-offline) so the user can at
    // least play offline.
    if let Some(ref mc_token) = auth.minecraft_token {
        if let Some(ref ms_token) = auth.microsoft_token {
            if auth::is_token_expired(mc_token) && !ms_token.refresh_token.is_empty() {
                events::emit_log(&app, "info", "launch", "Minecraft token expired; re-authenticating via Microsoft OAuth...");
                let client_id = config.client_id.clone();
                let refresh_tok = ms_token.refresh_token.clone();
                match auth::refresh_microsoft_token(&client_id, &refresh_tok).await {
                    Ok(new_ms) => {
                        match auth::full_auth_flow(&new_ms).await {
                            Ok((new_mc, new_profile)) => {
                                events::emit_log(&app, "info", "launch", &format!("Token refreshed for user: {}", new_profile.name));
                                let fresh_state = auth::AuthState {
                                    microsoft_token: Some(new_ms),
                                    minecraft_token: Some(new_mc),
                                    profile: Some(new_profile),
                                    timestamp: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default().as_secs(),
                                    offline_mode: false,
                                };
                                let _ = auth::save_auth_state(&config.auth_file(), &fresh_state);
                                {
                                    let mut as_guard = state.auth_state.lock().map_err(|e| e.to_string())?;
                                    *as_guard = fresh_state.clone();
                                }
                                auth = fresh_state; // <-- the if-let below will see the fresh token
                            }
                            Err(e) => {
                                events::emit_log(&app, "warn", "launch", &format!("Full re-auth failed: {}", e));
                            }
                        }
                    }
                    Err(e) => {
                        events::emit_log(&app, "warn", "launch", &format!("Microsoft token refresh failed: {}", e));
                    }
                }
            }
        }
    }

    let child = if let (Some(ref mc_token), Some(ref profile)) = (&auth.minecraft_token, &auth.profile) {
        events::emit_log(&app, "info", "launch", "Online mode: using Microsoft account");
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;

        events::emit_log(&app, "info", "launch", "Fetching version manifest...");
        let manifest = versions::fetch_version_manifest()
            .await
            .map_err(|e| e.to_string())?;

        let version_url = manifest.versions.iter()
            .find(|v| v.id == instance.mc_version)
            .ok_or_else(|| {
                let msg = format!("Version {} not found in manifest", instance.mc_version);
                events::emit_log(&app, "error", "launch", &msg);
                msg
            })?
            .url.clone();

        events::emit_log(&app, "info", "launch", &format!("Fetching version info for {}...", instance.mc_version));
        let version_info = versions::fetch_version_info(&version_url)
            .await
            .map_err(|e| e.to_string())?;

        events::emit_log(&app, "info", "launch", "Building classpath and launching Java...");
        launch::launch_minecraft(
            &config,
            &instance,
            &version_info,
            &mc_token.access_token,
            &profile.id,
            &profile.name,
        ).map_err(|e| {
            events::emit_log(&app, "error", "launch", &format!("Launch failed: {}", e));
            e.to_string()
        })?
    } else {
        // Try accounts from accounts.json (offline / Ely.by)
        let accounts_list = accounts::list_accounts(&data_dir);
        let default_account = accounts_list.iter().find(|a| a.default).or_else(|| accounts_list.first());

        if let Some(account) = default_account {
            let (uuid, username) = match account.account_type {
                accounts::AccountType::Offline => {
                    let uuid_val = account.uuid.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    (uuid_val, account.name.clone())
                }
                accounts::AccountType::ElyBy => {
                    let uuid_val = account.uuid.clone().unwrap_or_default();
                    (uuid_val, account.name.clone())
                }
                accounts::AccountType::Microsoft => {
                    // Try to refresh Microsoft token first
                    let auth_path = config.auth_file();
                    if let Some(auth_state) = auth::load_auth_state(&auth_path) {
                        if let Some(ref ms_token) = auth_state.microsoft_token {
                            if !ms_token.refresh_token.is_empty() {
                                events::emit_log(&app, "info", "launch", "Refreshing Microsoft token...");
                                let client_id = config.client_id.clone();
                                let refresh_tok = ms_token.refresh_token.clone();
                                match auth::refresh_microsoft_token(&client_id, &refresh_tok).await {
                                    Ok(new_token) => {
                                        let fresh_state = auth::AuthState {
                                            microsoft_token: Some(new_token),
                                            ..auth_state
                                        };
                                        let _ = auth::save_auth_state(&auth_path, &fresh_state);
                                    }
                                    Err(e) => {
                                        events::emit_log(&app, "warn", "launch", &format!("Token refresh failed: {}, using cached", e));
                                    }
                                }
                            }
                        }
                    }
                    // Fall through to cached credentials
                    if !auth::can_launch_offline(&auth_path) {
                        let msg = "Cannot launch offline: no valid cached credentials found.".to_string();
                        events::emit_log(&app, "error", "launch", &msg);
                        return Err(msg);
                    }
                    let (un, uid) = auth::get_offline_credentials(&config.auth_file())
                        .ok_or("Failed to get offline credentials")?;
                    (uid, un)
                }
            };

            events::emit_log(&app, "info", "launch", &format!("Using account: {} ({})", username, uuid));

            let instance = instances::get_instance(&config.instances_dir(), &instance_name)
                .map_err(|e| e.to_string())?;

            let version_path = config.versions_dir().join(&instance.mc_version).join(format!("{}.json", instance.mc_version));
            events::emit_log(&app, "info", "launch", &format!("Loading version info from {:?}", version_path));
            let version_info = versions::version_info_from_file(&version_path)
                .map_err(|e| {
                    let msg = format!("Failed to load version info from {:?}: {}", version_path, e);
                    events::emit_log(&app, "error", "launch", &msg);
                    msg
                })?;

            events::emit_log(&app, "info", "launch", "Building classpath and launching Java...");
            launch::launch_minecraft(
                &config,
                &instance,
                &version_info,
                "offline_token",
                &uuid,
                &username,
            ).map_err(|e| {
                events::emit_log(&app, "error", "launch", &format!("Launch failed: {}", e));
                e.to_string()
            })?
        } else {
            // Fall back to cached Microsoft credentials
            events::emit_log(&app, "info", "launch", "No accounts found, trying cached credentials");

            // Try to refresh token
            let auth_path = config.auth_file();
            if let Some(auth_state) = auth::load_auth_state(&auth_path) {
                if let Some(ref ms_token) = auth_state.microsoft_token {
                    if !ms_token.refresh_token.is_empty() {
                        events::emit_log(&app, "info", "launch", "Refreshing Microsoft token...");
                        let client_id = config.client_id.clone();
                        let refresh_tok = ms_token.refresh_token.clone();
                        match auth::refresh_microsoft_token(&client_id, &refresh_tok).await {
                            Ok(new_token) => {
                                let fresh_state = auth::AuthState {
                                    microsoft_token: Some(new_token),
                                    ..auth_state
                                };
                                let _ = auth::save_auth_state(&auth_path, &fresh_state);
                            }
                            Err(e) => {
                                events::emit_log(&app, "warn", "launch", &format!("Token refresh failed: {}, using cached", e));
                            }
                        }
                    }
                }
            }

            if !auth::can_launch_offline(&auth_path) {
                let msg = "Cannot launch offline: no accounts configured and no cached credentials found. Please add an account first.".to_string();
                events::emit_log(&app, "error", "launch", &msg);
                return Err(msg);
            }

            let (username, uuid) = auth::get_offline_credentials(&config.auth_file())
                .ok_or("Failed to get offline credentials")?;
            events::emit_log(&app, "info", "launch", &format!("Offline user: {} ({})", username, uuid));

            let instance = instances::get_instance(&config.instances_dir(), &instance_name)
                .map_err(|e| e.to_string())?;

            let version_path = config.versions_dir().join(&instance.mc_version).join(format!("{}.json", instance.mc_version));
            events::emit_log(&app, "info", "launch", &format!("Loading version info from {:?}", version_path));
            let version_info = versions::version_info_from_file(&version_path)
                .map_err(|e| {
                    let msg = format!("Failed to load version info from {:?}: {}", version_path, e);
                    events::emit_log(&app, "error", "launch", &msg);
                    msg
                })?;

            events::emit_log(&app, "info", "launch", "Building classpath and launching Java (offline)...");
            launch::launch_minecraft(
                &config,
                &instance,
                &version_info,
                "offline_token",
                &uuid,
                &username,
            ).map_err(|e| {
                events::emit_log(&app, "error", "launch", &format!("Launch failed: {}", e));
                e.to_string()
            })?
        }
    };

    let pid = child.id();
    events::emit_log(&app, "info", "launch", &format!("Java process started (PID: {})", pid));

    // Update last played
    let _ = instances::update_last_played(&config.instances_dir(), &instance_name);

    // Emit game started
    let _ = app.emit(
        "game_started",
        events::LaunchEventPayload {
            instance_id: instance_name.clone(),
            status: "running".into(),
            pid: Some(pid),
            exit_code: None,
        },
    );

    // Wrap the child in a shared handle so the playtime timer can call try_wait()
    // while the stdout/stderr reader threads consume the pipes.
    let child_handle: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(Some(child)));

    // Take stdout and stderr out of the child WITHOUT moving the child itself
    // (so the timer + wait tasks can still call try_wait on it).
    let (stdout_opt, stderr_opt) = {
        let mut guard = child_handle.lock().expect("child_handle lock poisoned");
        let c = guard.as_mut().expect("child just inserted");
        (c.stdout.take(), c.stderr.take())
    };

    // Register the playtime session in global state
    {
        let now = Instant::now();
        let session = playtime::ActiveSession {
            instance_name: instance_name.clone(),
            pid,
            started_at: now,
            last_flush: now,
            child: child_handle.clone(),
        };
        if let Ok(mut guard) = state.active_session.lock() {
            *guard = Some(session);
        }
    }

    // Read Java process output in background and forward to log_message events.
    // We read bytes and lossy-decode instead of `BufRead::lines()` because
    // Minecraft writes some output in the system code page (e.g. CP1251 on a
    // Russian Windows) and `lines().flatten()` silently drops every non-UTF-8
    // line — which is exactly when a crash is most likely to print a useful
    // message. Replacing invalid UTF-8 with U+FFFD keeps the log intact.
    if let Some(stdout) = stdout_opt {
        let app_clone = app.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            let mut pending = String::new();
            let mut handle = stdout;
            loop {
                match handle.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                        while let Some(idx) = pending.find('\n') {
                            let line: String = pending.drain(..=idx).collect();
                            let line = line.trim_end_matches(&['\r', '\n'][..]).to_string();
                            if !line.is_empty() {
                                events::emit_log(&app_clone, "info", "minecraft", &line);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            if !pending.is_empty() {
                events::emit_log(&app_clone, "info", "minecraft", pending.trim_end());
            }
        });
    }
    if let Some(stderr) = stderr_opt {
        let app_clone = app.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            let mut pending = String::new();
            let mut handle = stderr;
            loop {
                match handle.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                        while let Some(idx) = pending.find('\n') {
                            let line: String = pending.drain(..=idx).collect();
                            let line = line.trim_end_matches(&['\r', '\n'][..]).to_string();
                            if !line.is_empty() {
                                // Use `error` for stderr so the UI highlights
                                // Java exceptions visibly in the log panel.
                                events::emit_log(&app_clone, "error", "minecraft", &line);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            if !pending.is_empty() {
                events::emit_log(&app_clone, "error", "minecraft", pending.trim_end());
            }
        });
    }

    // Background task: every 60s, commit a minute of playtime if the process is alive.
    // Exits when the process is no longer alive (also commits the final partial minute).
    let app_for_timer = app.clone();
    let instance_for_timer = instance_name.clone();
    let data_dir_for_timer = data_dir.clone();
    let child_for_timer = child_handle.clone();
    tokio::spawn(async move {
        use std::time::Duration;
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.tick().await; // skip immediate tick
        loop {
            interval.tick().await;
            // Check liveness via the shared child handle
            let alive = {
                if let Ok(mut guard) = child_for_timer.lock() {
                    if let Some(child) = guard.as_mut() {
                        matches!(child.try_wait(), Ok(None))
                    } else { false }
                } else { false }
            };
            if !alive {
                // Process exited — commit any final sub-minute tail via the helper
                let now = Instant::now();
                let app_state = app_for_timer.state::<AppState>();
                if let Some((name, delta)) = playtime::take_session(&app_state.active_session, now) {
                    if delta > 0 {
                        playtime::add_minutes_and_save(&data_dir_for_timer, &name, delta);
                    }
                }
                events::emit_log(&app_for_timer, "info", "launch", &format!("Playtime session ended for: {}", instance_for_timer));
                break;
            }
            // Commit the actual unpaid minutes since the last flush. The timer
            // tick is just a cadence hint — a slow tick (GC pause, system suspend)
            // should credit 2+ minutes, and a fast tick should credit 0. The
            // `last_flush` cursor is advanced by `take_session` / `touch_session`
            // so the sub-minute remainder is preserved for the next tick or the
            // final teardown.
            let now = Instant::now();
            let app_state = app_for_timer.state::<AppState>();
            let delta = if let Ok(guard) = app_state.active_session.lock() {
                guard.as_ref().map(|s| s.unpaid_minutes(now)).unwrap_or(0)
            } else {
                0
            };
            if delta > 0 {
                playtime::add_minutes_and_save(&data_dir_for_timer, &instance_for_timer, delta);
                playtime::touch_session(&app_state.active_session, now);
                events::emit_log(&app_for_timer, "debug", "playtime", &format!("+{} min for {}", delta, instance_for_timer));
            }
        }
    });

    // Background task: wait for the process to exit and emit launch_complete
    let app_clone = app.clone();
    let instance_clone = instance_name.clone();
    let child_for_wait = child_handle.clone();
    tokio::spawn(async move {
        // Poll try_wait in a loop; multiple try_wait callers are safe.
        let exit_code: i32 = loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let status_opt = {
                if let Ok(mut guard) = child_for_wait.lock() {
                    if let Some(child) = guard.as_mut() {
                        child.try_wait().ok().flatten()
                    } else {
                        // Child was removed (shouldn't happen); bail out
                        break -1;
                    }
                } else {
                    break -1;
                }
            };
            if let Some(s) = status_opt {
                break s.code().unwrap_or(-1);
            }
        };

        events::emit_log(&app_clone, "info", "launch", &format!("Game exited with code {}", exit_code));
        let _ = app_clone.emit(
            "launch_complete",
            events::LaunchEventPayload {
                instance_id: instance_clone,
                status: "exited".into(),
                pid: None,
                exit_code: Some(exit_code),
            },
        );
    });

    events::emit_log(&app, "info", "launch", &format!("Game launched: {} (PID: {})", instance_name, pid));

    Ok(())
}

// ==================== Mod Loader Commands ====================

/// All five `cmd_get_*_versions` commands return a `LoaderVersionPage`
/// — a slice of the full sorted version list plus `total`, the count
/// of all versions that match the MC filter. The wizard uses this for
/// infinite scroll: it asks for `PAGE_SIZE` items starting at
/// `accumulator.length`, appends them, then asks for the next page
/// at the new length. When `accumulator.length >= total` the wizard
/// stops. See `CreateInstanceWizard.tsx` for the front-end logic.
///
/// The first call per `uid` triggers a network fetch (60s timeout,
/// fail-soft → empty page with `total = 0` on error); subsequent
/// pages come from the in-process cache and are instant.
const PAGE_SIZE: usize = 20;

#[tauri::command]
async fn cmd_get_fabric_versions(
    offset: usize,
    limit: usize,
) -> Result<modloaders::LoaderVersionPage, String> {
    let limit = if limit == 0 { PAGE_SIZE } else { limit };
    modloaders::fabric::get_loader_versions(offset, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_quilt_versions(
    offset: usize,
    limit: usize,
) -> Result<modloaders::LoaderVersionPage, String> {
    let limit = if limit == 0 { PAGE_SIZE } else { limit };
    modloaders::quilt::get_loader_versions(offset, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_install_fabric(
    state: State<'_, AppState>,
    mc_version: String,
    loader_version: String,
    instance_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let libraries_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.libraries_dir()
    };
    let profile = modloaders::fabric::install(&mc_version, &loader_version, &libraries_dir)
        .await
        .map_err(|e| e.to_string())?;

    // Save loader profile to instance
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    instance.loader_profile = Some(profile);
    instances::save_instance(&config.instances_dir(), &instance).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn cmd_install_quilt(
    state: State<'_, AppState>,
    mc_version: String,
    loader_version: String,
    instance_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let libraries_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.libraries_dir()
    };
    let profile = modloaders::quilt::install(&mc_version, &loader_version, &libraries_dir)
        .await
        .map_err(|e| e.to_string())?;

    // Save loader profile to instance
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    instance.loader_profile = Some(profile);
    instances::save_instance(&config.instances_dir(), &instance).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn cmd_get_forge_versions(
    mc_version: String,
    offset: usize,
    limit: usize,
) -> Result<modloaders::LoaderVersionPage, String> {
    let limit = if limit == 0 { PAGE_SIZE } else { limit };
    modloaders::forge::get_loader_versions(&mc_version, offset, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_neoforge_versions(
    mc_version: String,
    offset: usize,
    limit: usize,
) -> Result<modloaders::LoaderVersionPage, String> {
    let limit = if limit == 0 { PAGE_SIZE } else { limit };
    modloaders::neoforge::get_loader_versions(&mc_version, offset, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_liteloader_versions(
    mc_version: String,
    offset: usize,
    limit: usize,
) -> Result<modloaders::LoaderVersionPage, String> {
    let limit = if limit == 0 { PAGE_SIZE } else { limit };
    modloaders::liteloader::get_loader_versions(&mc_version, offset, limit)
        .await
        .map_err(|e| e.to_string())
}

// ==================== Modrinth API ====================

#[tauri::command]
async fn cmd_search_modrinth(
    query: String,
    project_type: String,
    mc_version: Option<String>,
    loader: Option<String>,
    offset: u32,
    limit: u32,
) -> Result<modrinth::ModrinthSearchResponse, String> {
    modrinth::search_mods(
        &query,
        &project_type,
        mc_version.as_deref(),
        loader.as_deref(),
        offset,
        limit,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_modrinth_versions(
    project_id: String,
    mc_version: Option<String>,
    loader: Option<String>,
) -> Result<Vec<modrinth::ModrinthVersion>, String> {
    modrinth::get_versions(&project_id, mc_version.as_deref(), loader.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_modrinth_project(id: String) -> Result<modrinth::ModrinthProject, String> {
    modrinth::get_project(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_modrinth_version_by_id(
    version_id: String,
) -> Result<modrinth::ModrinthVersionResponse, String> {
    modrinth::get_version_by_id(&version_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_modrinth_project_body(id: String) -> Result<String, String> {
    let project = modrinth::get_project(&id).await.map_err(|e| e.to_string())?;
    Ok(project.body.unwrap_or_default())
}

#[tauri::command]
async fn cmd_popular_modrinth(
    project_type: String,
    mc_version: Option<String>,
    loader: Option<String>,
    limit: u32,
    offset: u32,
) -> Result<modrinth::ModrinthSearchResponse, String> {
    modrinth::popular_mods(
        &project_type,
        mc_version.as_deref(),
        loader.as_deref(),
        limit,
        offset,
    )
    .await
    .map_err(|e| e.to_string())
}

// ==================== CurseForge API ====================

#[tauri::command]
async fn cmd_search_curseforge(
    state: State<'_, AppState>,
    query: String,
    mc_version: Option<String>,
    loader: Option<String>,
    offset: u32,
    limit: u32,
) -> Result<curseforge::CfSearchResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::search_mods(
        &query,
        mc_version.as_deref(),
        loader.as_deref(),
        offset,
        limit,
        &api_key,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_curseforge_files(
    state: State<'_, AppState>,
    mod_id: u64,
    mc_version: Option<String>,
    loader: Option<String>,
) -> Result<curseforge::CfFilesResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::get_mod_files(mod_id, mc_version.as_deref(), loader.as_deref(), &api_key)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_get_curseforge_mod_detail(
    state: State<'_, AppState>,
    mod_id: u64,
) -> Result<curseforge::CfModDetail, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::get_mod(mod_id, &api_key)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_popular_curseforge(
    state: State<'_, AppState>,
    mc_version: Option<String>,
    loader: Option<String>,
    limit: u32,
) -> Result<curseforge::CfSearchResponse, String> {
    let api_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };
    curseforge::popular_mods(mc_version.as_deref(), loader.as_deref(), limit, &api_key)
        .await
        .map_err(|e| e.to_string())
}

// ==================== File / Folder Open ====================

/// Open a folder in the OS file manager. Restricted to internal directories
/// (instances root, data dir, .minecraft of an instance, plus instance
/// subfolders like "mods", "logs", "screenshots", etc.). Arbitrary paths
/// from the frontend are rejected to prevent opening system locations.
#[tauri::command]
fn cmd_open_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: Option<String>,
    subfolder: Option<String>,
) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let base = if let Some(name) = instance_name.as_deref() {
        validate_instance_name(name)?;
        let instance = instances::get_instance(&config.instances_dir(), name)
            .map_err(|e| e.to_string())?;
        instance.minecraft_dir(&config.instances_dir())
    } else {
        config.data_dir.clone()
    };
    drop(config);

    let target = if let Some(sub) = subfolder.as_deref() {
        if sub.is_empty() {
            base
        } else {
            const ALLOWED: &[&str] = &[
                "mods", "resourcepacks", "shaderpacks", "config", "screenshots",
                "saves", "logs", "datapacks", "crash-reports", "versions",
            ];
            if !ALLOWED.contains(&sub) || sub.contains("..") || sub.contains('/')
                || sub.contains('\\') || sub.contains(':') || sub.contains('\0')
            {
                return Err(format!("Subfolder '{}' is not allowed", sub));
            }
            base.join(sub)
        }
    } else {
        base
    };

    if !target.exists() {
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    }

    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(target.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cmd_install_forge(
    state: State<'_, AppState>,
    mc_version: String,
    loader_version: String,
    instance_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let libraries_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.libraries_dir()
    };
    let profile = modloaders::forge::install(&mc_version, &loader_version, &libraries_dir)
        .await
        .map_err(|e| e.to_string())?;

    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    instance.loader_profile = Some(profile);
    instances::save_instance(&config.instances_dir(), &instance).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn cmd_install_neoforge(
    state: State<'_, AppState>,
    mc_version: String,
    loader_version: String,
    instance_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let libraries_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.libraries_dir()
    };
    let profile = modloaders::neoforge::install(&mc_version, &loader_version, &libraries_dir)
        .await
        .map_err(|e| e.to_string())?;

    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    instance.loader_profile = Some(profile);
    instances::save_instance(&config.instances_dir(), &instance).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn cmd_install_liteloader(
    _state: State<'_, AppState>,
    _mc_version: String,
    _loader_version: String,
    _instance_name: String,
) -> Result<(), String> {
    // LiteLoader has been abandoned since 2017 — its downloads live at
    // `dl.liteloader.com/versions/` which is no longer reachable. The
    // wizard still *lists* LiteLoader versions (for parity with Prism
    // Launcher), but the install path is a hard error so the user gets
    // a visible toast instead of a silent half-installed instance.
    //
    // The wizard's `cmd_install_liteloader` branch in CreateInstanceWizard
    // catches this and shows `create_instance.liteloader_unsupported`
    // with an "OK" button — see `addToast(..., 'warning')` there.
    Err("LiteLoader is no longer maintained and cannot be installed. \
         Versions exist only for Minecraft 1.12.1 / 1.12.2 and the \
         upstream download URLs (dl.liteloader.com) are unreachable."
        .to_string())
}

#[tauri::command]
async fn cmd_install_mod(
    state: State<'_, AppState>,
    instance_name: String,
    modrinth_version_id: Option<String>,
    #[allow(unused_variables)] curseforge_file_id: Option<i32>,
    file_name: String,
    download_url: String,
    project_id: Option<String>,
    version_number: Option<String>,
    provider: String,
) -> Result<String, String> {
    validate_instance_name(&instance_name)?;
    // Validate the URL is HTTPS and points to a known Modrinth / CurseForge CDN.
    if !download_url.starts_with("https://") {
        return Err("Download URL must be HTTPS.".to_string());
    }
    if !is_allowed_download_host(&download_url) {
        return Err(format!(
            "Download host is not in the allowlist: {}",
            download_url
        ));
    }
    let (mods_dir, safe_name, dest) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        let mods_dir = instance.mods_dir(&config.instances_dir());
        let safe_name = std::path::Path::new(&file_name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid file name")?
            .to_string();
        let dest = mods_dir.join(&safe_name);
        (mods_dir, safe_name, dest)
    };
    download::download_file(&download_url, &dest, "")
        .await
        .map_err(|e| e.to_string())?;
    let final_name = safe_name.clone();

    // Write sidecar so the installed mod can be tracked back to its source.
    if let Some(pid) = project_id.as_deref() {
        let sidecar = serde_json::json!({
            "provider": provider,
            "project_id": pid,
            "version_id": modrinth_version_id,
            "version_number": version_number,
        });
        let sidecar_path = mods_dir.join(format!("{}.voidlauncher.json", safe_name.trim_end_matches(".jar")));
        let _ = std::fs::write(sidecar_path, sidecar.to_string());
    }

    Ok(final_name)
}

#[tauri::command]
async fn cmd_download_to_folder(
    state: State<'_, AppState>,
    instance_name: String,
    subfolder: String,
    download_url: String,
    file_name: String,
    project_id: Option<String>,
    version_id: Option<String>,
    version_number: Option<String>,
    provider: String,
) -> Result<String, String> {
    validate_instance_name(&instance_name)?;
    if !download_url.starts_with("https://") {
        return Err("Download URL must be HTTPS.".to_string());
    }
    if !is_allowed_download_host(&download_url) {
        return Err(format!(
            "Download host is not in the allowlist: {}",
            download_url
        ));
    }
    // Whitelist allowed subfolders under the instance .minecraft dir.
    const ALLOWED: &[&str] = &[
        "mods", "resourcepacks", "shaderpacks", "config",
    ];
    let safe_subfolder = subfolder.trim_matches('/').trim_matches('\\');
    if !ALLOWED.iter().any(|s| s.eq_ignore_ascii_case(safe_subfolder)) {
        return Err(format!("Subfolder '{}' is not allowed.", subfolder));
    }
    if safe_subfolder.contains("..")
        || safe_subfolder.contains('\0')
        || safe_subfolder.contains(';')
    {
        return Err("Subfolder contains invalid characters.".to_string());
    }
    let (dest_dir, safe_name, dest) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        let dest_dir = instance.minecraft_dir(&config.instances_dir()).join(safe_subfolder);
        let _ = std::fs::create_dir_all(&dest_dir);
        let safe_name = std::path::Path::new(&file_name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid file name")?
            .to_string();
        let dest = dest_dir.join(&safe_name);
        (dest_dir, safe_name, dest)
    };
    download::download_file(&download_url, &dest, "")
        .await
        .map_err(|e| e.to_string())?;
    let final_name = safe_name.clone();
    if let Some(pid) = project_id.as_deref() {
        let sidecar = serde_json::json!({
            "provider": provider,
            "project_id": pid,
            "version_id": version_id,
            "version_number": version_number,
        });
        let sidecar_name = format!("{}.voidlauncher.json", safe_name.trim_end_matches(".jar"));
        let _ = std::fs::write(dest_dir.join(sidecar_name), sidecar.to_string());
    }
    Ok(final_name)
}

#[tauri::command]
fn cmd_open_instance_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
    subfolder: Option<String>,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    const ALLOWED: &[&str] = &[
        "mods", "resourcepacks", "shaderpacks", "config", "screenshots", "saves", "logs", "",
    ];
    let sub = subfolder.as_deref().unwrap_or("").trim_matches('/').trim_matches('\\');
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
fn cmd_get_instance_dir(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<String, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    Ok(instance.minecraft_dir(&config.instances_dir()).to_string_lossy().to_string())
}

#[tauri::command]
fn cmd_list_instance_mods(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<ModMetadata>, String> {
    validate_instance_name(&instance_name)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let mods_dir = instance.mods_dir(&config.instances_dir());
    if !mods_dir.exists() {
        return Ok(Vec::new());
    }
    let mut mods = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            // Skip the voidlauncher sidecar files
            if filename.ends_with(".voidlauncher.json") {
                continue;
            }
            let is_jar = filename.ends_with(".jar");
            let is_disabled = filename.ends_with(".jar.disabled");
            if !(is_jar || is_disabled) {
                continue;
            }
            let enabled = is_jar && !is_disabled;
            let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let meta = read_mod_meta_from_jar(&path);
            mods.push(ModMetadata {
                filename,
                name: meta.name,
                version: meta.version,
                provider: meta.provider,
                enabled,
                file_size,
                icon: meta.icon,
                slug: meta.slug,
            });
        }
    }
    mods.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(mods)
}

#[tauri::command]
fn cmd_remove_instance_mod(
    state: State<'_, AppState>,
    instance_name: String,
    filename: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let safe_name = std::path::Path::new(&filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?
        .to_string();
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let mods_dir = instance.mods_dir(&config.instances_dir());
    let mod_path = mods_dir.join(&safe_name);
    if mod_path.exists() {
        std::fs::remove_file(&mod_path).map_err(|e| e.to_string())?;
    }
    // Also remove any sidecar
    let sidecar = mods_dir.join(format!("{}.voidlauncher.json",
        safe_name.trim_end_matches(".jar").trim_end_matches(".disabled")));
    let _ = std::fs::remove_file(sidecar);
    Ok(())
}

#[tauri::command]
fn cmd_emit_log(
    app: AppHandle,
    level: String,
    source: String,
    message: String,
) -> Result<(), String> {
    // Whitelist the level enum to prevent arbitrary strings from polluting logs.
    let level_normalized = match level.to_lowercase().as_str() {
        "info" | "warn" | "warning" | "error" | "debug" => level.to_lowercase(),
        _ => return Err("Invalid log level".to_string()),
    };
    // Bound the source length to keep log files readable.
    let safe_source: String = source
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(32)
        .collect();
    if safe_source.is_empty() {
        return Err("Source must contain at least one alphanumeric character".to_string());
    }
    events::emit_log(&app, &level_normalized, &safe_source, &message);
    Ok(())
}

#[tauri::command]
fn cmd_rename_file(
    state: State<'_, AppState>,
    from: String,
    to: String,
) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instances_dir = config.instances_dir();
    let from_path = std::path::Path::new(&from);
    let to_path = std::path::Path::new(&to);
    // Open the source first to fail fast on missing/unreadable files.
    let _ = std::fs::File::open(from_path).map_err(|e| e.to_string())?;
    if let Ok(canon) = from_path.canonicalize() {
        let base_canon = instances_dir.canonicalize().map_err(|_| "Invalid base".to_string())?;
        if !canon.starts_with(&base_canon) {
            return Err("Access denied: path is outside instances directory".to_string());
        }
    }
    // Target may not exist yet; check parent
    if let Some(parent) = to_path.parent() {
        if let Ok(parent_canon) = parent.canonicalize() {
            let base_canon = instances_dir.canonicalize().map_err(|_| "Invalid base".to_string())?;
            if !parent_canon.starts_with(&base_canon) {
                return Err("Access denied: target is outside instances directory".to_string());
            }
        }
    }
    std::fs::rename(&from, &to).map_err(|e| e.to_string())
}

#[tauri::command]
fn cmd_delete_file(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instances_dir = config.instances_dir();
    if let Ok(canon) = std::path::Path::new(&path).canonicalize() {
        let base_canon = instances_dir.canonicalize().map_err(|_| "Invalid base".to_string())?;
        if !canon.starts_with(&base_canon) {
            return Err("Access denied: path is outside instances directory".to_string());
        }
    }
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

#[derive(Debug, serde::Serialize)]
pub struct ModMetadata {
    pub filename: String,
    pub name: String,
    pub version: String,
    pub provider: String,
    pub enabled: bool,
    pub file_size: u64,
    pub icon: Option<String>,
    pub slug: Option<String>,
}

#[tauri::command]
fn cmd_get_mod_metadata(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<ModMetadata>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let mods_dir = instance.minecraft_dir(&config.instances_dir()).join("mods");
    if !mods_dir.exists() {
        return Ok(Vec::new());
    }
    let mut mods = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let is_jar = filename.ends_with(".jar");
            let is_disabled = filename.ends_with(".jar.disabled");
            if is_jar || is_disabled {
                let enabled = is_jar && !is_disabled;
                let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let meta = read_mod_meta_from_jar(&path);
                mods.push(ModMetadata {
                    filename,
                    name: meta.name,
                    version: meta.version,
                    provider: meta.provider,
                    enabled,
                    file_size,
                    icon: meta.icon,
                    slug: meta.slug,
                });
            }
        }
    }
    mods.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(mods)
}

struct ModMetaResult {
    name: String,
    version: String,
    provider: String,
    icon: Option<String>,
    slug: Option<String>,
}

fn fallback_meta_from_filename(path: &std::path::Path) -> ModMetaResult {
    let fallback_name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string();
    let clean_name = if let Some(dash_pos) = fallback_name.rfind('-') {
        let potential_version = &fallback_name[dash_pos+1..];
        if potential_version.chars().next().map_or(false, |c| c.is_ascii_digit()) {
            fallback_name[..dash_pos].to_string()
        } else {
            fallback_name.clone()
        }
    } else {
        fallback_name.clone()
    };
    ModMetaResult { name: clean_name, version: "Unknown".into(), provider: "Local".into(), icon: None, slug: None }
}

fn read_mod_meta_from_jar(path: &std::path::Path) -> ModMetaResult {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return fallback_meta_from_filename(path),
    };
    let reader = std::io::BufReader::new(file);
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(a) => a,
        Err(_) => return fallback_meta_from_filename(path),
    };

    // Try fabric.mod.json
    if let Ok(mut file) = archive.by_name("fabric.mod.json") {
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_ok() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                let name = json["name"].as_str().or_else(|| json["id"].as_str()).unwrap_or("Unknown").to_string();
                let version = json["version"].as_str().unwrap_or("Unknown").to_string();
                let slug = json["id"].as_str().map(|s| s.to_string());
                let icon = json["icon"].as_str().map(|s| {
                    let clean = s.trim_start_matches("/").trim_start_matches("assets/").to_string();
                    clean
                });
                return ModMetaResult { name, version, provider: "Fabric".into(), icon, slug };
            }
        }
    }

    // Try META-INF/mods.toml (Forge/NeoForge)
    for toml_name in &["META-INF/mods.toml", "META-INF/neoforge.mods.toml"] {
        if let Ok(mut file) = archive.by_name(toml_name) {
            let mut contents = String::new();
            if file.read_to_string(&mut contents).is_ok() {
                // Parse mods.toml - may have multiple [[mods]] sections
                let mod_id = extract_toml_field(&contents, "modId").unwrap_or("Unknown".to_string());
                let display_name = extract_toml_field(&contents, "displayName").unwrap_or_else(|| mod_id.clone());
                let version = extract_toml_field(&contents, "version").unwrap_or("Unknown".to_string());
                let logo = extract_toml_field(&contents, "logoFile");
                let provider = if toml_name.contains("neoforge") { "NeoForge" } else { "Forge" };
                return ModMetaResult { name: display_name, version, provider: provider.into(), icon: logo, slug: Some(mod_id) };
            }
        }
    }

    // Try quilt.mod.json
    if let Ok(mut file) = archive.by_name("quilt.mod.json") {
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_ok() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                let name = json["quilt_loader"]["metadata"]["name"]
                    .as_str()
                    .or_else(|| json["quilt_loader"]["id"].as_str())
                    .unwrap_or("Unknown").to_string();
                let version = json["quilt_loader"]["version"].as_str().unwrap_or("Unknown").to_string();
                let slug = json["quilt_loader"]["id"].as_str().map(|s| s.to_string());
                let icon = json["quilt_loader"]["metadata"]["icon"].as_str().map(|s| s.trim_start_matches("/").trim_start_matches("assets/").to_string());
                return ModMetaResult { name, version, provider: "Quilt".into(), icon, slug };
            }
        }
    }

    // Fallback: extract name from filename
    fallback_meta_from_filename(path)
}

fn extract_toml_field(content: &str, field: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(field) && trimmed.contains('=') {
            let val = trimmed.splitn(2, '=').nth(1)?.trim().trim_matches('"').trim_matches('\'');
            if val.starts_with('{') {
                if let Some(start) = val.find(field) {
                    let after = &val[start..];
                    if let Some(eq_pos) = after.find('=') {
                        let v = after[eq_pos+1..].trim().trim_matches('"').trim_matches('\'');
                        let v = v.trim_end_matches(',').trim_end_matches('}').trim();
                        return Some(v.to_string());
                    }
                }
                continue;
            }
            return Some(val.to_string());
        }
    }
    None
}

#[tauri::command]
fn cmd_get_mod_icon(
    state: State<'_, AppState>,
    instance_name: String,
    filename: String,
) -> Result<Option<String>, String> {
    let safe_filename = std::path::Path::new(&filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?
        .to_string();
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let mods_dir = instance.minecraft_dir(&config.instances_dir()).join("mods");
    let jar_path = mods_dir.join(&safe_filename);
    if !jar_path.exists() {
        return Ok(None);
    }
    extract_icon_from_jar(&jar_path).map_err(|e| e.to_string())
}

fn extract_icon_from_jar(path: &std::path::Path) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader)?;

    // Try to find icon in common locations
    let mut found_icon_name = None;

    // First check fabric.mod.json for icon path
    if let Ok(mut f) = archive.by_name("fabric.mod.json") {
        let mut contents = String::new();
        if f.read_to_string(&mut contents).is_ok() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(icon) = json["icon"].as_str() {
                    let clean = icon.trim_start_matches("/").to_string();
                    found_icon_name = Some(clean);
                }
            }
        }
    }

    // Also check quilt.mod.json
    if found_icon_name.is_none() {
        if let Ok(mut f) = archive.by_name("quilt.mod.json") {
            let mut contents = String::new();
            if f.read_to_string(&mut contents).is_ok() {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                    if let Some(icon) = json["quilt_loader"]["metadata"]["icon"].as_str() {
                        let clean = icon.trim_start_matches("/").to_string();
                        found_icon_name = Some(clean);
                    }
                }
            }
        }
    }

    // If we found an icon path in metadata, try to read it
    if let Some(icon_name) = found_icon_name {
        if let Ok(mut f) = archive.by_name(&icon_name) {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            let b64 = base64_encode(&buf);
            let mime = if icon_name.ends_with(".png") { "image/png" } else if icon_name.ends_with(".jpg") || icon_name.ends_with(".jpeg") { "image/jpeg" } else { "image/png" };
            return Ok(Some(format!("data:{};base64,{}", mime, b64)));
        }
    }

    // Fallback: look for icon.png in root or assets
    for candidate in &["icon.png", "logo.png", "assets/icon.png"] {
        if let Ok(mut f) = archive.by_name(candidate) {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            let b64 = base64_encode(&buf);
            return Ok(Some(format!("data:image/png;base64,{}", b64)));
        }
    }

    // Scan all entries for icon-like files
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().to_string();
            let lower = name.to_lowercase();
            if (lower.ends_with("icon.png") || lower.ends_with("logo.png")) && !lower.contains("META-INF") {
                let mut buf = Vec::new();
                let mut file = entry;
                file.read_to_end(&mut buf)?;
                let b64 = base64_encode(&buf);
                return Ok(Some(format!("data:image/png;base64,{}", b64)));
            }
        }
    }

    Ok(None)
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

// ==================== Instance State Commands ====================

#[tauri::command]
fn cmd_check_instance_installed(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<bool, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;

    let version_jar = config
        .versions_dir()
        .join(&instance.mc_version)
        .join(format!("{}.jar", instance.mc_version));

    let version_json = config
        .versions_dir()
        .join(&instance.mc_version)
        .join(format!("{}.json", instance.mc_version));

    Ok(version_jar.exists() && version_json.exists())
}

// ==================== Launch State Commands ====================

#[tauri::command]
fn cmd_get_launch_state(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let running = state.running_instance_id.lock().map_err(|e| e.to_string())?;
    Ok(running.clone())
}

// ==================== Cache Commands ====================

#[tauri::command]
fn cmd_clear_cache(app: AppHandle, state: State<'_, AppState>) -> Result<u64, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let data_dir = config.data_dir.clone();
    drop(config);

    let mut freed: u64 = 0;
    for subdir in &["assets", "libraries"] {
        let dir = data_dir.join(subdir);
        if dir.exists() {
            let size = dir_size(&dir).unwrap_or(0);
            std::fs::remove_dir_all(&dir).map_err(|e| {
                let msg = format!("Failed to remove {:?}: {}", dir, e);
                events::emit_log(&app, "error", "cache", &msg);
                msg
            })?;
            freed += size;
            events::emit_log(&app, "info", "cache", &format!("Removed {:?} ({} MB)", dir, size / 1024 / 1024));
        }
    }

    Ok(freed)
}

fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            total += dir_size(&entry.path()).unwrap_or(0);
        } else {
            total += meta.len();
        }
    }
    Ok(total)
}

// ==================== Playtime Commands ====================

#[tauri::command]
fn cmd_get_playtime(state: State<'_, AppState>, instance_name: String) -> Result<u64, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(playtime::get_playtime(&config.data_dir, &instance_name))
}

#[tauri::command]
fn cmd_format_playtime(minutes: u64, language: Option<String>) -> String {
    let lang = match language.as_deref() {
        Some("en") => playtime::PlaytimeLang::En,
        // Default to English on unknown / null / "ru" — historically this
        // command always returned Russian, but the launcher's UI now ships
        // in English by default and the playtime label should match.
        _ => playtime::PlaytimeLang::En,
    };
    playtime::format_playtime_in(minutes, lang)
}

/// Flush the active playtime session (commits whole minutes and clears the session).
/// Called manually from the frontend (e.g., when the user closes a running game).
#[tauri::command]
fn cmd_flush_playtime(state: State<'_, AppState>) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let now = Instant::now();
    if let Some((name, delta)) = playtime::take_session(&state.active_session, now) {
        if delta > 0 {
            playtime::add_minutes_and_save(&config.data_dir, &name, delta);
        }
    }
    Ok(())
}

// ==================== System Commands ====================

#[tauri::command]
fn cmd_detect_system_ram() -> u64 {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    sys.total_memory() / (1024 * 1024) // Return MB
}

// ==================== Config Commands ====================

#[tauri::command]
fn cmd_get_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(config.clone())
}

#[tauri::command]
fn cmd_save_config(app: AppHandle, state: State<'_, AppState>, new_config: AppConfig) -> Result<(), String> {
    events::emit_log(&app, "info", "config", "Saving configuration...");
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    *config = new_config;
    config.save().map_err(|e| e.to_string())?;
    events::emit_log(&app, "info", "config", "Configuration saved");
    Ok(())
}

// ==================== App Builder ====================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("VoidLauncher");

    // File logger MUST be initialized first, before any other code path
    // that might emit a tracing event or call eprintln! (e.g. AppConfig::load
    // when the on-disk config is corrupt).
    logger::init(&data_dir);

    let config = AppConfig::load(&data_dir);
    let auth_state = auth::load_auth_state(&config.auth_file()).unwrap_or_default();
    let icon_cache = load_icon_cache_from_disk(&config);

    tracing::info!(target: "launcher", "Data dir: {}", data_dir.display());
    tracing::info!(target: "launcher", "Config: data_dir={}, default_memory_mb={}, gc={}",
        config.data_dir.display(), config.default_memory_mb, config.default_gc_preset);

    // If there's a cached Microsoft session, ensure it's in accounts.json
    if let Some(ref profile) = auth_state.profile {
        tracing::info!(target: "launcher", "Restoring cached Microsoft session for {}", profile.name);
        let _ = accounts::upsert_microsoft_account(&data_dir, &profile.name, &profile.id);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState {
            config: Mutex::new(config),
            auth_state: Mutex::new(auth_state),
            running_instance_id: Mutex::new(None),
            pack_watcher: Mutex::new(None),
            icon_cache: Mutex::new(icon_cache),
            active_session: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            cmd_start_login,
            cmd_poll_login,
            cmd_get_auth_state,
            cmd_logout,
            cmd_can_launch_offline,
            cmd_get_offline_credentials,
            cmd_get_versions,
            cmd_get_version_info,
            cmd_list_instances,
            cmd_create_instance,
            cmd_delete_instance,
            cmd_get_instance,
            cmd_save_instance,
            cmd_detect_java,
            cmd_install_version,
            cmd_launch_game,
            cmd_get_fabric_versions,
            cmd_get_quilt_versions,
            cmd_get_forge_versions,
            cmd_get_neoforge_versions,
            cmd_get_liteloader_versions,
            cmd_install_fabric,
            cmd_install_quilt,
            cmd_install_forge,
            cmd_install_neoforge,
            cmd_install_liteloader,
            cmd_get_config,
            cmd_save_config,
            cmd_get_launch_state,
            cmd_check_instance_installed,
            cmd_detect_system_ram,
            cmd_search_modrinth,
            cmd_search_curseforge,
            cmd_get_modrinth_versions,
            cmd_get_curseforge_files,
            cmd_install_mod,
            cmd_get_modrinth_project,
            cmd_get_modrinth_version_by_id,
            cmd_popular_modrinth,
            cmd_popular_curseforge,
            cmd_get_curseforge_mod_detail,
            cmd_get_modrinth_project_body,
            cmd_list_accounts,
            cmd_add_offline_account,
            cmd_add_elyby_account,
            cmd_remove_account,
            cmd_set_default_account,
            cmd_change_skin,
            cmd_get_skin_path,
            cmd_open_folder,
            cmd_get_instance_dir,
            cmd_list_instance_mods,
            cmd_remove_instance_mod,
            cmd_get_mod_metadata,
            cmd_get_mod_icon,
            cmd_emit_log,
            cmd_rename_file,
            cmd_delete_file,
            cmd_duplicate_instance,
            cmd_set_instance_icon,
            cmd_list_saves,
            cmd_list_screenshots,
            cmd_list_packs,
            cmd_get_pack_icon,
            cmd_watch_instance,
            cmd_unwatch_instance,
            cmd_download_to_folder,
            cmd_open_instance_folder,
            cmd_get_icon_cache,
            cmd_set_icon_cache_entry,
            cmd_get_playtime,
            cmd_format_playtime,
            cmd_flush_playtime,
            cmd_clear_cache,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { .. } = event {
                // Flush any active playtime session before the app exits.
                // We do NOT call api.prevent_close() — we let the window close,
                // but first we write the unpaid minutes to disk synchronously.
                let state: tauri::State<'_, AppState> = window.state();
                let now = Instant::now();
                if let Some((name, delta)) = playtime::take_session(&state.active_session, now) {
                    if delta > 0 {
                        if let Ok(cfg) = state.config.lock() {
                            playtime::add_minutes_and_save(&cfg.data_dir, &name, delta);
                        }
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("Error while running VoidLauncher");
}

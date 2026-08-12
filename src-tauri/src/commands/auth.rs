//! Account & authentication commands: Microsoft device-code login,
//! Elyby login, offline accounts, skin management.

use crate::accounts;
use crate::auth;
use crate::events;
use crate::AppState;
use tauri::{AppHandle, State};

// ==================== Auth Commands ====================

#[tauri::command]
pub async fn cmd_start_login(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
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
pub async fn cmd_poll_login(
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

    events::emit_log(
        &app,
        "info",
        "auth",
        &format!("Login successful: {}", profile.name),
    );
    Ok(profile)
}

#[tauri::command]
pub fn cmd_get_auth_state(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let auth = state.auth_state.lock().map_err(|e| e.to_string())?;
    // Strip tokens — only expose profile to frontend
    Ok(serde_json::json!({
        "profile": auth.profile,
        "offline_mode": auth.offline_mode,
    }))
}

/// Check if we can launch in offline mode using cached credentials
#[tauri::command]
pub fn cmd_can_launch_offline(state: State<'_, AppState>) -> Result<bool, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(auth::can_launch_offline(&config.auth_file()))
}

/// Get offline mode credentials (username and UUID) from cached auth state
#[tauri::command]
pub fn cmd_get_offline_credentials(
    state: State<'_, AppState>,
) -> Result<Option<(String, String)>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(auth::get_offline_credentials(&config.auth_file()))
}

#[tauri::command]
pub fn cmd_logout(state: State<'_, AppState>) -> Result<(), String> {
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
pub fn cmd_list_accounts(
    state: State<'_, AppState>,
) -> Result<Vec<accounts::PublicAccountEntry>, String> {
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
pub fn cmd_add_offline_account(
    state: State<'_, AppState>,
    username: String,
) -> Result<Vec<accounts::PublicAccountEntry>, String> {
    validate_offline_username(&username)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let entry = accounts::AccountEntry::new_offline(&username);
    Ok(accounts::add_account(&config.data_dir, entry)?
        .into_iter()
        .map(accounts::PublicAccountEntry::from)
        .collect())
}

/// Validate an offline-account username.
/// Rules:
///   * 3-16 characters
///   * only ASCII letters, digits, and underscores
///   * must NOT contain any Cyrillic (Russian) letters
pub(crate) fn validate_offline_username(name: &str) -> Result<(), String> {
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

#[tauri::command]
pub async fn cmd_add_elyby_account(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<Vec<accounts::PublicAccountEntry>, String> {
    let data_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.data_dir.clone()
    };
    let (name, uuid, access_token) = auth::elyby_login(&username, &password)
        .await
        .map_err(|e| e.to_string())?;
    let entry = accounts::AccountEntry::new_elyby(&name, &uuid, &access_token);
    let accounts = accounts::add_account(&data_dir, entry)?;
    Ok(accounts
        .into_iter()
        .map(accounts::PublicAccountEntry::from)
        .collect())
}

#[tauri::command]
pub fn cmd_remove_account(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<accounts::PublicAccountEntry>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(accounts::remove_account(&config.data_dir, &id)?
        .into_iter()
        .map(accounts::PublicAccountEntry::from)
        .collect())
}

#[tauri::command]
pub fn cmd_set_default_account(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<accounts::PublicAccountEntry>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(accounts::set_default_account(&config.data_dir, &id)?
        .into_iter()
        .map(accounts::PublicAccountEntry::from)
        .collect())
}

#[tauri::command]
pub async fn cmd_change_skin(
    state: State<'_, AppState>,
    account_id: String,
    skin_path: String,
    variant: String,
) -> Result<(), String> {
    // Validate skin path: must exist, be a file, and have image extension
    let skin = std::path::PathBuf::from(&skin_path);
    if !skin.exists() || !skin.is_file() {
        return Err("Invalid skin file path".to_string());
    }
    let ext = skin
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !["png", "jpg", "jpeg"].contains(&ext.as_str()) {
        return Err("Skin file must be a PNG or JPG image".to_string());
    }
    let (data_dir, account_type, mc_token) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let accounts_list = accounts::list_accounts(&config.data_dir);
        let account = accounts_list
            .iter()
            .find(|a| a.id == account_id)
            .ok_or("Account not found")?
            .clone();
        let mc_token = if account.account_type == accounts::AccountType::Microsoft {
            let auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
            auth_state
                .minecraft_token
                .as_ref()
                .map(|t| t.access_token.clone())
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
pub fn cmd_get_skin_path(
    state: State<'_, AppState>,
    account_id: String,
) -> Result<Option<String>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let accounts_list = accounts::list_accounts(&config.data_dir);
    let account = accounts_list
        .iter()
        .find(|a| a.id == account_id)
        .ok_or("Account not found")?;

    // Check if there's a skin file for this account
    let skin_path = config
        .data_dir
        .join("skins")
        .join(format!("{}.png", account.id));
    if skin_path.exists() {
        Ok(Some(skin_path.to_string_lossy().to_string()))
    } else {
        Ok(None)
    }
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
        assert!(validate_offline_username("ab").is_err()); // too short
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

//! Account & authentication commands: Microsoft device-code login,
//! Elyby login, offline accounts, skin management.

use crate::accounts;
use crate::auth;
use crate::config::AppConfig;
use crate::events;
use crate::AppState;
use tauri::{AppHandle, State};

/// Build the public (token-free) account list for the frontend. Mirrors the
/// legacy fallback of `ensure_ms_session`: a Microsoft account whose session
/// still lives only in the single global auth-state file (pre-multi-account
/// install that was never migrated) is reported as signed-in when that
/// global session matches the account's uuid — otherwise the Accounts page
/// would show a false "sign-in required" badge for an account that launches
/// just fine.
pub(crate) fn public_account_entries(config: &AppConfig) -> Vec<accounts::PublicAccountEntry> {
    let global = auth::load_auth_state(&config.auth_file());
    accounts::list_accounts(&config.data_dir)
        .into_iter()
        .map(|a| {
            let mut p = accounts::PublicAccountEntry::from(a.clone());
            if !p.has_ms_session {
                if let (Some(profile), Some(uuid)) =
                    (global.as_ref().and_then(|g| g.profile.clone()), a.uuid.as_deref())
                {
                    if profile.id == uuid {
                        p.has_ms_session = true;
                    }
                }
            }
            p
        })
        .collect()
}

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

    // Register/refresh the account entry, store the session under the
    // account's own vault slot (multiple Microsoft accounts are supported),
    // and make the freshly signed-in account the active one.
    {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let entry = accounts::upsert_microsoft_account(&config.data_dir, &profile.name, &profile.id)?;
        // A failed vault write here would leave the account entry without
        // credentials (launch would demand a re-login), so retry once.
        if let Err(e) = accounts::store_ms_session(&entry.id, &new_auth) {
            tracing::warn!(target: "launcher", "Session vault write failed ({}), retrying", e);
            accounts::store_ms_session(&entry.id, &new_auth)
                .map_err(|e| format!("Failed to store session in OS vault: {e}"))?;
        }
        accounts::set_default_account(&config.data_dir, &entry.id)?;
        auth::save_auth_state(&config.auth_file(), &new_auth).map_err(|e| e.to_string())?;
        let mut auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
        *auth_state = new_auth;
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
    // Find the account entry matching the active session and remove it
    // (remove_account also wipes its tokens from the OS vault).
    let removed_id = {
        let auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
        match auth_state.profile.as_ref() {
            Some(profile) => accounts::list_accounts(&config.data_dir)
                .iter()
                .find(|a| {
                    a.account_type == accounts::AccountType::Microsoft
                        && a.uuid.as_deref() == Some(profile.id.as_str())
                })
                .map(|a| a.id.clone()),
            None => None,
        }
    };
    if let Some(id) = removed_id {
        let _ = accounts::remove_account(&config.data_dir, &id);
    }

    // Hand the active slot to another Microsoft account if one remains
    // (prefer the account marked as default). Its tokens stay untouched.
    let next_session = accounts::list_accounts(&config.data_dir)
        .into_iter()
        .filter(|a| a.account_type == accounts::AccountType::Microsoft)
        .filter_map(|a| accounts::load_ms_session(&a.id).map(|s| (a.default, s)))
        .max_by_key(|(is_default, _)| *is_default);

    let mut auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
    match next_session {
        Some((_, session)) => {
            tracing::info!(target: "launcher", "Active Microsoft session handed to another signed-in account");
            let _ = auth::save_auth_state(&config.auth_file(), &session);
            *auth_state = session;
        }
        None => {
            *auth_state = auth::AuthState::default();
            auth::clear_auth_state(&config.auth_file());
        }
    }
    Ok(())
}

// ==================== Per-account Microsoft sessions ====================

/// Resolve the stored Microsoft session for a specific account entry,
/// refreshing it through the OAuth chain when the Minecraft token has
/// expired. The refreshed session is written back to the account's vault
/// slot; if the account is the currently active one, the global cache is
/// updated as well.
///
/// Returns an error only when the account has no stored session at all
/// (i.e. it requires a fresh device-code sign-in). A failed refresh keeps
/// the stale session so callers can fall back to offline play.
pub(crate) async fn ensure_ms_session(
    state: &AppState,
    account: &accounts::AccountEntry,
) -> Result<auth::AuthState, String> {
    let client_id = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.client_id.clone()
    };
    let data_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.data_dir.clone()
    };

    let mut session = accounts::load_ms_session(&account.id);
    if session.is_none() {
        // Legacy fallback: the single cached session, if it belongs to this account.
        let config = state.config.lock().map_err(|e| e.to_string())?;
        if let Some(global) = auth::load_auth_state(&config.auth_file()) {
            if let (Some(profile), Some(uuid)) = (&global.profile, account.uuid.as_deref()) {
                if profile.id == uuid {
                    session = Some(global);
                }
            }
        }
    }

    let Some(ref mut session) = session else {
        return Err(format!(
            "Account '{}' has no saved Microsoft sign-in. Open Accounts, press 'Add Microsoft' and sign in as '{}' to restore it.",
            account.name, account.name
        ));
    };

    // Refresh expired tokens: Microsoft -> Xbox -> XSTS -> Minecraft.
    if let (Some(mc_token), Some(ms_token)) =
        (session.minecraft_token.clone(), session.microsoft_token.clone())
    {
        if auth::is_token_expired(&mc_token) && !ms_token.refresh_token.is_empty() {
            tracing::info!(target: "launcher", "Refreshing Microsoft token for account '{}'", account.name);
            match auth::refresh_microsoft_token(&client_id, &ms_token.refresh_token).await {
                Ok(new_ms) => match auth::full_auth_flow(&new_ms).await {
                    Ok((new_mc, new_profile)) => {
                        tracing::info!(target: "launcher", "Token refreshed for user: {}", new_profile.name);
                        *session = auth::AuthState {
                            microsoft_token: Some(new_ms),
                            minecraft_token: Some(new_mc),
                            profile: Some(new_profile),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            offline_mode: false,
                        };
                        let _ = accounts::store_ms_session(&account.id, session);
                        let is_active = accounts::list_accounts(&data_dir)
                            .iter()
                            .any(|a| a.default && a.id == account.id);
                        if is_active {
                            let config = state.config.lock().map_err(|e| e.to_string())?;
                            let _ = auth::save_auth_state(&config.auth_file(), session);
                            let mut auth_state =
                                state.auth_state.lock().map_err(|e| e.to_string())?;
                            *auth_state = session.clone();
                        }
                    }
                    Err(e) => {
                        tracing::warn!(target: "launcher", "Full re-auth failed for '{}': {}", account.name, e);
                    }
                },
                Err(e) => {
                    tracing::warn!(target: "launcher", "Microsoft token refresh failed for '{}': {}", account.name, e);
                }
            }
        }
    }

    Ok(session.clone())
}

// ==================== Account Management Commands ====================

#[tauri::command]
pub fn cmd_list_accounts(
    state: State<'_, AppState>,
) -> Result<Vec<accounts::PublicAccountEntry>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    // Account list carries no secrets — tokens live in the OS credential vault
    // and are read back by the launch flow via accounts::get_elyby_token.
    Ok(public_account_entries(&config))
}

#[tauri::command]
pub fn cmd_add_offline_account(
    state: State<'_, AppState>,
    username: String,
) -> Result<Vec<accounts::PublicAccountEntry>, String> {
    validate_offline_username(&username)?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let entry = accounts::AccountEntry::new_offline(&username);
    accounts::add_account(&config.data_dir, entry)?;
    Ok(public_account_entries(&config))
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
    let entry = accounts::AccountEntry::new_elyby(&name, &uuid);
    let accounts = accounts::add_account(&data_dir, entry)?;
    let account_id = accounts.last().map(|a| a.id.clone()).ok_or("Account not saved")?;
    accounts::store_elyby_token(&account_id, &access_token).map_err(|e| e.to_string())?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(public_account_entries(&config))
}

#[tauri::command]
pub fn cmd_remove_account(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<accounts::PublicAccountEntry>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    accounts::remove_account(&config.data_dir, &id)?;
    Ok(public_account_entries(&config))
}

#[tauri::command]
pub fn cmd_set_default_account(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<accounts::PublicAccountEntry>, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let list = accounts::set_default_account(&config.data_dir, &id)?;

    // Keep the visible session in sync with the newly activated account:
    // a Microsoft account loads its own stored session; an offline/Ely.by
    // account hides the Microsoft session from the UI (its tokens remain
    // safely stored under the account's vault slot for later re-activation).
    let activated = list.iter().find(|a| a.id == id);
    match activated.map(|a| &a.account_type) {
        Some(accounts::AccountType::Microsoft) => {
            if let Some(session) = accounts::load_ms_session(&id) {
                let _ = auth::save_auth_state(&config.auth_file(), &session);
                let mut auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
                *auth_state = session;
            }
        }
        Some(_) => {
            let mut auth_state = state.auth_state.lock().map_err(|e| e.to_string())?;
            *auth_state = auth::AuthState::default();
            auth::clear_auth_state(&config.auth_file());
        }
        None => {}
    }

    Ok(public_account_entries(&config))
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
    let (data_dir, account) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let account = accounts::list_accounts(&config.data_dir)
            .into_iter()
            .find(|a| a.id == account_id)
            .ok_or("Account not found")?;
        (config.data_dir.clone(), account)
    };

    // Each Microsoft account uses its own stored session, so skins can be
    // changed for any signed-in account — not just the active one.
    if account.account_type == accounts::AccountType::Microsoft {
        let session = ensure_ms_session(state.inner(), &account).await?;
        let token = session
            .minecraft_token
            .ok_or("Not logged in with Microsoft")?
            .access_token;
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

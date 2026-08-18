use serde::{Deserialize, Serialize};
use tracing;

/// Service name used for every Windows Credential Manager entry.
/// Credentials are stored in the OS vault (DPAPI-protected, tied to the
/// current Windows user), never in files on disk.
const CM_SERVICE: &str = "VoidLauncher";

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum AccountType {
    Microsoft,
    Offline,
    ElyBy,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AccountEntry {
    pub id: String,
    pub name: String,
    pub account_type: AccountType,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub skin_variant: Option<String>,
    #[serde(default)]
    pub default: bool,
}

/// Legacy on-disk representation used to migrate old plaintext/hex files:
/// tokens used to live inside `accounts.json` next to the profile data.
/// Tokens are read from here exactly once and moved into the OS vault.
#[derive(Debug, Deserialize, Clone)]
struct LegacyAccountEntry {
    id: String,
    name: String,
    account_type: AccountType,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    elyby_token: Option<String>,
    #[serde(default)]
    skin_variant: Option<String>,
    #[serde(default)]
    default: bool,
}

/// Public, token-free view of an account. Sent to the frontend by
/// `cmd_list_accounts` so secrets never cross the bridge into the
/// renderer process. The launch flow reads tokens from the OS vault
/// via `get_account_token`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PublicAccountEntry {
    pub id: String,
    pub name: String,
    pub account_type: AccountType,
    pub uuid: Option<String>,
    pub skin_variant: Option<String>,
    pub default: bool,
}

impl From<AccountEntry> for PublicAccountEntry {
    fn from(a: AccountEntry) -> Self {
        Self {
            id: a.id,
            name: a.name,
            account_type: a.account_type,
            uuid: a.uuid,
            skin_variant: a.skin_variant,
            default: a.default,
        }
    }
}

impl AccountEntry {
    pub fn new_offline(name: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            account_type: AccountType::Offline,
            uuid: Some(uuid::Uuid::new_v4().to_string()),
            skin_variant: None,
            default: false,
        }
    }

    pub fn new_elyby(name: &str, uuid: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            account_type: AccountType::ElyBy,
            uuid: Some(uuid.to_string()),
            skin_variant: None,
            default: false,
        }
    }

    pub fn new_microsoft(name: &str, uuid: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            account_type: AccountType::Microsoft,
            uuid: Some(uuid.to_string()),
            skin_variant: None,
            default: false,
        }
    }
}

// ==================== OS credential vault ====================

fn vault_set(user: &str, value: &str) -> Result<(), String> {
    keyring::Entry::new(CM_SERVICE, user)
        .map_err(|e| format!("vault init failed: {e}"))?
        .set_password(value)
        .map_err(|e| format!("vault write failed: {e}"))
}

fn vault_get(user: &str) -> Option<String> {
    match keyring::Entry::new(CM_SERVICE, user)
        .and_then(|e| e.get_password())
    {
        Ok(v) => Some(v),
        Err(_) => None,
    }
}

fn vault_delete(user: &str) {
    let _ = keyring::Entry::new(CM_SERVICE, user).and_then(|e| e.delete_credential());
}

fn elyby_key(account_id: &str) -> String {
    format!("account:{account_id}:elyby")
}

/// Persist an Ely.by access token in the OS credential vault.
pub fn store_elyby_token(account_id: &str, token: &str) -> Result<(), String> {
    vault_set(&elyby_key(account_id), token)
}

/// Read an Ely.by access token from the OS credential vault.
pub fn get_elyby_token(account_id: &str) -> Option<String> {
    vault_get(&elyby_key(account_id))
}

/// Remove every vault entry belonging to an account.
pub fn delete_account_tokens(account_id: &str) {
    vault_delete(&elyby_key(account_id));
}

// ==================== accounts.json (non-secret profile data) ====================

pub fn list_accounts(accounts_dir: &std::path::Path) -> Vec<AccountEntry> {
    let path = accounts_dir.join("accounts.json");
    if !path.exists() {
        return Vec::new();
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    // 1) Try the legacy layout (with embedded tokens) → migrate tokens to the
    //    OS vault, then rewrite the file without any secrets. The file is only
    //    rewritten when EVERY token was moved successfully — otherwise we
    //    leave it untouched rather than risk losing credentials.
    if let Some(mut legacy) = parse_legacy_accounts(&content) {
        let mut dirty = false;
        let mut all_migrated = true;
        for acc in legacy.iter_mut() {
            if acc.elyby_token.is_some() || acc.access_token.is_some() {
                let token = acc.elyby_token.take().or_else(|| acc.access_token.take());
                if let Some(token) = token {
                    match store_elyby_token(&acc.id, &token) {
                        Ok(()) => dirty = true,
                        Err(e) => {
                            tracing::warn!(target: "launcher", "Vault migration failed for account {}: {}", acc.id, e);
                            all_migrated = false;
                        }
                    }
                }
            }
        }
        let accounts: Vec<AccountEntry> = legacy
            .into_iter()
            .map(|a| AccountEntry {
                id: a.id,
                name: a.name,
                account_type: a.account_type,
                uuid: a.uuid,
                skin_variant: a.skin_variant,
                default: a.default,
            })
            .collect();
        if dirty && all_migrated {
            let _ = save_accounts(accounts_dir, &accounts);
        }
        return accounts;
    }

    // 2) Plain JSON (current format, no tokens).
    if let Ok(accounts) = serde_json::from_str(&content) {
        return accounts;
    }
    // 3) Hex-encoded JSON (older format).
    if let Ok(bytes) = hex::decode(content.trim()) {
        if let Ok(accounts) = serde_json::from_slice(&bytes) {
            return accounts;
        }
    }
    Vec::new()
}

fn parse_legacy_accounts(content: &str) -> Option<Vec<LegacyAccountEntry>> {
    if let Ok(accounts) = serde_json::from_str::<Vec<LegacyAccountEntry>>(content) {
        return Some(accounts);
    }
    let bytes = hex::decode(content.trim()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save_accounts(accounts_dir: &std::path::Path, accounts: &[AccountEntry]) -> Result<(), String> {
    let path = accounts_dir.join("accounts.json");
    if let Err(e) = std::fs::create_dir_all(accounts_dir) {
        tracing::warn!(target: "launcher", "Failed to create accounts directory: {}", e);
        return Err(e.to_string());
    }
    let json = serde_json::to_string(accounts).map_err(|e| {
        tracing::warn!(target: "launcher", "Failed to serialize accounts: {}", e);
        e.to_string()
    })?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| {
        tracing::warn!(target: "launcher", "Failed to write accounts temp file: {}", e);
        e.to_string()
    })?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        tracing::warn!(target: "launcher", "Failed to rename accounts file: {}", e);
        e.to_string()
    })?;
    Ok(())
}

pub fn add_account(accounts_dir: &std::path::Path, entry: AccountEntry) -> Result<Vec<AccountEntry>, String> {
    tracing::info!(target: "launcher", "Adding {:?} account: {}", entry.account_type, entry.name);
    let mut accounts = list_accounts(accounts_dir);
    accounts.push(entry);
    save_accounts(accounts_dir, &accounts)?;
    Ok(accounts)
}

pub fn remove_account(accounts_dir: &std::path::Path, id: &str) -> Result<Vec<AccountEntry>, String> {
    tracing::info!(target: "launcher", "Removing account with id: {}", id);
    delete_account_tokens(id);
    let mut accounts = list_accounts(accounts_dir);
    accounts.retain(|a| a.id != id);
    save_accounts(accounts_dir, &accounts)?;
    Ok(accounts)
}

/// Update or insert a Microsoft account entry (matches by account_type + uuid)
pub fn upsert_microsoft_account(accounts_dir: &std::path::Path, name: &str, uuid: &str) -> Result<Vec<AccountEntry>, String> {
    let mut accounts = list_accounts(accounts_dir);
    // Look for existing Microsoft account with same UUID
    let existing_idx = accounts.iter().position(|a| a.account_type == AccountType::Microsoft && a.uuid.as_deref() == Some(uuid));
    if let Some(idx) = existing_idx {
        let entry = &mut accounts[idx];
        entry.name = name.to_string();
    } else {
        let entry = AccountEntry::new_microsoft(name, uuid);
        // If no accounts exist, make this one the default
        let is_first = accounts.is_empty();
        let entry = AccountEntry {
            default: is_first,
            ..entry
        };
        accounts.push(entry);
    }
    save_accounts(accounts_dir, &accounts)?;
    Ok(accounts)
}

/// Remove the Microsoft account with the given UUID
pub fn remove_microsoft_account(accounts_dir: &std::path::Path, uuid: &str) -> Result<Vec<AccountEntry>, String> {
    let mut accounts = list_accounts(accounts_dir);
    accounts.retain(|a| !(a.account_type == AccountType::Microsoft && a.uuid.as_deref() == Some(uuid)));
    save_accounts(accounts_dir, &accounts)?;
    Ok(accounts)
}

pub fn set_default_account(accounts_dir: &std::path::Path, id: &str) -> Result<Vec<AccountEntry>, String> {
    tracing::info!(target: "launcher", "Setting default account to id: {}", id);
    let mut accounts = list_accounts(accounts_dir);
    for a in &mut accounts {
        a.default = a.id == id;
    }
    save_accounts(accounts_dir, &accounts)?;
    Ok(accounts)
}

#[allow(dead_code)]
pub fn get_default_account(accounts_dir: &std::path::Path) -> Option<AccountEntry> {
    let accounts = list_accounts(accounts_dir);
    accounts.into_iter().find(|a| a.default).or_else(|| {
        let accounts = list_accounts(accounts_dir);
        accounts.into_iter().next()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_plain_with_tokens_is_parsed() {
        let json = r#"[{"id":"a1","name":"Steve","account_type":"ElyBy","uuid":"u1","access_token":"tok1","elyby_token":"tok1","default":true}]"#;
        let legacy = parse_legacy_accounts(json).expect("legacy plain must parse");
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].elyby_token.as_deref(), Some("tok1"));
        assert_eq!(legacy[0].access_token.as_deref(), Some("tok1"));
    }

    #[test]
    fn legacy_hex_with_tokens_is_parsed() {
        let json = r#"[{"id":"a1","name":"Steve","account_type":"ElyBy","uuid":"u1","access_token":"tok1","elyby_token":"tok1"}]"#;
        let hexed = hex::encode(json.as_bytes());
        let legacy = parse_legacy_accounts(&hexed).expect("legacy hex must parse");
        assert_eq!(legacy[0].elyby_token.as_deref(), Some("tok1"));
    }

    #[test]
    fn current_format_without_tokens_is_parsed() {
        let json = r#"[{"id":"a1","name":"Steve","account_type":"Offline","uuid":"u1"}]"#;
        let legacy = parse_legacy_accounts(json).expect("current format must parse");
        assert_eq!(legacy[0].elyby_token, None);
        assert_eq!(legacy[0].default, false);
    }

    #[test]
    fn non_account_json_is_not_parsed() {
        assert!(parse_legacy_accounts("not json at all").is_none());
        assert!(parse_legacy_accounts("{}").is_none());
    }

    #[test]
    fn public_view_strips_nothing_secret() {
        let entry = AccountEntry::new_offline("Alex");
        let public = PublicAccountEntry::from(entry);
        assert_eq!(public.name, "Alex");
    }
}

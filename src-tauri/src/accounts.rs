use serde::{Deserialize, Serialize};
use tracing;
use hex;

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
    pub uuid: Option<String>,
    pub access_token: Option<String>,
    pub elyby_token: Option<String>,
    pub skin_variant: Option<String>,
    pub default: bool,
}

/// Public, token-free view of an account. Sent to the frontend by
/// `cmd_list_accounts` so access tokens never cross the bridge into the
/// renderer process. The launch flow reads `access_token` / `elyby_token`
/// directly from `accounts.json` on disk via `accounts::list_accounts`.
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
            access_token: None,
            elyby_token: None,
            skin_variant: None,
            default: false,
        }
    }

    pub fn new_elyby(name: &str, uuid: &str, access_token: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            account_type: AccountType::ElyBy,
            uuid: Some(uuid.to_string()),
            access_token: Some(access_token.to_string()),
            elyby_token: Some(access_token.to_string()),
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
            access_token: None,
            elyby_token: None,
            skin_variant: None,
            default: false,
        }
    }
}

pub fn list_accounts(accounts_dir: &std::path::Path) -> Vec<AccountEntry> {
    let path = accounts_dir.join("accounts.json");
    if !path.exists() {
        return Vec::new();
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    // Try plain JSON first (backward compat with old plaintext files)
    if let Ok(accounts) = serde_json::from_str(&content) {
        return accounts;
    }
    // Try hex-encoded JSON
    if let Ok(bytes) = hex::decode(content.trim()) {
        if let Ok(accounts) = serde_json::from_slice(&bytes) {
            return accounts;
        }
    }
    Vec::new()
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
    let encoded = hex::encode(json.as_bytes());
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &encoded).map_err(|e| {
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

use serde::{Deserialize, Serialize};

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
    /// Microsoft only: whether the account has a stored session. A
    /// Microsoft account without one cannot launch until the user signs
    /// in again (the sign-in targets the same entry by uuid).
    #[serde(default)]
    pub has_ms_session: bool,
}

impl From<AccountEntry> for PublicAccountEntry {
    fn from(a: AccountEntry) -> Self {
        let has_ms_session =
            a.account_type == AccountType::Microsoft && load_ms_session(&a.id).is_some();
        Self {
            id: a.id,
            name: a.name,
            account_type: a.account_type,
            uuid: a.uuid,
            skin_variant: a.skin_variant,
            default: a.default,
            has_ms_session,
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

/// Windows Credential Manager caps a single password blob at 2560 bytes,
/// which the keyring crate fills as UTF-16 (~1280 chars). A serialized
/// Microsoft session (OAuth + Minecraft tokens + profile) easily exceeds
/// that, so values are split across numbered entries `<key>#0`, `<key>#1`, ...
const VAULT_CHUNK_CHARS: usize = 1024;
/// Upper bound on chunks scanned on read/delete (32 KB is far more than any
/// session needs); also stops the scan loop from running away.
const VAULT_MAX_CHUNKS: usize = 32;

fn chunk_key(user: &str, index: usize) -> String {
    format!("{user}#{index}")
}

/// Split `value` into vault-sized chunks. Always returns at least one chunk
/// (possibly empty) so a written marker entry exists even for blank values.
fn split_into_chunks(value: &str) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    let chunk_count = chars.len().div_ceil(VAULT_CHUNK_CHARS).max(1);
    (0..chunk_count)
        .map(|i| {
            let start = i * VAULT_CHUNK_CHARS;
            let end = (start + VAULT_CHUNK_CHARS).min(chars.len());
            chars[start..end].iter().collect()
        })
        .collect()
}

/// Minimal key/value interface in front of the OS credential vault.
///
/// Production uses [`OsCredentialStore`] (Windows Credential Manager through
/// `keyring`). The commit/recovery protocol below is security-sensitive, so it
/// is kept generic over this trait: tests drive it with an in-memory fake and
/// can inject read/write failures deterministically. That fake does NOT test
/// the real Windows vault — it tests the protocol.
trait CredentialStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    fn delete(&self, key: &str) -> Result<(), String>;
}

struct OsCredentialStore;

impl CredentialStore for OsCredentialStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let entry =
            keyring::Entry::new(CM_SERVICE, key).map_err(|e| format!("vault init failed: {e}"))?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("vault read failed: {e}")),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        keyring::Entry::new(CM_SERVICE, key)
            .map_err(|e| format!("vault init failed: {e}"))?
            .set_password(value)
            .map_err(|e| format!("vault write failed: {e}"))
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        let entry =
            keyring::Entry::new(CM_SERVICE, key).map_err(|e| format!("vault init failed: {e}"))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("vault delete failed: {e}")),
        }
    }
}

fn vault_store() -> &'static OsCredentialStore {
    static STORE: OsCredentialStore = OsCredentialStore;
    &STORE
}

/// Per-slot lock registry. Concurrent login/refresh/delete operations used to
/// read the marker, write a generation and roll the marker back without any
/// coordination, so one writer could delete the generation another one had
/// just referenced. Locking is per vault key: unrelated accounts never wait on
/// each other, and a single operation never holds two key locks (no deadlock).
fn vault_lock(user: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.entry(user.to_string()).or_default().clone()
}

fn with_key_lock<T>(user: &str, f: impl FnOnce() -> T) -> T {
    let lock = vault_lock(user);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f()
}

/// Clamp a chunk count to the maximum the scan loop will accept.
fn vault_chunk_count(chunks: usize) -> usize {
    chunks.min(VAULT_MAX_CHUNKS)
}

/// Marker entry that commits one chunk generation. Format:
/// `generation:chunk_count[,generation:chunk_count...]` — the first entry is
/// the active generation, the rest are kept during a transition so a crash
/// can still fall back to the previous session. A leading `!` means the write
/// is still pending: `!<generation>:<count>|<previous committed body>`.
fn vault_marker_key(user: &str) -> String {
    format!("{user}#current")
}

fn vault_gen_chunk_key(user: &str, generation: &str, index: usize) -> String {
    format!("{user}#g{generation}#{index}")
}

fn parse_vault_generations(raw: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let Some((id, count)) = entry.trim().split_once(':') else {
            continue;
        };
        let count = match count.parse::<usize>() {
            Ok(c) if (1..=VAULT_MAX_CHUNKS).contains(&c) => c,
            _ => continue,
        };
        let id_ok = !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if id_ok {
            out.push((id.to_string(), count));
        }
    }
    out
}

fn format_vault_generation_list(generations: &[(String, usize)]) -> String {
    generations
        .iter()
        .map(|(id, count)| format!("{id}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn format_vault_generations(
    current: &str,
    current_chunks: usize,
    previous: &[(String, usize)],
) -> String {
    let mut parts = vec![(current.to_string(), current_chunks)];
    for (id, count) in previous {
        if id != current {
            parts.push((id.clone(), *count));
        }
    }
    format_vault_generation_list(&parts)
}

/// Split a pending marker into the half-written generation spec and the body
/// of the last committed marker. Returns `None` for a committed marker.
fn parse_pending_marker(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix('!')?;
    Some(rest.split_once('|').unwrap_or((rest, "")))
}

fn parse_single_generation(spec: &str) -> Option<(String, usize)> {
    parse_vault_generations(spec).into_iter().next()
}

fn read_marker_raw(store: &dyn CredentialStore, user: &str) -> Result<Option<String>, String> {
    store.get(&vault_marker_key(user))
}

fn write_vault_marker(store: &dyn CredentialStore, user: &str, marker: &str) -> Result<(), String> {
    store.set(&vault_marker_key(user), marker)
}

/// Put the previous committed marker back after a failed write. Best effort:
/// the caller is already returning the original failure.
fn restore_vault_marker(store: &dyn CredentialStore, user: &str, previous_body: &str) {
    let result = if previous_body.is_empty() {
        store.delete(&vault_marker_key(user))
    } else {
        write_vault_marker(store, user, previous_body)
    };
    if let Err(e) = result {
        tracing::warn!(
            target: "launcher",
            "Vault marker restore failed for '{}': {}",
            user, e
        );
    }
}

fn read_vault_generation(
    store: &dyn CredentialStore,
    user: &str,
    generation: &str,
    chunks: usize,
) -> Option<String> {
    let chunks = vault_chunk_count(chunks);
    if chunks == 0 {
        return None;
    }
    let mut parts = Vec::with_capacity(chunks);
    for i in 0..chunks {
        match store.get(&vault_gen_chunk_key(user, generation, i)) {
            Ok(Some(part)) => parts.push(part),
            _ => return None,
        }
    }
    Some(parts.concat())
}

fn delete_vault_generation(
    store: &dyn CredentialStore,
    user: &str,
    generation: &str,
    chunks: usize,
) {
    for i in 0..vault_chunk_count(chunks) {
        if let Err(e) = store.delete(&vault_gen_chunk_key(user, generation, i)) {
            tracing::warn!(
                target: "launcher",
                "Failed to delete vault chunk {}/{} for '{}': {}",
                i, chunks, user, e
            );
        }
    }
}

/// Finish an interrupted write. The pending marker is written before the first
/// chunk, so a crash can leave: (a) only the marker, (b) the marker plus some
/// chunks, or (c) all chunks but the marker still pending. In every case the
/// half-written generation is deleted and the previous committed body is
/// restored. The new generation is never exposed: it was never committed.
fn recover_pending_marker(store: &dyn CredentialStore, user: &str) -> Result<(), String> {
    let Some(raw) = read_marker_raw(store, user)? else {
        return Ok(());
    };
    let Some((spec, previous)) = parse_pending_marker(&raw) else {
        return Ok(());
    };

    // Delete chunks first: if this process dies right here, the pending marker
    // remains and the next recovery simply finds the chunks missing.
    if let Some((generation, chunks)) = parse_single_generation(spec) {
        delete_vault_generation(store, user, &generation, chunks);
    } else {
        tracing::warn!(
            target: "launcher",
            "Ignoring corrupt pending vault marker for '{}'",
            user
        );
    }
    if previous.is_empty() {
        store.delete(&vault_marker_key(user))
    } else {
        store.set(&vault_marker_key(user), previous)
    }
}

/// Read the value of the last committed generation. A pending marker whose
/// cleanup failed must never expose uncommitted chunks, so only its previous
/// body is considered.
fn read_active_value(store: &dyn CredentialStore, user: &str) -> Result<Option<String>, String> {
    let Some(raw) = read_marker_raw(store, user)? else {
        return read_legacy_value(store, user);
    };
    if raw.trim().is_empty() {
        return read_legacy_value(store, user);
    }
    let committed = match parse_pending_marker(&raw) {
        Some((_, previous)) => previous,
        None => raw.as_str(),
    };
    for (generation, chunks) in parse_vault_generations(committed) {
        if let Some(value) = read_vault_generation(store, user, &generation, chunks) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// Pre-generation layouts: numbered `#0..#N` chunks and the oldest single
/// entry. Kept for accounts written by older VoidLauncher releases.
fn read_legacy_value(store: &dyn CredentialStore, user: &str) -> Result<Option<String>, String> {
    let mut parts: Vec<String> = Vec::new();
    while parts.len() < VAULT_MAX_CHUNKS {
        match store.get(&chunk_key(user, parts.len())) {
            Ok(Some(part)) => parts.push(part),
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(
                    target: "launcher",
                    "Legacy vault chunk read failed for '{}': {}",
                    user, e
                );
                break;
            }
        }
    }
    if !parts.is_empty() {
        return Ok(Some(parts.concat()));
    }
    store.get(user)
}

fn vault_set(user: &str, value: &str) -> Result<(), String> {
    vault_set_with(vault_store(), user, value)
}

fn vault_set_with(store: &dyn CredentialStore, user: &str, value: &str) -> Result<(), String> {
    with_key_lock(user, || vault_set_locked(store, user, value))
}

fn vault_set_locked(store: &dyn CredentialStore, user: &str, value: &str) -> Result<(), String> {
    // Roll back any interrupted writer before looking at the committed set.
    recover_pending_marker(store, user)?;

    let previous_marker = read_marker_raw(store, user)?.unwrap_or_default();
    let mut previous: Vec<(String, usize)> = Vec::new();
    for (generation, chunks) in parse_vault_generations(&previous_marker) {
        if previous.iter().any(|(id, _)| id == &generation) {
            continue;
        }
        // Only readable generations may be used as a rollback target: a marker
        // that already points at deleted chunks must be dropped, not restored.
        if read_vault_generation(store, user, &generation, chunks).is_some() {
            previous.push((generation, chunks));
        } else {
            tracing::warn!(
                target: "launcher",
                "Vault marker for '{}' references a missing generation; dropping it",
                user
            );
        }
    }
    let previous_body = format_vault_generation_list(&previous);

    let generation = uuid::Uuid::new_v4().simple().to_string();
    let chunks = split_into_chunks(value);

    // Publish the pending marker before the first chunk. A crash after this
    // point is recoverable by `recover_pending_marker`, including the chunks
    // that would otherwise stay orphaned in Credential Manager forever.
    let pending = format!(
        "!{}|{}",
        format_vault_generations(&generation, chunks.len(), &[]),
        previous_body
    );
    write_vault_marker(store, user, &pending).map_err(|e| format!("vault commit failed: {e}"))?;

    for (i, part) in chunks.iter().enumerate() {
        if let Err(e) = store.set(&vault_gen_chunk_key(user, &generation, i), part) {
            delete_vault_generation(store, user, &generation, chunks.len());
            restore_vault_marker(store, user, &previous_body);
            return Err(e);
        }
    }

    let committed = format_vault_generations(&generation, chunks.len(), &previous);
    if let Err(e) = write_vault_marker(store, user, &committed) {
        delete_vault_generation(store, user, &generation, chunks.len());
        restore_vault_marker(store, user, &previous_body);
        return Err(format!("vault commit failed: {e}"));
    }

    // Read back through the same path readers use. If the store accepted the
    // write but returns something else, roll the marker back to a generation
    // that is still readable.
    if !matches!(
        read_active_value(store, user),
        Ok(Some(ref observed)) if observed == value
    ) {
        delete_vault_generation(store, user, &generation, chunks.len());
        restore_vault_marker(store, user, &previous_body);
        return Err("vault verification failed after write".to_string());
    }

    // New generation is live: drop the previous chunks and the pre-chunking
    // layouts, then compact the marker so it cannot reference deleted data.
    for (old_generation, old_chunks) in &previous {
        delete_vault_generation(store, user, old_generation, *old_chunks);
    }
    for i in 0..VAULT_MAX_CHUNKS {
        let _ = store.delete(&chunk_key(user, i));
    }
    let _ = store.delete(user);
    if let Err(e) = write_vault_marker(
        store,
        user,
        &format_vault_generations(&generation, chunks.len(), &[]),
    ) {
        // The expanded marker still references the deleted old chunks; readers
        // skip unreadable entries and the next write compacts it again.
        tracing::warn!(
            target: "launcher",
            "Vault marker compaction failed for '{}': {}",
            user, e
        );
    }

    Ok(())
}

/// Retry a vault write once. Credential Manager can transiently fail while a
/// logout/cleanup from another process holds the same entry; losing a freshly
/// refreshed session because of one failed write is worse than a second try.
fn vault_set_retry(
    store: &dyn CredentialStore,
    user: &str,
    value: &str,
    attempts: usize,
) -> Result<(), String> {
    let attempts = attempts.max(1);
    let mut last_error = String::new();
    for attempt in 0..attempts {
        match vault_set_with(store, user, value) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(
                    target: "launcher",
                    "Vault write for '{}' failed (attempt {}/{})",
                    user,
                    attempt + 1,
                    attempts
                );
                last_error = e;
            }
        }
    }
    Err(last_error)
}

fn vault_get(user: &str) -> Option<String> {
    vault_get_with(vault_store(), user)
}

fn vault_get_with(store: &dyn CredentialStore, user: &str) -> Option<String> {
    with_key_lock(user, || {
        if let Err(e) = recover_pending_marker(store, user) {
            tracing::warn!(
                target: "launcher",
                "Vault recovery for '{}' failed: {}",
                user, e
            );
        }
        match read_active_value(store, user) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(
                    target: "launcher",
                    "Vault read for '{}' failed: {}",
                    user, e
                );
                None
            }
        }
    })
}

fn vault_delete(user: &str) {
    vault_delete_with(vault_store(), user)
}

fn vault_delete_with(store: &dyn CredentialStore, user: &str) {
    with_key_lock(user, || {
        if let Err(e) = recover_pending_marker(store, user) {
            tracing::warn!(
                target: "launcher",
                "Vault recovery before delete failed for '{}': {}",
                user, e
            );
        }
        match read_marker_raw(store, user) {
            Ok(Some(raw)) => {
                let committed = match parse_pending_marker(&raw) {
                    Some((spec, previous)) => {
                        // Recovery may have failed; still delete the pending
                        // generation named by the marker.
                        if let Some((generation, chunks)) = parse_single_generation(spec) {
                            delete_vault_generation(store, user, &generation, chunks);
                        }
                        previous
                    }
                    None => raw.as_str(),
                };
                for (generation, chunks) in parse_vault_generations(committed) {
                    delete_vault_generation(store, user, &generation, chunks);
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(
                target: "launcher",
                "Vault marker read before delete failed for '{}': {}",
                user, e
            ),
        }
        if let Err(e) = store.delete(&vault_marker_key(user)) {
            tracing::warn!(
                target: "launcher",
                "Vault marker delete failed for '{}': {}",
                user, e
            );
        }
        let _ = store.delete(user);
        for i in 0..VAULT_MAX_CHUNKS {
            let _ = store.delete(&chunk_key(user, i));
        }
    });
}

/// Store the global Microsoft auth-state blob (shared legacy slot).
pub fn store_auth_state_blob(json: &str) -> Result<(), String> {
    vault_set_retry(vault_store(), "auth-state", json, 2)
}

/// Read the global Microsoft auth-state blob (shared legacy slot).
pub fn load_auth_state_blob() -> Option<String> {
    vault_get("auth-state")
}

/// Remove the global Microsoft auth-state blob.
pub fn clear_auth_state_blob() {
    vault_delete("auth-state");
}

fn elyby_key(account_id: &str) -> String {
    format!("account:{account_id}:elyby")
}

/// Vault slot holding a Microsoft account's own session (OAuth tokens +
/// Minecraft token + profile). One slot per account enables any number of
/// simultaneously signed-in Microsoft accounts.
fn ms_session_key(account_id: &str) -> String {
    format!("account:{account_id}:msauth")
}

/// Persist an Ely.by access token in the OS credential vault.
pub fn store_elyby_token(account_id: &str, token: &str) -> Result<(), String> {
    vault_set(&elyby_key(account_id), token)
}

/// Read an Ely.by access token from the OS credential vault.
/// Used by the launch flow (`cmd_launch_game`) as `--accessToken`.
pub fn get_elyby_token(account_id: &str) -> Option<String> {
    vault_get(&elyby_key(account_id))
}

/// Persist a Microsoft account's session in the OS credential vault.
/// Uses one retry so a transient Credential Manager failure during a token
/// refresh does not silently throw away the new tokens.
pub fn store_ms_session(account_id: &str, state: &crate::auth::AuthState) -> Result<(), String> {
    let json = serde_json::to_string(state).map_err(|e| e.to_string())?;
    vault_set_retry(vault_store(), &ms_session_key(account_id), &json, 2)
}

/// Load a Microsoft account's session from the OS credential vault.
pub fn load_ms_session(account_id: &str) -> Option<crate::auth::AuthState> {
    let json = vault_get(&ms_session_key(account_id))?;
    serde_json::from_str(&json).ok()
}

/// Remove every vault entry belonging to an account.
pub fn delete_account_tokens(account_id: &str) {
    vault_delete(&elyby_key(account_id));
    vault_delete(&ms_session_key(account_id));
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

pub fn save_accounts(
    accounts_dir: &std::path::Path,
    accounts: &[AccountEntry],
) -> Result<(), String> {
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

pub fn add_account(
    accounts_dir: &std::path::Path,
    entry: AccountEntry,
) -> Result<Vec<AccountEntry>, String> {
    tracing::info!(target: "launcher", "Adding {:?} account: {}", entry.account_type, entry.name);
    let mut accounts = list_accounts(accounts_dir);
    accounts.push(entry);
    save_accounts(accounts_dir, &accounts)?;
    Ok(accounts)
}

pub fn remove_account(
    accounts_dir: &std::path::Path,
    id: &str,
) -> Result<Vec<AccountEntry>, String> {
    tracing::info!(target: "launcher", "Removing account with id: {}", id);
    let mut accounts = list_accounts(accounts_dir);
    let removed_was_default = accounts
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.default)
        .unwrap_or(false);
    accounts.retain(|a| a.id != id);

    // Keep the list self-consistent: if the removed account was the default,
    // promote the next account before writing. Doing this in the same atomic
    // save (instead of a second write) avoids a window where no account is
    // marked as default.
    if removed_was_default {
        if let Some(next) = accounts.first_mut() {
            next.default = true;
        }
    }

    // Persist the account list first. If this fails, the account must remain
    // usable: deleting its vault tokens before the save would leave the
    // still-listed account permanently signed out.
    save_accounts(accounts_dir, &accounts)?;
    delete_account_tokens(id);
    Ok(accounts)
}

/// Whether `accounts.json` already exists. Used by startup migration: a
/// missing file means this is a legacy single-session install; an existing
/// file is the source of truth and must never be silently extended from a
/// stale cached session (that resurrected deleted accounts).
pub fn accounts_file_exists(accounts_dir: &std::path::Path) -> bool {
    accounts_dir.join("accounts.json").is_file()
}

/// Whether the existing `accounts.json` can be parsed in any supported
/// (current, legacy or hex) layout. A corrupt file must not be treated as an
/// empty account list when deciding to discard a cached session.
pub fn accounts_file_is_parseable(accounts_dir: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(accounts_dir.join("accounts.json")) else {
        return false;
    };
    if parse_legacy_accounts(&content).is_some()
        || serde_json::from_str::<Vec<AccountEntry>>(&content).is_ok()
    {
        return true;
    }
    hex::decode(content.trim())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<AccountEntry>>(&bytes).ok())
        .is_some()
}

/// Update or insert a Microsoft account entry (matches by account_type + uuid)
/// and return the stored entry (its `id` keys the per-account vault slots).
pub fn upsert_microsoft_account(
    accounts_dir: &std::path::Path,
    name: &str,
    uuid: &str,
) -> Result<AccountEntry, String> {
    let mut accounts = list_accounts(accounts_dir);
    // Look for existing Microsoft account with same UUID
    let existing_idx = accounts
        .iter()
        .position(|a| a.account_type == AccountType::Microsoft && a.uuid.as_deref() == Some(uuid));
    let entry = match existing_idx {
        Some(idx) => {
            accounts[idx].name = name.to_string();
            accounts[idx].clone()
        }
        None => {
            let entry = AccountEntry::new_microsoft(name, uuid);
            // Make it the default when no other account is marked as such.
            let has_default = accounts.iter().any(|a| a.default);
            let entry = AccountEntry {
                default: !has_default,
                ..entry
            };
            accounts.push(entry.clone());
            entry
        }
    };
    save_accounts(accounts_dir, &accounts)?;
    Ok(entry)
}

pub fn set_default_account(
    accounts_dir: &std::path::Path,
    id: &str,
) -> Result<Vec<AccountEntry>, String> {
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
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn temp_accounts_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_accounts_test_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// In-memory stand-in for Windows Credential Manager. It exists only to
    /// exercise the commit/recovery protocol with deterministic failures; it
    /// is not evidence that the real OS vault behaves the same way.
    #[derive(Default)]
    struct FakeStore {
        data: Mutex<HashMap<String, String>>,
        set_rules: Mutex<Vec<FailRule>>,
        get_rules: Mutex<Vec<FailRule>>,
    }

    #[derive(Clone)]
    struct FailRule {
        needle: String,
        /// Matching calls that succeed before failures start.
        skip: usize,
        /// Number of calls to fail.
        failures: usize,
    }

    impl FailRule {
        fn hit(&mut self, key: &str) -> bool {
            if !key.contains(&self.needle) {
                return false;
            }
            if self.skip > 0 {
                self.skip -= 1;
                return false;
            }
            if self.failures == 0 {
                return false;
            }
            self.failures -= 1;
            true
        }

        fn expired(&self) -> bool {
            self.skip == 0 && self.failures == 0
        }
    }

    fn should_fail(rules: &Mutex<Vec<FailRule>>, key: &str) -> bool {
        let mut rules = rules.lock().unwrap();
        let mut failed = false;
        for rule in rules.iter_mut() {
            if rule.hit(key) {
                failed = true;
                break;
            }
        }
        rules.retain(|rule| !rule.expired());
        failed
    }

    impl FakeStore {
        fn new() -> Self {
            Self::default()
        }

        fn fail_sets(&self, needle: &str, skip: usize, failures: usize) {
            self.set_rules.lock().unwrap().push(FailRule {
                needle: needle.to_string(),
                skip,
                failures,
            });
        }

        fn fail_gets(&self, needle: &str, skip: usize, failures: usize) {
            self.get_rules.lock().unwrap().push(FailRule {
                needle: needle.to_string(),
                skip,
                failures,
            });
        }

        fn set_raw(&self, key: &str, value: &str) {
            self.data
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
        }

        fn raw(&self, key: &str) -> Option<String> {
            self.data.lock().unwrap().get(key).cloned()
        }

        fn keys(&self) -> Vec<String> {
            let mut keys: Vec<String> = self.data.lock().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        }

        fn count_with(&self, needle: &str) -> usize {
            self.data
                .lock()
                .unwrap()
                .keys()
                .filter(|key| key.contains(needle))
                .count()
        }
    }

    impl CredentialStore for FakeStore {
        fn get(&self, key: &str) -> Result<Option<String>, String> {
            if should_fail(&self.get_rules, key) {
                return Err("injected read failure".to_string());
            }
            Ok(self.data.lock().unwrap().get(key).cloned())
        }

        fn set(&self, key: &str, value: &str) -> Result<(), String> {
            if should_fail(&self.set_rules, key) {
                return Err("injected write failure".to_string());
            }
            self.data
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, key: &str) -> Result<(), String> {
            self.data.lock().unwrap().remove(key);
            Ok(())
        }
    }

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
        assert!(!legacy[0].default);
    }

    #[test]
    fn non_account_json_is_not_parsed() {
        assert!(parse_legacy_accounts("not json at all").is_none());
        assert!(parse_legacy_accounts("{}").is_none());
    }

    #[test]
    fn remove_account_drops_only_target_and_promotes_new_default() {
        let dir = temp_accounts_dir("remove");
        let mut first = AccountEntry::new_microsoft("First", "uuid-first");
        first.default = true;
        let second = AccountEntry::new_offline("Second");
        save_accounts(&dir, &[first.clone(), second.clone()]).unwrap();

        let remaining = remove_account(&dir, &first.id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, second.id);
        assert!(
            remaining[0].default,
            "remaining account must become default"
        );

        // The persistent list must be the same after a simulated restart.
        let reread = list_accounts(&dir);
        assert_eq!(reread.len(), 1);
        assert_eq!(reread[0].id, second.id);
        assert!(reread[0].default);
        assert!(!reread.iter().any(|a| a.id == first.id));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_account_keeps_the_list_when_the_save_fails() {
        let dir = temp_accounts_dir("remove_save_fail");
        let first = AccountEntry::new_microsoft("First", "uuid-first");
        let second = AccountEntry::new_offline("Second");
        save_accounts(&dir, &[first.clone(), second.clone()]).unwrap();
        // A directory at the temp-file path makes the atomic write fail.
        std::fs::create_dir(dir.join("accounts.json.tmp")).unwrap();

        assert!(remove_account(&dir, &first.id).is_err());
        let list = list_accounts(&dir);
        assert_eq!(list.len(), 2, "failed save must not drop the account");
        assert!(list.iter().any(|a| a.id == first.id));
        assert!(list.iter().any(|a| a.id == second.id));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accounts_file_state_distinguishes_absent_and_corrupt() {
        let dir = temp_accounts_dir("file_state");
        assert!(!accounts_file_exists(&dir));
        assert!(!accounts_file_is_parseable(&dir));

        std::fs::write(dir.join("accounts.json"), b"{not json").unwrap();
        assert!(accounts_file_exists(&dir));
        assert!(
            !accounts_file_is_parseable(&dir),
            "corrupt file must not be treated as an empty list"
        );

        let entry = AccountEntry::new_offline("Valid");
        save_accounts(&dir, &[entry]).unwrap();
        assert!(accounts_file_exists(&dir));
        assert!(accounts_file_is_parseable(&dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn public_view_strips_nothing_secret() {
        let entry = AccountEntry::new_offline("Alex");
        let public = PublicAccountEntry::from(entry);
        assert_eq!(public.name, "Alex");
    }

    #[test]
    fn vault_chunks_respect_size_limit_and_roundtrip() {
        // Short value → single chunk.
        let short = split_into_chunks("hello");
        assert_eq!(short, vec!["hello".to_string()]);

        // Exactly at the limit → still one chunk.
        let exact = split_into_chunks(&"x".repeat(VAULT_CHUNK_CHARS));
        assert_eq!(exact.len(), 1);

        // One char over → two chunks, and joining restores the original.
        let long: String = "abcdefgh".repeat(1024); // 8192 chars
        let chunks = split_into_chunks(&long);
        assert_eq!(chunks.len(), 8);
        assert!(chunks
            .iter()
            .all(|c| c.chars().count() <= VAULT_CHUNK_CHARS));
        assert_eq!(chunks.concat(), long);

        // Empty value still produces a marker chunk.
        assert_eq!(split_into_chunks(""), vec![String::new()]);
    }

    #[test]
    fn vault_generation_marker_roundtrips_and_rejects_garbage() {
        let parsed = parse_vault_generations("abc123:2,def456:1");
        assert_eq!(
            parsed,
            vec![("abc123".to_string(), 2), ("def456".to_string(), 1)]
        );

        let marker = format_vault_generations("newgen", 3, &parsed);
        assert_eq!(marker, "newgen:3,abc123:2,def456:1");
        assert_eq!(
            parse_vault_generations(&marker),
            vec![
                ("newgen".to_string(), 3),
                ("abc123".to_string(), 2),
                ("def456".to_string(), 1)
            ]
        );

        // Zero/overflowing chunk counts and unsafe ids are ignored, so a
        // corrupt marker can never point at arbitrary credential entries.
        assert!(parse_vault_generations("bad id:2").is_empty());
        assert!(parse_vault_generations("gen:0").is_empty());
        assert!(parse_vault_generations("gen:999").is_empty());
        assert!(parse_vault_generations("gen").is_empty());
    }

    #[test]
    fn upsert_supports_multiple_microsoft_accounts() {
        let dir = std::env::temp_dir().join(format!("voidl_acc_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let first = upsert_microsoft_account(&dir, "Steve", "uuid-steve").expect("first upsert");
        assert!(first.default, "first ever account becomes the default");

        let second = upsert_microsoft_account(&dir, "Alex", "uuid-alex").expect("second upsert");
        assert!(
            !second.default,
            "a second account must not steal the default flag"
        );
        assert_ne!(first.id, second.id, "distinct accounts need distinct ids");

        // Re-login of an existing account keeps its id and default state.
        let again = upsert_microsoft_account(&dir, "Stevie", "uuid-steve").expect("re-upsert");
        assert_eq!(
            again.id, first.id,
            "same uuid must map to the same account entry"
        );
        assert_eq!(again.name, "Stevie");

        let list = list_accounts(&dir);
        assert_eq!(list.len(), 2);
        assert!(list.iter().filter(|a| a.default).count() == 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_roundtrips_and_compacts_previous_generations() {
        let store = FakeStore::new();
        vault_set_with(&store, "slot", "short").expect("first write");
        assert_eq!(vault_get_with(&store, "slot").as_deref(), Some("short"));

        let first_marker = store.raw("slot#current").expect("marker");
        assert!(!first_marker.starts_with('!'), "committed marker expected");
        let first_generations = parse_vault_generations(&first_marker);
        assert_eq!(first_generations.len(), 1);

        let long: String = "abcdefgh".repeat(1024); // 8192 chars → 8 chunks
        vault_set_with(&store, "slot", &long).expect("second write");
        assert_eq!(
            vault_get_with(&store, "slot").as_deref(),
            Some(long.as_str())
        );

        let committed = parse_vault_generations(&store.raw("slot#current").unwrap());
        assert_eq!(
            committed.len(),
            1,
            "the committed marker must reference only the live generation"
        );
        assert_eq!(store.count_with("#g"), 8);
        assert!(
            !store
                .keys()
                .iter()
                .any(|key| key.contains(&first_generations[0].0)),
            "old generation chunks must be removed"
        );
    }

    #[test]
    fn crashed_generation_is_removed_and_previous_session_survives() {
        let store = FakeStore::new();
        vault_set_with(&store, "slot", "old-session").expect("seed write");
        let previous = store.raw("slot#current").expect("previous marker");

        // Crash after writing the pending marker and one new chunk.
        store.set_raw("slot#current", &format!("!crashedgen:2|{previous}"));
        store.set_raw("slot#gcrashedgen#0", "half-written");

        assert_eq!(
            vault_get_with(&store, "slot").as_deref(),
            Some("old-session"),
            "an uncommitted generation must never be served"
        );
        assert_eq!(
            store.raw("slot#current").as_deref(),
            Some(previous.as_str()),
            "previous marker must be restored"
        );
        assert_eq!(
            store.count_with("crashedgen"),
            0,
            "orphaned chunks must be deleted"
        );
    }

    #[test]
    fn pending_generation_without_previous_commit_is_discarded() {
        let store = FakeStore::new();
        store.set_raw("slot#current", "!onlygen:1|");
        store.set_raw("slot#gonlygen#0", "uncommitted");

        assert_eq!(vault_get_with(&store, "slot"), None);
        assert_eq!(store.raw("slot#current"), None);
        assert_eq!(store.count_with("onlygen"), 0);
    }

    #[test]
    fn failed_chunk_write_keeps_previous_session_and_leaves_no_orphans() {
        let store = FakeStore::new();
        vault_set_with(&store, "slot", "old").expect("seed write");
        let keys_before = store.keys();

        let long = "x".repeat(VAULT_CHUNK_CHARS + 100); // two chunks
        store.fail_sets("slot#g", 0, 1);
        let err = vault_set_with(&store, "slot", &long).unwrap_err();
        assert!(err.contains("injected write failure"), "got: {err}");

        assert_eq!(vault_get_with(&store, "slot").as_deref(), Some("old"));
        assert_eq!(store.keys(), keys_before, "no orphan keys may remain");
        assert!(
            !store.raw("slot#current").unwrap().starts_with('!'),
            "pending marker must be rolled back"
        );

        // A retry after the transient failure succeeds and replaces the value.
        vault_set_with(&store, "slot", &long).expect("retry write");
        assert_eq!(
            vault_get_with(&store, "slot").as_deref(),
            Some(long.as_str())
        );
        assert_eq!(store.count_with("#g"), 2);
    }

    #[test]
    fn failed_marker_commit_rolls_back_to_previous_generation() {
        let store = FakeStore::new();
        vault_set_with(&store, "slot", "old").expect("seed write");
        let previous = store.raw("slot#current").expect("previous marker");

        // The pending marker (first #current write) succeeds; the commit fails.
        store.fail_sets("#current", 1, 1);
        let err = vault_set_with(&store, "slot", "new").unwrap_err();
        assert!(err.contains("commit"), "got: {err}");

        assert_eq!(
            store.raw("slot#current").as_deref(),
            Some(previous.as_str())
        );
        assert_eq!(vault_get_with(&store, "slot").as_deref(), Some("old"));
        assert_eq!(store.count_with("#g"), 1);
    }

    #[test]
    fn readback_failure_rolls_back_only_to_readable_generations() {
        let store = FakeStore::new();
        // A marker left by an interrupted transition: the first generation is
        // gone, the fallback is readable.
        store.set_raw("slot#current", "deadgen:1,goodgen:1");
        store.set_raw("slot#ggoodgen#0", "good-session");

        // Reads 1-2 are deadgen/goodgen during the pre-write readability
        // check; read 3 is the new generation after commit.
        store.fail_gets("#g", 2, 1);
        let err = vault_set_with(&store, "slot", "new").unwrap_err();
        assert!(err.contains("verification"), "got: {err}");

        let marker = parse_vault_generations(&store.raw("slot#current").unwrap());
        assert_eq!(
            marker,
            vec![("goodgen".to_string(), 1)],
            "rollback must reference only readable generations"
        );
        assert_eq!(
            vault_get_with(&store, "slot").as_deref(),
            Some("good-session")
        );
    }

    #[test]
    fn transient_marker_failure_is_retried() {
        let store = FakeStore::new();
        store.fail_sets("#current", 0, 1);

        vault_set_retry(&store, "slot", "session", 2).expect("retry must succeed");
        assert_eq!(vault_get_with(&store, "slot").as_deref(), Some("session"));
    }

    #[test]
    fn delete_removes_committed_pending_and_legacy_entries() {
        let store = FakeStore::new();
        vault_set_with(&store, "slot", "session").expect("seed write");
        let committed = store.raw("slot#current").expect("committed marker");

        // Interrupted overwrite on top of the committed value.
        store.set_raw("slot#current", &format!("!pendinggen:1|{committed}"));
        store.set_raw("slot#gpendinggen#0", "partial");
        // Entries written by older releases.
        store.set_raw("slot#0", "legacy-chunk");
        store.set_raw("slot", "legacy-single");

        vault_delete_with(&store, "slot");
        assert!(store.keys().is_empty(), "left behind: {:?}", store.keys());
    }

    #[test]
    fn deleting_one_account_leaves_other_accounts_intact() {
        let store = FakeStore::new();
        vault_set_with(&store, "account:a:elyby", "ely-token").expect("ely write");
        vault_set_with(&store, "account:a:msauth", "ms-token").expect("ms write");
        vault_set_with(&store, "account:b:msauth", "other-token").expect("other write");

        vault_delete_with(&store, "account:a:elyby");
        vault_delete_with(&store, "account:a:msauth");

        assert_eq!(vault_get_with(&store, "account:a:elyby"), None);
        assert_eq!(vault_get_with(&store, "account:a:msauth"), None);
        assert_eq!(
            vault_get_with(&store, "account:b:msauth").as_deref(),
            Some("other-token")
        );
        assert_eq!(store.count_with("account:a:"), 0);
    }

    #[test]
    fn legacy_chunked_and_single_entry_layouts_still_read() {
        let store = FakeStore::new();
        store.set_raw("legacy#0", "hello ");
        store.set_raw("legacy#1", "world");
        assert_eq!(
            vault_get_with(&store, "legacy").as_deref(),
            Some("hello world")
        );

        store.set_raw("single", "value");
        assert_eq!(vault_get_with(&store, "single").as_deref(), Some("value"));

        // A committed generation wins over stale legacy entries and removes
        // them once the write is verified.
        vault_set_with(&store, "single", "new").expect("write over legacy");
        assert_eq!(vault_get_with(&store, "single").as_deref(), Some("new"));
        assert_eq!(store.raw("single"), None);
    }

    #[test]
    fn concurrent_writes_never_mix_generations_and_leave_no_orphans() {
        let store = Arc::new(FakeStore::new());
        let mut handles = Vec::new();
        for thread in 0..6 {
            let store = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                for round in 0..8 {
                    let token = format!("{thread}-{round}");
                    let value = format!("{token};").repeat(400); // spans 2 chunks
                    vault_set_with(store.as_ref(), "shared", &value).expect("concurrent write");

                    let seen = vault_get_with(store.as_ref(), "shared").expect("concurrent read");
                    let first = seen.split(';').next().unwrap();
                    assert!(
                        seen.split(';')
                            .filter(|part| !part.is_empty())
                            .all(|part| part == first),
                        "mixed generations observed in '{}...'",
                        &seen[..seen.len().min(40)]
                    );
                }
            }));
        }
        for handle in handles {
            handle.join().expect("writer thread must not panic");
        }

        let marker_key = "shared#current".to_string();
        let marker = parse_vault_generations(&store.raw(&marker_key).unwrap());
        assert_eq!(marker.len(), 1);
        let live_generation = &marker[0].0;
        for key in store.keys() {
            if key == marker_key {
                continue;
            }
            assert!(
                key.contains(live_generation),
                "orphan vault entry left behind: {key}"
            );
        }
    }
}

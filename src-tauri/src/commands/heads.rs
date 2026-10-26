//! Player-head pipeline for account avatars.
//!
//! The renderer used to load heads from mc-heads.net directly; that service
//! silently substitutes the default Steve head whenever ITS lookup of the
//! Mojang profile fails, so avatars flip to Steve on flaky networks. This
//! module fetches the RAW SKIN from authoritative sources instead and caches
//! it on disk per uuid:
//!
//!   1. sessionserver.mojang.com (public profile → textures.minecraft.net)
//!   2. api.minecraftservices.com/minecraft/profile with the account's own
//!      stored Minecraft token (covers sessionserver rate-limit windows)
//!   3. skinsystem.ely.by for Ely.by accounts (their own skin delivery, which
//!      also proxies skins of premium Minecraft users)
//!   4. mc-heads.net/skin as the last resort
//!
//! The frontend crops the square head out of the skin PNG (canvas), so every
//! surface (Home, Settings, Sidebar, Accounts) renders the SAME cached image
//! for the same account. Offline accounts never go to the network: their
//! random profile uuid deterministically maps onto one of the bundled
//! vanilla default skins (see `crate::default_skin`), which is exactly what
//! the game itself renders for a skinless profile. Microsoft/Ely.by profiles
//! fall back to that same bundled default when every source (and the disk
//! cache) comes up empty.
//!
//! This module also owns CAPES for Microsoft accounts: the official profile
//! lists every cape the account owns (id/alias/state/url), and the
//! `/minecraft/profile/capes/active` endpoint switches the worn one. Cape
//! PNGs travel to the renderer as data URLs through the same byte-capped
//! allowlisted pipeline as skins and are cached on disk by their content
//! hash, so repeat visits skip the downloads. A cape SWITCH answers with
//! metadata only: the renderer already holds the textures, so no texture is
//! ever re-downloaded just to flip which one is worn. The parsed list itself
//! is memoized for a short TTL (every switch refreshes it), so re-opening the
//! page never hammers the rate-limited profile endpoint — and a failed fetch
//! is reported as an error instead of a fake empty list.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::accounts::{self, AccountEntry, AccountType};
use crate::AppState;

/// Skin files are a few KB; a hard cap keeps a hostile/broken host from
/// forcing large allocations through the renderer bridge.
const MAX_SKIN_BYTES: u64 = 512 * 1024;
/// Cached skins revalidate after a week — fresh enough for skin changes,
/// stable enough that a transient Mojang outage never blanks avatars.
const CACHE_TTL_SECS: u64 = 7 * 24 * 3600;

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Normalize a uuid for cache keys and API paths: lowercase, dash-stripped.
/// The result becomes a file name, so it must be exactly 32 hex chars —
/// anything else (slashes, dots, unicode) must never reach the filesystem.
fn normalize_uuid(raw: &str) -> Option<String> {
    let s: String = raw
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    if s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(s)
    } else {
        None
    }
}

fn skin_cache_path(data_dir: &std::path::Path, norm: &str) -> std::path::PathBuf {
    data_dir.join("heads").join(format!("{norm}.png"))
}

fn cache_is_fresh(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    match std::time::SystemTime::now().duration_since(modified) {
        Ok(age) => age.as_secs() < CACHE_TTL_SECS,
        Err(_) => false,
    }
}

fn png_bytes_ok(bytes: &[u8]) -> bool {
    bytes.len() > PNG_MAGIC.len() && bytes.starts_with(PNG_MAGIC)
}

fn data_url(bytes: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        crate::commands::mods::base64_encode_pub(bytes)
    )
}

/// Minimal standard-alphabet base64 decoder (with padding) for the textures
/// property of sessionserver profiles. No new dependency for 20 lines.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let cleaned: Vec<u8> = input
        .bytes()
        .filter(|b| *b != b'\n' && *b != b'\r')
        .collect();
    if !cleaned.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(cleaned.len() / 4 * 3);
    for chunk in cleaned.chunks(4) {
        let pad = chunk.iter().filter(|b| **b == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut n: u32 = 0;
        for (i, b) in chunk.iter().enumerate() {
            let shift = 18 - 6 * i;
            if *b == b'=' {
                // Padding is only valid in the last two positions.
                if i < 2 {
                    return None;
                }
                continue;
            }
            n |= val(*b)? << shift;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Pull the SKIN texture URL out of a sessionserver profile document.
fn skin_url_from_sessionserver(json: &serde_json::Value) -> Option<String> {
    for prop in json["properties"].as_array()? {
        if prop["name"].as_str() != Some("textures") {
            continue;
        }
        let raw = prop["value"].as_str()?;
        let decoded = base64_decode(raw)?;
        let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
        let url = v["textures"]["SKIN"]["url"].as_str()?;
        if url.is_empty() {
            return None;
        }
        return Some(url.to_string());
    }
    None
}

/// Pull the ACTIVE skin URL out of api.minecraftservices.com /minecraft/profile.
fn skin_url_from_minecraftservices(json: &serde_json::Value) -> Option<String> {
    let skins = json["skins"].as_array()?;
    let pick = |v: &serde_json::Value| {
        v["url"]
            .as_str()
            .map(str::to_string)
            .filter(|u| !u.is_empty())
    };
    for s in skins {
        if s["state"].as_str() == Some("ACTIVE") {
            if let Some(url) = pick(s) {
                return Some(url);
            }
        }
    }
    skins.first().and_then(pick)
}

/// Mojang serves texture URLs over plain http; upgrade to https so the
/// request goes through the normal HTTPS path and redirect policy.
fn upgrade_texture_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("http://") {
        format!("https://{rest}")
    } else {
        url.to_string()
    }
}

/// Download a remote image with the launcher's HTTP stack (proxy with direct
/// fallback) and a strict byte cap.
async fn fetch_skin_capped(url: &str) -> Option<Vec<u8>> {
    if !crate::is_allowed_download_host(url) {
        tracing::warn!(target: "launcher", "Skin source host not allowlisted: {}", url);
        return None;
    }
    let client = crate::download::global_http_client().ok()?;
    let resp = crate::download::send_with_fallback(client.get(url))
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_SKIN_BYTES {
            return None;
        }
    }
    use futures::StreamExt;
    let mut remaining = MAX_SKIN_BYTES;
    let mut buf: Vec<u8> =
        Vec::with_capacity(resp.content_length().unwrap_or(0).min(MAX_SKIN_BYTES) as usize);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if (remaining as usize) < chunk.len() {
            return None;
        }
        buf.extend_from_slice(&chunk);
        remaining -= chunk.len() as u64;
    }
    if buf.is_empty() || !png_bytes_ok(&buf) {
        return None;
    }
    Some(buf)
}

/// Public Mojang profile (no auth): works for any account's uuid.
async fn fetch_skin_sessionserver(norm: &str) -> Option<Vec<u8>> {
    let url = format!(
        "https://sessionserver.mojang.com/session/minecraft/profile/{}",
        norm
    );
    let client = crate::download::global_http_client().ok()?;
    let resp = crate::download::send_with_fallback(client.get(&url))
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    let skin_url = skin_url_from_sessionserver(&json)?;
    fetch_skin_capped(&upgrade_texture_url(&skin_url)).await
}

/// Official profile for a signed-in account, authorized with its own stored
/// Minecraft token. Covers sessionserver rate-limit/unreachability windows.
async fn fetch_skin_minecraftservices(account: &AccountEntry) -> Option<Vec<u8>> {
    let session = accounts::load_ms_session(&account.id)?;
    let token = session.minecraft_token?.access_token;
    if token.is_empty() {
        return None;
    }
    let client = crate::download::global_http_client().ok()?;
    let resp = crate::download::send_with_fallback(
        client
            .get("https://api.minecraftservices.com/minecraft/profile")
            .header("Authorization", format!("Bearer {}", token)),
    )
    .await
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    let skin_url = skin_url_from_minecraftservices(&json)?;
    fetch_skin_capped(&upgrade_texture_url(&skin_url)).await
}

/// Pull the SKIN texture URL out of an Ely.by textures-system document
/// (skinsystem.ely.by/textures/{nickname}: `{"SKIN":{"url":"..."}}`).
fn skin_url_from_elyby_textures(json: &serde_json::Value) -> Option<String> {
    let url = json["SKIN"]["url"].as_str()?;
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

/// Ely.by's own skin delivery, keyed by NICKNAME (their system is premium-
/// aware: for non-Ely.by names it proxies the Mojang skin). The textures
/// endpoint yields the real texture URL, which is then upgraded to https and
/// fetched through the allowlisted, byte-capped pipeline — same shape as the
/// sessionserver flow above.
async fn fetch_skin_elyby(name: &str) -> Option<Vec<u8>> {
    // Ely.by nicknames follow the Minecraft rules (alnum + underscore); the
    // name is interpolated into a URL path, so keep it strictly to that set.
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let url = format!("https://skinsystem.ely.by/textures/{}", name);
    let client = crate::download::global_http_client().ok()?;
    let resp = crate::download::send_with_fallback(client.get(&url))
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    let skin_url = skin_url_from_elyby_textures(&json)?;
    fetch_skin_capped(&upgrade_texture_url(&skin_url)).await
}

/// Last resort: mc-heads' RAW SKIN endpoint (not /avatar, which renders a
/// head and falls back to Steve silently).
async fn fetch_skin_mcheads(norm: &str) -> Option<Vec<u8>> {
    fetch_skin_capped(&format!("https://mc-heads.net/skin/{}", norm)).await
}

async fn fetch_skin_fresh(norm: &str, account: Option<&AccountEntry>) -> Option<Vec<u8>> {
    if let Some(a) = account {
        if a.account_type == AccountType::ElyBy {
            if let Some(bytes) = fetch_skin_elyby(&a.name).await {
                return Some(bytes);
            }
        }
    }
    if let Some(bytes) = fetch_skin_sessionserver(norm).await {
        return Some(bytes);
    }
    if let Some(account) = account {
        if account.account_type == AccountType::Microsoft {
            if let Some(bytes) = fetch_skin_minecraftservices(account).await {
                return Some(bytes);
            }
        }
    }
    fetch_skin_mcheads(norm).await
}

/// Return the account entry whose stored uuid matches the given one.
fn find_account_by_uuid<'a>(accounts: &'a [AccountEntry], norm: &str) -> Option<&'a AccountEntry> {
    accounts.iter().find(|a| {
        a.uuid
            .as_deref()
            .and_then(normalize_uuid)
            .is_some_and(|u| u == norm)
    })
}

/// Fetch (or serve from the disk cache) the raw skin of an account as a
/// PNG data URL. The renderer crops the head from it; see module docs.
#[tauri::command]
pub async fn cmd_get_player_head(
    state: State<'_, AppState>,
    uuid: String,
) -> Result<Option<String>, String> {
    let Some(norm) = normalize_uuid(&uuid) else {
        return Ok(None);
    };
    let data_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.data_dir.clone()
    };
    let path = skin_cache_path(&data_dir, &norm);

    let account_list = accounts::list_accounts(&data_dir);
    let account = find_account_by_uuid(&account_list, &norm);

    // Offline accounts have no network identity: the deterministic bundled
    // vanilla default for their uuid is exactly what the game renders.
    if account.is_some_and(|a| a.account_type == AccountType::Offline) {
        return Ok(bundled_default_skin(&norm));
    }

    // Fresh cache: instant answer, no network.
    if cache_is_fresh(&path) {
        if let Ok(bytes) = std::fs::read(&path) {
            if png_bytes_ok(&bytes) {
                return Ok(Some(data_url(&bytes)));
            }
        }
    }

    if let Some(bytes) = fetch_skin_fresh(&norm, account).await {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&data_dir));
        if let Err(e) = std::fs::write(&path, &bytes) {
            tracing::warn!(target: "launcher", "Could not cache skin for {}: {}", norm, e);
        }
        return Ok(Some(data_url(&bytes)));
    }

    // Refresh failed (Mojang down, rate-limited, offline): a possibly
    // outdated cached head still beats a blank/Steve avatar.
    if let Ok(bytes) = std::fs::read(&path) {
        if png_bytes_ok(&bytes) {
            return Ok(Some(data_url(&bytes)));
        }
    }

    // Nothing cached either: the vanilla default by uuid is the honest
    // stand-in (a skinless profile renders exactly like this). Not written to
    // the disk cache — a later successful fetch must be able to win.
    Ok(bundled_default_skin(&norm))
}

/// PNG data URL of the bundled vanilla default skin for the uuid.
fn bundled_default_skin(norm: &str) -> Option<String> {
    crate::default_skin::default_skin_png(norm).map(|(_, _, png)| data_url(png))
}

// ==================== Microsoft capes ====================

/// One cape owned by a Microsoft account, ready for the renderer: metadata
/// plus the cape texture as a PNG data URL (empty string when the texture
/// could not be downloaded — the UI still shows the owned cape by alias).
#[derive(serde::Serialize, Clone, Debug)]
pub struct CapeInfo {
    pub id: String,
    pub alias: String,
    pub active: bool,
    pub png: String,
}

/// Parse the `capes` list out of an official profile document
/// (`api.minecraftservices.com/minecraft/profile`). Absent/empty list is
/// legitimate (an account may own no capes); entries without an id are
/// dropped.
fn capes_from_profile(json: &serde_json::Value) -> Vec<(String, String, bool, String)> {
    let Some(capes) = json["capes"].as_array() else {
        return Vec::new();
    };
    capes
        .iter()
        .filter_map(|c| {
            let id = c["id"].as_str()?;
            if id.is_empty() {
                return None;
            }
            Some((
                id.to_string(),
                c["alias"].as_str().unwrap_or("").to_string(),
                c["state"].as_str() == Some("ACTIVE"),
                c["url"].as_str().unwrap_or("").to_string(),
            ))
        })
        .collect()
}

/// Metadata-only cape list from parsed tuples (the renderer keeps the PNGs it
/// already received from `cmd_get_player_capes`).
fn capes_meta_from_tuples(tuples: Vec<(String, String, bool, String)>) -> Vec<CapeInfo> {
    tuples
        .into_iter()
        .map(|(id, alias, active, _url)| CapeInfo {
            id,
            alias,
            active,
            png: String::new(),
        })
        .collect()
}

/// How long a cached cape list may answer without a network round-trip.
const CAPE_CACHE_TTL: Duration = Duration::from_secs(45);

/// Last-seen cape tuples of one account, plus the fetch time.
struct CapeCacheEntry {
    at: Instant,
    capes: Vec<(String, String, bool, String)>,
}

/// Recently fetched cape lists, keyed by account id. Re-entering the page
/// reuses a fresh entry instead of hammering the rate-limited official
/// profile endpoint; every cape switch refreshes the entry immediately.
fn cape_cache() -> &'static Mutex<HashMap<String, CapeCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CapeCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cape_cache_is_fresh(at: Instant) -> bool {
    at.elapsed() < CAPE_CACHE_TTL
}

fn cape_cache_get(account_id: &str) -> Option<Vec<(String, String, bool, String)>> {
    let guard = cape_cache().lock().ok()?;
    let entry = guard.get(account_id)?;
    cape_cache_is_fresh(entry.at).then(|| entry.capes.clone())
}

fn cape_cache_store(account_id: &str, capes: &[(String, String, bool, String)]) {
    if let Ok(mut guard) = cape_cache().lock() {
        guard.retain(|_, e| cape_cache_is_fresh(e.at));
        guard.insert(
            account_id.to_string(),
            CapeCacheEntry {
                at: Instant::now(),
                capes: capes.to_vec(),
            },
        );
    }
}

/// Cache file for a cape texture, keyed by the content hash in the URL's
/// last segment (Mojang textures are immutable per hash, so a hit can never
/// serve stale art). Non-hash URLs (anything but a pure hex segment) are
/// simply not cached.
fn cape_cache_path(data_dir: &std::path::Path, url: &str) -> Option<std::path::PathBuf> {
    let seg = url.rsplit('/').next()?;
    if seg.len() < 8 || seg.len() > 64 || !seg.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(
        data_dir
            .join("capes")
            .join(format!("{}.png", seg.to_ascii_lowercase())),
    )
}

/// Download a cape texture (a few KB) or serve it from the disk cache as a
/// PNG data URL.
///
/// Cape texture URLs are content-addressed (the last path segment is the
/// texture hash), so a cached file can never serve outdated art. Any valid
/// cached PNG is therefore served immediately, even if its mtime is old —
/// this is what makes the worn cape appear on the very first frame and
/// survive a flaky connection.
async fn fetch_cape_png_cached(data_dir: &std::path::Path, url: &str) -> Option<String> {
    let path = cape_cache_path(data_dir, url);
    if let Some(p) = &path {
        if let Ok(bytes) = std::fs::read(p) {
            if png_bytes_ok(&bytes) {
                return Some(data_url(&bytes));
            }
            // A truncated/corrupt cache file must not shadow a fresh
            // download forever.
            let _ = std::fs::remove_file(p);
        }
    }
    match fetch_skin_capped(url).await {
        Some(bytes) => {
            if let Some(p) = &path {
                let _ = std::fs::create_dir_all(p.parent().unwrap_or(data_dir));
                if let Err(e) = std::fs::write(p, &bytes) {
                    tracing::warn!(target: "launcher", "Could not cache cape texture: {}", e);
                }
            }
            Some(data_url(&bytes))
        }
        None => {
            tracing::warn!(target: "launcher", "Could not download cape texture: {}", url);
            None
        }
    }
}

/// GET the official profile for a Minecraft token. Returns a descriptive
/// error (HTTP status + short body, or the transport error) so the in-app
/// log can explain WHY capes failed instead of a generic message.
async fn fetch_ms_profile(token: &str) -> Result<serde_json::Value, String> {
    let client = crate::download::global_http_client().map_err(|e| e.to_string())?;
    let mut last_err = String::new();
    for attempt in 1..=2u32 {
        let req = client
            .get("https://api.minecraftservices.com/minecraft/profile")
            .header("Authorization", format!("Bearer {}", token))
            .timeout(Duration::from_secs(15));
        match crate::download::send_with_fallback(req).await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.json().await {
                        Ok(json) => return Ok(json),
                        Err(e) => last_err = format!("invalid profile response: {}", e),
                    }
                } else {
                    let code = status.as_u16();
                    let body = resp.text().await.unwrap_or_default();
                    let body = body.chars().take(200).collect::<String>();
                    last_err = if body.is_empty() {
                        format!("Minecraft profile request failed (HTTP {})", code)
                    } else {
                        format!("Minecraft profile request failed (HTTP {}): {}", code, body)
                    };
                    // A rejected token will not become valid on retry.
                    if code == 401 || code == 403 {
                        return Err(last_err);
                    }
                }
            }
            Err(e) => last_err = format!("Minecraft profile request failed: {}", e),
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    }
    Err(last_err)
}

/// Turn parsed cape tuples into renderer-ready entries, downloading each cape
/// texture (small PNGs) through the allowlisted byte-capped pipeline — or
/// serving it from the per-hash disk cache.
async fn fetch_cape_infos(
    parsed: Vec<(String, String, bool, String)>,
    data_dir: &std::path::Path,
) -> Vec<CapeInfo> {
    let fetches = parsed
        .into_iter()
        .map(|(id, alias, active, url)| async move {
            let png = if url.is_empty() {
                String::new()
            } else {
                fetch_cape_png_cached(data_dir, &upgrade_texture_url(&url))
                    .await
                    .unwrap_or_default()
            };
            CapeInfo {
                id,
                alias,
                active,
                png,
            }
        });
    futures::future::join_all(fetches).await
}

/// Parsed cape list (id, alias, active, texture url) of a Microsoft account:
/// the session is ensured (tokens refresh as needed) and the official profile
/// is fetched.
async fn fetch_capes_meta(
    state: &AppState,
    account: &AccountEntry,
) -> Result<Vec<(String, String, bool, String)>, String> {
    let session = crate::commands::auth::ensure_ms_session(state, account).await?;
    let token = session
        .minecraft_token
        .ok_or("Not logged in with Microsoft")?
        .access_token;
    if token.is_empty() {
        return Err("Empty Minecraft token".to_string());
    }
    let json = fetch_ms_profile(&token)
        .await
        .map_err(|e| format!("Could not load the Minecraft profile: {}", e))?;
    Ok(capes_from_profile(&json))
}

/// How long on-disk cape metadata may answer without a network round-trip.
/// In-launcher cape switches rewrite this file immediately, so it only
/// delays noticing changes made on another device.
const CAPE_META_DISK_TTL_SECS: u64 = 6 * 3600;

/// On-disk cape metadata, so the picker and the worn cape survive an app
/// restart even when the profile endpoint is unreachable at startup.
type CapeTuple = (String, String, bool, String);

#[derive(Serialize, Deserialize)]
struct CapeMetaFile {
    fetched_at: u64,
    capes: Vec<CapeTuple>,
}

fn cape_meta_path(data_dir: &std::path::Path, account_id: &str) -> Option<std::path::PathBuf> {
    // Account ids are opaque strings; keep the file name to a safe alphabet.
    let safe: String = account_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() {
        return None;
    }
    Some(data_dir.join("capes").join(format!("meta-{}.json", safe)))
}

fn save_cape_meta(data_dir: &std::path::Path, account_id: &str, capes: &[CapeTuple]) {
    let Some(path) = cape_meta_path(data_dir, account_id) else {
        return;
    };
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(target: "launcher", "Could not create cape cache dir: {}", e);
            return;
        }
    }
    let payload = CapeMetaFile {
        fetched_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        capes: capes.to_vec(),
    };
    match serde_json::to_vec(&payload) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(&path, bytes) {
                tracing::warn!(target: "launcher", "Could not save cape metadata: {}", e);
            }
        }
        Err(e) => tracing::warn!(target: "launcher", "Could not serialize cape metadata: {}", e),
    }
}

fn load_cape_meta(data_dir: &std::path::Path, account_id: &str) -> Option<(Vec<CapeTuple>, u64)> {
    let path = cape_meta_path(data_dir, account_id)?;
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: CapeMetaFile = serde_json::from_str(&raw).ok()?;
    Some((parsed.capes, parsed.fetched_at))
}

fn cape_meta_is_fresh(fetched_at: u64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(fetched_at) < CAPE_META_DISK_TTL_SECS
}

/// List the capes owned by a Microsoft account. A fresh cached list answers
/// without touching the network; otherwise the official profile is fetched
/// and memoized. A failed fetch is a real error — the renderer keeps the
/// list it already has and shows the reason instead of a fake empty picker.
/// Only a non-Microsoft account or an account genuinely owning no capes
/// yields an empty list.
#[tauri::command]
pub async fn cmd_get_player_capes(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    account_id: String,
    force: Option<bool>,
) -> Result<Vec<CapeInfo>, String> {
    let force = force.unwrap_or(false);
    let (account, data_dir) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let account = accounts::list_accounts(&config.data_dir)
            .into_iter()
            .find(|a| a.id == account_id)
            .ok_or("Account not found")?;
        (account, config.data_dir.clone())
    };
    if account.account_type != AccountType::Microsoft {
        return Ok(Vec::new());
    }
    // Fresh memory cache: instant answer, no request to the rate-limited
    // profile endpoint (textures still come from the per-hash disk cache).
    if !force {
        if let Some(tuples) = cape_cache_get(&account_id) {
            return Ok(fetch_cape_infos(tuples, &data_dir).await);
        }
        // Disk metadata written by an earlier session: answer immediately,
        // so capes appear on the first frame even on a broken connection.
        if let Some((tuples, fetched_at)) = load_cape_meta(&data_dir, &account_id) {
            if cape_meta_is_fresh(fetched_at) {
                cape_cache_store(&account_id, &tuples);
                return Ok(fetch_cape_infos(tuples, &data_dir).await);
            }
        }
    }
    match fetch_capes_meta(state.inner(), &account).await {
        Ok(tuples) => {
            cape_cache_store(&account_id, &tuples);
            save_cape_meta(&data_dir, &account_id, &tuples);
            Ok(fetch_cape_infos(tuples, &data_dir).await)
        }
        Err(e) => {
            tracing::warn!(target: "launcher", "Could not load capes for '{}': {}", account.name, e);
            // The profile endpoint being down must not empty the picker or
            // the worn cape: serve the last successfully saved list (its
            // textures are content-addressed and cached on disk).
            if let Some((tuples, _)) = load_cape_meta(&data_dir, &account_id) {
                crate::events::emit_log(
                    &app,
                    "warn",
                    "accounts",
                    &format!(
                        "Could not refresh capes for '{}': {} — showing the last saved list",
                        account.name, e
                    ),
                );
                cape_cache_store(&account_id, &tuples);
                return Ok(fetch_cape_infos(tuples, &data_dir).await);
            }
            // No cached list either: surface the reason in the in-app log
            // (tracing alone only reaches the file log).
            crate::events::emit_log(
                &app,
                "error",
                "accounts",
                &format!("Could not load capes for '{}': {}", account.name, e),
            );
            Err(e)
        }
    }
}

/// Wear a cape (`Some(cape_id)`) or hide the cape (`None`) on a Microsoft
/// account. The official endpoints return the updated profile, from which
/// the fresh METADATA list is built — textures are not re-downloaded, the
/// renderer keeps the PNGs it already received. If the body cannot be parsed
/// the profile is re-fetched explicitly.
#[tauri::command]
pub async fn cmd_set_player_cape(
    state: State<'_, AppState>,
    account_id: String,
    cape_id: Option<String>,
) -> Result<Vec<CapeInfo>, String> {
    let account = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        accounts::list_accounts(&config.data_dir)
            .into_iter()
            .find(|a| a.id == account_id)
            .ok_or("Account not found")?
    };
    if account.account_type != AccountType::Microsoft {
        return Err("Capes are only available for Microsoft accounts.".to_string());
    }
    let session = crate::commands::auth::ensure_ms_session(state.inner(), &account).await?;
    let token = session
        .minecraft_token
        .ok_or("Not logged in with Microsoft")?
        .access_token;
    if token.is_empty() {
        return Err("Empty Minecraft token".to_string());
    }

    const CAPES_ACTIVE_URL: &str =
        "https://api.minecraftservices.com/minecraft/profile/capes/active";
    let client = crate::download::global_http_client().map_err(|e| e.to_string())?;
    let resp = match cape_id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => {
            // The id goes into a JSON body, but keep it to the uuid alphabet:
            // a malformed id can never reach the API.
            if id.len() > 36 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                return Err("Invalid cape id".to_string());
            }
            crate::download::send_with_fallback(
                client
                    .put(CAPES_ACTIVE_URL)
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&serde_json::json!({ "capeId": id })),
            )
            .await
            .map_err(|e| e.to_string())?
        }
        _ => crate::download::send_with_fallback(
            client
                .delete(CAPES_ACTIVE_URL)
                .header("Authorization", format!("Bearer {}", token)),
        )
        .await
        .map_err(|e| e.to_string())?,
    };

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let text = text.chars().take(200).collect::<String>();
        return Err(format!("Cape change failed ({}): {}", status, text));
    }

    let json = match resp.json::<serde_json::Value>().await {
        Ok(j) => j,
        Err(_) => fetch_ms_profile(&token)
            .await
            .map_err(|e| format!("Could not reload the Minecraft profile: {}", e))?,
    };
    // The switch response carries the updated list: memoize it so re-entering
    // the page right after a change does not re-fetch the profile.
    let tuples = capes_from_profile(&json);
    cape_cache_store(&account_id, &tuples);
    let data_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.data_dir.clone()
    };
    save_cape_meta(&data_dir, &account_id, &tuples);
    Ok(capes_meta_from_tuples(tuples))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- normalize_uuid: the value is a file name — path safety matters ----

    #[test]
    fn normalize_accepts_dashed_undashed_and_uppercase() {
        assert_eq!(
            normalize_uuid("1a650540fb594d9da1d3f8f7a67379a3").as_deref(),
            Some("1a650540fb594d9da1d3f8f7a67379a3")
        );
        assert_eq!(
            normalize_uuid("1A650540-FB59-4D9D-A1D3-F8F7A67379A3").as_deref(),
            Some("1a650540fb594d9da1d3f8f7a67379a3")
        );
    }

    #[test]
    fn normalize_rejects_non_uuid_input() {
        assert_eq!(normalize_uuid(""), None);
        assert_eq!(normalize_uuid("not-a-uuid"), None);
        assert_eq!(normalize_uuid("../../evil"), None);
        assert_eq!(normalize_uuid("c19dadb2-de74-4d14-888a-635ea075492"), None); // 31 hex
        assert_eq!(normalize_uuid("zz650540fb594d9da1d3f8f7a67379a3"), None);
        assert_eq!(normalize_uuid("конец"), None);
    }

    // ---- base64_decode ----

    #[test]
    fn base64_roundtrip_and_padding() {
        assert_eq!(base64_decode("QQ==").unwrap(), vec![b'A']);
        assert_eq!(base64_decode("QUI=").unwrap(), vec![b'A', b'B']);
        assert_eq!(base64_decode("QUJD").unwrap(), vec![b'A', b'B', b'C']);
        assert!(base64_decode("A").is_none());
        assert!(base64_decode("A===").is_none());
        assert!(base64_decode("@@@@").is_none());
    }

    // ---- sessionserver profile parsing ----

    #[test]
    fn sessionserver_skin_url_is_extracted_from_textures_property() {
        // Real response shape (Notch), trimmed to the relevant fields.
        let json = serde_json::json!({
            "id": "069a79f444e94726a5befca90e38aaf5",
            "name": "Notch",
            "properties": [{
                "name": "textures",
                "value": "ewogICJ0aW1lc3RhbXAiIDogMTc5MjA3MTIzNTYxNCwKICAicHJvZmlsZUlkIiA6ICIwNjlhNzlmNDQ0ZTk0NzI2YTViZWZjYTkwZTM4YWFmNSIsCiAgInByb2ZpbGVOYW1lIiA6ICJOb3RjaCIsCiAgInRleHR1cmVzIiA6IHsKICAgICJTS0lOIiA6IHsKICAgICAgInVybCIgOiAiaHR0cDovL3RleHR1cmVzLm1pbmVjcmFmdC5uZXQvdGV4dHVyZS8yOTIwMDlhNDkyNWI1OGYwMmM3N2RhZGMzZWNlZjA3ZWE0Yzc0NzJmNjRlMGZkYzMyY2U1NTIyNDg5MzYyNjgwIgogICAgfQogIH0KfQ=="
            }]
        });
        let url = skin_url_from_sessionserver(&json).expect("skin url");
        assert_eq!(
            url,
            "http://textures.minecraft.net/texture/292009a4925b58f02c77dadc3ecef07ea4c7472f64e0fdc32ce5522489362680"
        );
        assert_eq!(
            upgrade_texture_url(&url),
            "https://textures.minecraft.net/texture/292009a4925b58f02c77dadc3ecef07ea4c7472f64e0fdc32ce5522489362680"
        );
    }

    #[test]
    fn sessionserver_without_textures_yields_none() {
        let json = serde_json::json!({ "id": "abc", "name": "X", "properties": [] });
        assert!(skin_url_from_sessionserver(&json).is_none());
    }

    // ---- Ely.by textures parsing ----

    #[test]
    fn elyby_textures_skin_url_is_extracted_and_upgraded() {
        // Real response shape (erickskrauch, trimmed to the relevant field).
        let json = serde_json::json!({
            "SKIN": { "url": "http://ely.by/storage/skins/69c6740d2993e5d6f6a7fc92420efc29.png" }
        });
        let url = skin_url_from_elyby_textures(&json).expect("skin url");
        assert_eq!(
            url,
            "http://ely.by/storage/skins/69c6740d2993e5d6f6a7fc92420efc29.png"
        );
        assert_eq!(
            upgrade_texture_url(&url),
            "https://ely.by/storage/skins/69c6740d2993e5d6f6a7fc92420efc29.png"
        );
    }

    #[test]
    fn elyby_textures_without_skin_yields_none() {
        assert!(skin_url_from_elyby_textures(&serde_json::json!({})).is_none());
        assert!(
            skin_url_from_elyby_textures(&serde_json::json!({ "CAPE": { "url": "x" } })).is_none()
        );
        assert!(
            skin_url_from_elyby_textures(&serde_json::json!({ "SKIN": { "url": "" } })).is_none()
        );
    }

    // ---- minecraftservices profile parsing ----

    #[test]
    fn minecraftservices_prefers_active_skin() {
        let json = serde_json::json!({
            "id": "1a65",
            "name": "Black893",
            "skins": [
                { "state": "INACTIVE", "url": "https://textures.minecraft.net/texture/old", "variant": "CLASSIC" },
                { "state": "ACTIVE", "url": "https://textures.minecraft.net/texture/current", "variant": "SLIM" }
            ]
        });
        assert_eq!(
            skin_url_from_minecraftservices(&json).as_deref(),
            Some("https://textures.minecraft.net/texture/current")
        );
    }

    #[test]
    fn minecraftservices_falls_back_to_first_skin() {
        let json = serde_json::json!({
            "skins": [{ "url": "https://textures.minecraft.net/texture/only" }]
        });
        assert_eq!(
            skin_url_from_minecraftservices(&json).as_deref(),
            Some("https://textures.minecraft.net/texture/only")
        );
        assert!(skin_url_from_minecraftservices(&serde_json::json!({})).is_none());
    }

    // ---- cache path + magic ----

    #[test]
    fn cache_path_uses_only_the_normalized_uuid() {
        let dir = std::path::Path::new("/data");
        let p = skin_cache_path(dir, "abc");
        assert!(p.ends_with("heads/abc.png") || p.ends_with("heads\\abc.png"));
    }

    #[test]
    fn png_magic_gate() {
        assert!(png_bytes_ok(b"\x89PNG\r\n\x1a\nrest"));
        assert!(!png_bytes_ok(b"\x89PNG"));
        assert!(!png_bytes_ok(b"<html>not a png</html>"));
    }

    // ---- capes profile parsing ----

    #[test]
    fn capes_parse_active_and_inactive() {
        // Real response shape (trimmed): the owned capes with states.
        let json = serde_json::json!({
            "id": "1a65",
            "name": "Black893",
            "capes": [
                { "id": "cape-1", "state": "ACTIVE", "url": "http://textures.minecraft.net/texture/a", "alias": "Migrator" },
                { "id": "cape-2", "state": "INACTIVE", "url": "https://textures.minecraft.net/texture/b", "alias": "Vanilla" }
            ]
        });
        let capes = capes_from_profile(&json);
        assert_eq!(capes.len(), 2);
        assert_eq!(capes[0].0, "cape-1");
        assert_eq!(capes[0].1, "Migrator");
        assert!(capes[0].2);
        assert_eq!(capes[0].3, "http://textures.minecraft.net/texture/a");
        assert!(!capes[1].2);
        assert_eq!(capes[1].1, "Vanilla");
    }

    #[test]
    fn capes_absent_empty_or_idless_yield_nothing() {
        assert!(capes_from_profile(&serde_json::json!({})).is_empty());
        assert!(capes_from_profile(&serde_json::json!({ "capes": [] })).is_empty());
        // Entry without an id cannot be activated — dropped, with or without
        // the surrounding valid entries.
        let json = serde_json::json!({
            "capes": [
                { "state": "ACTIVE", "alias": "Ghost" },
                { "id": "ok", "state": "INACTIVE", "url": "", "alias": "" }
            ]
        });
        let capes = capes_from_profile(&json);
        assert_eq!(capes.len(), 1);
        assert_eq!(capes[0].0, "ok");
        assert_eq!(capes[0].1, "");
        assert_eq!(capes[0].3, "");
    }

    // ---- cape cache path + metadata-only switch response ----

    #[test]
    fn cape_cache_path_accepts_only_hex_hash_segments() {
        let dir = std::path::Path::new("/data");
        let ok = cape_cache_path(
            dir,
            "https://textures.minecraft.net/texture/292009A4925B58F02C77DADC3ECEF07EA4C7472F64E0FDC32CE5522489362680",
        );
        assert_eq!(
            ok,
            Some(
                dir.join("capes")
                    .join("292009a4925b58f02c77dadc3ecef07ea4c7472f64e0fdc32ce5522489362680.png")
            )
        );
        // Ely.by storage URLs end with ".png" — not a pure hex segment.
        assert!(cape_cache_path(
            dir,
            "https://ely.by/storage/skins/69c6740d2993e5d6f6a7fc92420efc29.png"
        )
        .is_none());
        assert!(cape_cache_path(dir, "https://textures.minecraft.net/texture/short").is_none());
        assert!(
            cape_cache_path(dir, "https://textures.minecraft.net/texture/../../evil").is_none()
        );
    }

    #[test]
    fn capes_meta_carries_states_without_textures() {
        let json = serde_json::json!({
            "capes": [
                { "id": "c1", "alias": "Migrator", "state": "ACTIVE", "url": "https://x/a" },
                { "id": "c2", "alias": "Vanilla", "state": "INACTIVE", "url": "https://x/b" }
            ]
        });
        let meta = capes_meta_from_tuples(capes_from_profile(&json));
        assert_eq!(meta.len(), 2);
        assert!(meta[0].active);
        assert!(!meta[1].active);
        assert!(meta.iter().all(|c| c.png.is_empty()));
    }

    // ---- cape cache freshness / metadata from tuples ----

    #[test]
    fn cape_cache_freshness_respects_ttl() {
        assert!(cape_cache_is_fresh(Instant::now()));
        let old = Instant::now()
            .checked_sub(CAPE_CACHE_TTL + Duration::from_secs(1))
            .expect("monotonic clock");
        assert!(!cape_cache_is_fresh(old));
    }

    #[test]
    fn capes_meta_from_tuples_drops_textures() {
        let meta = capes_meta_from_tuples(vec![
            ("a".into(), "Migrator".into(), true, "https://x/a".into()),
            ("b".into(), String::new(), false, String::new()),
        ]);
        assert_eq!(meta.len(), 2);
        assert!(meta[0].active);
        assert!(meta[0].png.is_empty());
        assert!(!meta[1].active);
    }

    #[test]
    fn cape_meta_roundtrips_and_sanitizes_account_ids() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher-cape-meta-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let tuples = vec![
            (
                "c1".to_string(),
                "Migrator".to_string(),
                true,
                "https://x/a".to_string(),
            ),
            (
                "c2".to_string(),
                "Vanilla".to_string(),
                false,
                "https://x/b".to_string(),
            ),
        ];
        save_cape_meta(&dir, "acct-1", &tuples);
        let (loaded, fetched_at) = load_cape_meta(&dir, "acct-1").expect("meta file must load");
        assert_eq!(loaded, tuples);
        assert!(cape_meta_is_fresh(fetched_at));
        assert!(!cape_meta_is_fresh(0));

        let path = cape_meta_path(&dir, "../evil").expect("sanitized path");
        assert!(path.starts_with(dir.join("capes")));
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("meta-evil.json")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

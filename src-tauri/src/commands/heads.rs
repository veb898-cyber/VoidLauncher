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
//!   3. skin.ely.by for Ely.by accounts (their own skin delivery)
//!   4. mc-heads.net/skin as the last resort
//!
//! The frontend crops the square head out of the skin PNG (canvas), so every
//! surface (Home, Settings, Sidebar, Accounts) renders the SAME cached image
//! for the same account. Offline accounts never reach this command — the UI
//! shows a fixed grayscale Steve for them by design.

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

/// Ely.by's own skin delivery. 404 means "no custom skin set" — callers
/// continue down the chain (a default Steve is then the honest answer).
async fn fetch_skin_elyby(norm: &str) -> Option<Vec<u8>> {
    fetch_skin_capped(&format!("https://skin.ely.by/{}.png", norm)).await
}

/// Last resort: mc-heads' RAW SKIN endpoint (not /avatar, which renders a
/// head and falls back to Steve silently).
async fn fetch_skin_mcheads(norm: &str) -> Option<Vec<u8>> {
    fetch_skin_capped(&format!("https://mc-heads.net/skin/{}", norm)).await
}

async fn fetch_skin_fresh(norm: &str, account: Option<&AccountEntry>) -> Option<Vec<u8>> {
    let is_ely = account.is_some_and(|a| a.account_type == AccountType::ElyBy);
    if is_ely {
        if let Some(bytes) = fetch_skin_elyby(norm).await {
            return Some(bytes);
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

    // Fresh cache: instant answer, no network.
    if cache_is_fresh(&path) {
        if let Ok(bytes) = std::fs::read(&path) {
            if png_bytes_ok(&bytes) {
                return Ok(Some(data_url(&bytes)));
            }
        }
    }

    let account_list = accounts::list_accounts(&data_dir);
    let account = find_account_by_uuid(&account_list, &norm);

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
    Ok(None)
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
}

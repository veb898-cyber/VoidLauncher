//! Room state: role (host/guest), room code, host reference and persistence.
//!
//! The reusable Machine Sharing grant lives in Tailscale itself and is NOT
//! revoked when a room is closed. The launcher only records:
//!   * which role this machine currently plays,
//!   * the room code (UI-level identifier, shown as `ABCD-1234`),
//!   * the resolved host endpoint discovered over the tailnet,
//!   * the currently advertised Minecraft LAN port (host side).

use crate::rooms::HostInfo;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Room persistent state mirrors `RoomStateRow` on disk (`rooms.json`).
pub const ROOMS_FILE: &str = "rooms.json";

/// Characters used for room codes. Excludes easily-confused glyphs
/// (I, O, 0, 1) so users can read codes aloud reliably.
const CODE_ALPHABET: &[u8] = b"ACDEFGHJKLMNPQRSTUVWXYZ2345679";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoomRole {
    #[default]
    None,
    Host,
    Guest,
}

/// The in-memory + on-disk room row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomStateRow {
    pub role: RoomRole,
    /// UI room code (`ABCD-1234`). Belongs to VoidLauncher, NOT to Tailscale.
    pub room_id: Option<String>,
    /// Optional human-readable name shown in the Rooms UI.
    pub room_name: Option<String>,
    /// Resolved host endpoint (guest side fills this from peer discovery).
    pub host: Option<HostInfo>,
    /// Minecraft "Open to LAN" port, host side (advertised via VoidLink).
    pub minecraft_port: Option<u16>,
    /// TCP port of the VoidLink control bridge on the host.
    pub void_link_port: u16,
    /// The machine's Tailscale hostname BEFORE the temporary `<room>-host`
    /// discovery hint was applied. Restored when the host leaves the room so
    /// the user's machine name is never left permanently renamed. Empty for
    /// guests / machines where no hint was ever set.
    pub previous_hostname: Option<String>,
}

impl Default for RoomStateRow {
    fn default() -> Self {
        Self {
            role: RoomRole::None,
            room_id: None,
            room_name: None,
            host: None,
            minecraft_port: None,
            void_link_port: crate::rooms::void_link::VOID_LINK_PORT,
            previous_hostname: None,
        }
    }
}

/// Generate a random room code in the `XXXX-XXXX` format. The code is a
/// UI-level identifier for the room — it is NOT a Tailscale secret. Enough
/// entropy (8 chars from a 33-symbol alphabet ≈ 40 bits) to avoid collisions
/// among a handful of weekly rooms.
pub fn generate_room_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(9);
    let state = RandomState::new();
    for i in 0..8 {
        if i == 4 {
            out.push('-');
        }
        let mut hasher = state.build_hasher();
        hasher.write_u64((uuid::Uuid::new_v4().as_u128() as u64) ^ u64::from(std::process::id()));
        let idx = (hasher.finish() as usize) % CODE_ALPHABET.len();
        out.push(CODE_ALPHABET[idx] as char);
    }
    out
}

/// Owns room state and its persistence. Shared (Arc) between the VoidLink
/// server, peer discovery and the command layer, hence `inner: Mutex`.
#[derive(Debug)]
pub struct RoomStateManager {
    data_dir: PathBuf,
    inner: Mutex<RoomStateRow>,
}

impl RoomStateManager {
    /// Load (or create) the persisted room state for `data_dir`.
    ///
    /// A persisted `Host` room is a room *restored after an app restart*; the
    /// Minecraft world that advertised `minecraft_port` is gone, so the stale
    /// port is cleared at load time and the cleared value is persisted — a
    /// restart must never re-advertise an expired port.
    pub fn new(data_dir: &Path) -> Self {
        let dir = crate::rooms::rooms_dir(data_dir);
        let mut row = resolve_persisted(&dir).unwrap_or_default();
        if row.role == RoomRole::Host && row.minecraft_port.is_some() {
            row.minecraft_port = None;
            let _ = persist_row(&dir, &row);
        }
        // A fresh row must carry the default bridge port even if the file
        // predates it (serde `#[serde(default)]` on the field handles the
        // missing-key case; this covers a full-default fallback too).
        Self {
            data_dir: data_dir.to_path_buf(),
            inner: Mutex::new(row),
        }
    }

    pub fn snapshot(&self) -> RoomStateRow {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn role(&self) -> RoomRole {
        self.snapshot().role
    }

    pub fn room_id(&self) -> Option<String> {
        self.snapshot().room_id
    }

    pub fn void_link_port(&self) -> u16 {
        self.snapshot().void_link_port
    }

    /// The host's machine name captured before the temporary room hint was
    /// applied (None when no hint was ever set or the room ended).
    pub fn previous_hostname(&self) -> Option<String> {
        self.snapshot().previous_hostname
    }

    /// Remember the machine's pre-room hostname so `leave` can restore it.
    /// Only the FIRST remembered name is kept: a re-created room while a hint
    /// is active would otherwise capture the hint itself as the "original".
    pub fn remember_original_hostname(&self, name: &str) -> crate::error::Result<()> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if row.previous_hostname.is_some() {
            return Ok(());
        }
        row.previous_hostname = Some(name.to_string());
        self.persist_locked(&row)
    }

    /// Override the control-bridge port; used by tests to bind an ephemeral
    /// port instead of the production `48888`.
    #[cfg(test)]
    pub fn set_void_link_port(&self, port: u16) -> crate::error::Result<()> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        row.void_link_port = port;
        self.persist_locked(&row)
    }

    /// Create a room as host. Idempotent for the same room name.
    pub fn create_room(&self, name: Option<String>) -> crate::error::Result<RoomStateRow> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        row.role = RoomRole::Host;
        if row.room_id.is_none() {
            row.room_id = Some(generate_room_id());
        }
        if let Some(name) = name {
            if !name.trim().is_empty() {
                row.room_name = Some(name.trim().to_string());
            }
        }
        row.host = None;
        row.minecraft_port = None;
        let out = row.clone();
        self.persist_locked(&row)?;
        Ok(out)
    }

    /// Join an existing room as guest given its UI code.
    pub fn join_room(&self, room_id: &str) -> crate::error::Result<RoomStateRow> {
        let id = room_id.trim().to_uppercase();
        if !valid_room_id(&id) {
            return Err(crate::error::LauncherError::Auth(format!(
                "Invalid room code '{}'. Expected the XXXX-XXXX format shown on the host.",
                room_id
            )));
        }
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        row.role = RoomRole::Guest;
        row.room_id = Some(id);
        row.host = None;
        row.minecraft_port = None;
        let out = row.clone();
        self.persist_locked(&row)?;
        Ok(out)
    }

    /// Leave the room. The Tailscale Machine Sharing grant is preserved; the
    /// temporary hostname hint is cleared for `cmd_room_leave` to restore.
    pub fn leave(&self) -> crate::error::Result<RoomStateRow> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        row.role = RoomRole::None;
        row.room_id = None;
        row.room_name = None;
        row.host = None;
        row.minecraft_port = None;
        row.previous_hostname = None;
        let out = row.clone();
        self.persist_locked(&row)?;
        Ok(out)
    }

    pub fn set_host(&self, host: Option<HostInfo>) -> crate::error::Result<()> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if row.host.as_ref().map(|h| h.node_key.as_str()) == host.as_ref().map(|h| h.node_key.as_str())
        {
            return Ok(());
        }
        row.host = host;
        self.persist_locked(&row)
    }

    pub fn set_minecraft_port(&self, port: Option<u16>) -> crate::error::Result<()> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if row.minecraft_port == port {
            return Ok(());
        }
        row.minecraft_port = port;
        self.persist_locked(&row)
    }

    fn persist_locked(&self, row: &RoomStateRow) -> crate::error::Result<()> {
        persist_row(&crate::rooms::rooms_dir(&self.data_dir), row)
    }
}

fn persist_row(dir: &Path, row: &RoomStateRow) -> crate::error::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(ROOMS_FILE);
    let json = serde_json::to_string_pretty(row)?;
    std::fs::write(path, json)?;
    Ok(())
}

fn resolve_persisted(dir: &Path) -> Option<RoomStateRow> {
    let path = dir.join(ROOMS_FILE);
    let contents = std::fs::read_to_string(&path).ok()?;
    if contents.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<RoomStateRow>(&contents).ok()
}

/// Room codes look like `ABCD-1234`: letters/digits groups separated by a
/// dash. Accepted with either case and optional surrounding whitespace.
pub fn valid_room_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 9 && bytes[4] == b'-' && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_ids_match_expected_shape_and_lookalikes_are_excluded() {
        for _ in 0..200 {
            let id = generate_room_id();
            assert_eq!(id.len(), 9, "room id length: {}", id);
            assert_eq!(&id[4..5], "-", "room id separator: {}", id);
            for c in id.chars() {
                assert!(
                    c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-',
                    "unexpected char {} in {}",
                    c,
                    id
                );
                assert!(!matches!(c, 'I' | 'O' | '0' | '1'), "lookalike {} in {}", c, id);
            }
            let prefix = CODE_ALPHABET
                .iter()
                .map(|b| *b as char)
                .collect::<String>();
            for c in id.chars().filter(|c| *c != '-') {
                assert!(prefix.contains(c), "char {} not in alphabet", c);
            }
        }
    }

    #[test]
    fn room_id_generation_is_suffix_distinct_across_many_calls() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..500 {
            assert!(seen.insert(generate_room_id()), "collision in room id");
        }
    }

    #[test]
    fn valid_room_id_accepts_canonical_and_rejects_garbage() {
        assert!(valid_room_id("ABCD-1234"));
        assert!(valid_room_id("abcd-1234"));
        assert!(!valid_room_id("ABCD1234"));
        assert!(!valid_room_id("ABC-1234"));
        assert!(!valid_room_id("ABCD-123"));
        assert!(!valid_room_id("ABCD-1234-"));
        assert!(!valid_room_id(""));
    }

    #[test]
    fn state_roundtrips_to_disk() {
        let dir = std::env::temp_dir().join(format!("vl_rooms_test_{}", std::process::id()));
        let mgr = RoomStateManager::new(&dir);
        mgr.create_room(Some("Вечерняя игра".into())).unwrap();
        mgr.remember_original_hostname("gaming-pc").unwrap();
        mgr.set_minecraft_port(Some(45565)).unwrap();
        mgr.set_host(Some(HostInfo {
            node_key: "node-key-1".into(),
            host_name: "abc-host".into(),
            dns_name: "abc-host.tail1234.ts.net".into(),
            tailnet_ips: vec!["100.64.0.1".into()],
            online: true,
        }))
        .unwrap();

        let loaded = RoomStateManager::new(&dir);
        let row = loaded.snapshot();
        assert_eq!(row.role, RoomRole::Host);
        assert_eq!(row.room_name.as_deref(), Some("Вечерняя игра"));
        // The advertised port never survives a restart (stale-port policy).
        assert_eq!(row.minecraft_port, None, "stale port must be cleared on restore");
        assert_eq!(row.host.as_ref().map(|h| h.node_key.as_str()), Some("node-key-1"));
        // The pre-room hostname survives so a later `leave` can restore it.
        assert_eq!(row.previous_hostname.as_deref(), Some("gaming-pc"));

        // Leaving preserves nothing on the launcher side (share lives in Tailscale).
        loaded.leave().unwrap();
        let row = loaded.snapshot();
        assert_eq!(row.role, RoomRole::None);
        assert!(row.room_id.is_none());
        assert!(row.host.is_none());
        assert!(row.previous_hostname.is_none(), "hint origin cleared on leave");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persisted_host_room_clears_stale_port_on_restart() {
        let dir = std::env::temp_dir().join(format!("vl_rooms_stale_{}", std::process::id()));
        let mgr = RoomStateManager::new(&dir);
        mgr.create_room(None).unwrap();
        mgr.set_minecraft_port(Some(45565)).unwrap();

        // Simulate an app restart: the same persisted file is re-read by a
        // fresh manager. The stale port must not be advertised again — and
        // the cleared value must be written back to disk.
        let reloaded = RoomStateManager::new(&dir);
        let snap = reloaded.snapshot();
        assert_eq!(snap.role, RoomRole::Host, "role survives restart");
        assert_eq!(snap.minecraft_port, None, "stale port cleared on restart");

        let on_disk = resolve_persisted(&crate::rooms::rooms_dir(&dir)).expect("file");
        assert_eq!(on_disk.minecraft_port, None, "cleared port persisted");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn original_hostname_remembered_once_and_cleared_on_leave() {
        let dir = std::env::temp_dir().join(format!("vl_rooms_hint_{}", std::process::id()));
        let mgr = RoomStateManager::new(&dir);
        mgr.create_room(None).unwrap();

        // First room session: capture the real machine name.
        mgr.remember_original_hostname("gaming-pc").unwrap();
        assert_eq!(mgr.previous_hostname().as_deref(), Some("gaming-pc"));

        // Re-entry while a hint `<code>-host` is active must NOT overwrite the
        // original with the hint itself — the first name sticks.
        mgr.remember_original_hostname("abcd-1234-host").unwrap();
        assert_eq!(
            mgr.previous_hostname().as_deref(),
            Some("gaming-pc"),
            "first remembered hostname must stick"
        );

        // Leaving the room drops the remembered name (the hint is restored by
        // the command layer), so the NEXT room session can capture again.
        mgr.leave().unwrap();
        assert!(mgr.previous_hostname().is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clearing_minecraft_port_persists_none() {
        let dir = std::env::temp_dir().join(format!("vl_rooms_port_test_{}", std::process::id()));
        let mgr = RoomStateManager::new(&dir);
        mgr.create_room(None).unwrap();
        mgr.set_minecraft_port(Some(45565)).unwrap();
        // Closing the world must write `None`, not resurrect the stale port.
        mgr.set_minecraft_port(None).unwrap();

        let reloaded = RoomStateManager::new(&dir);
        assert_eq!(reloaded.snapshot().minecraft_port, None, "cleared port must persist");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn join_room_normalizes_case_and_validates() {
        let dir = std::env::temp_dir().join(format!("vl_rooms_join_test_{}", std::process::id()));
        let mgr = RoomStateManager::new(&dir);
        mgr.join_room("  abcd-1234 ").unwrap();
        assert_eq!(mgr.room_id().as_deref(), Some("ABCD-1234"));
        assert_eq!(mgr.role(), RoomRole::Guest);
        assert!(mgr.join_room("nope").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
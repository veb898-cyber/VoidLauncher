//! Room state: role (host/guest), public room id, invitation secret, host
//! reference and persistence.
//!
//! The reusable Machine Sharing grant lives in Tailscale itself and is NOT
//! revoked when a room is closed. The launcher only records:
//!   * which role this machine currently plays,
//!   * the public room id (shown as `ABCD-1234`),
//!   * the private invitation secret used for VoidLink authentication,
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

/// Prefix of the copyable room invitation. The invitation contains the public
/// room id AND the private proof secret; a bare room id is not joinable.
pub const INVITATION_PREFIX: &str = "VLR1";

/// A parsed room invitation: the public identifier plus the shared secret used
/// for the VoidLink challenge-response. The secret is never sent during
/// discovery; only the guest and the host know it.
#[derive(Clone, PartialEq, Eq)]
pub struct RoomInvitation {
    pub room_id: String,
    pub secret: String,
}

impl std::fmt::Debug for RoomInvitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomInvitation")
            .field("room_id", &self.room_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoomRole {
    #[default]
    None,
    Host,
    Guest,
}

/// Custom serde for the invitation secret. New writes use DPAPI on Windows;
/// legacy plaintext values remain readable so existing rooms migrate on the
/// next save. A failed decryption is treated as a missing secret, never as a
/// reason to corrupt or discard the room file.
mod room_secret_codec {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(secret) => {
                let encoded = crate::rooms::secret_store::encode(secret).ok_or_else(|| {
                    serde::ser::Error::custom("could not protect the room invitation secret")
                })?;
                serializer.serialize_some(&encoded)
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Option::<String>::deserialize(deserializer)?;
        Ok(raw.and_then(|value| crate::rooms::secret_store::decode(&value)))
    }
}

/// The in-memory + on-disk room row.
#[derive(Clone, Serialize, Deserialize)]
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
    /// Instance whose game currently owns the room (host side). The launcher
    /// uses it to read exactly that session's log and to clear the advertised
    /// port when this game exits — even while the Rooms page is closed.
    #[serde(default)]
    pub host_instance: Option<String>,
    /// Shared invitation secret (64 lowercase hex characters). Required by the
    /// VoidLink challenge-response; never sent to discovery candidates and
    /// never included in the public room status returned to the UI.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "room_secret_codec"
    )]
    pub room_secret: Option<String>,
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
            host_instance: None,
            room_secret: None,
            void_link_port: crate::rooms::void_link::VOID_LINK_PORT,
            previous_hostname: None,
        }
    }
}

impl std::fmt::Debug for RoomStateRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomStateRow")
            .field("role", &self.role)
            .field("room_id", &self.room_id)
            .field("room_name", &self.room_name)
            .field("host", &self.host)
            .field("minecraft_port", &self.minecraft_port)
            .field("host_instance", &self.host_instance)
            .field(
                "room_secret",
                &self.room_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("void_link_port", &self.void_link_port)
            .field("previous_hostname", &self.previous_hostname)
            .finish()
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

/// Generate a high-entropy invitation secret from the OS CSPRNG (through two
/// UUID v4 values, which are backed by `getrandom`). The room id stays short
/// and human-readable; the secret is what authenticates the host during
/// discovery.
pub fn generate_room_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub fn valid_room_secret(secret: &str) -> bool {
    secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Copyable invitation: `VLR1:<ROOM-ID>:<64 hex chars>`.
pub fn format_invitation(room_id: &str, secret: &str) -> String {
    format!(
        "{}:{}:{}",
        INVITATION_PREFIX,
        room_id.trim().to_ascii_uppercase(),
        secret.trim().to_ascii_lowercase()
    )
}

/// Parse an invitation pasted by the guest. A bare room code is deliberately
/// not accepted: without the secret the guest cannot authenticate the host and
/// must not fall back to the old code-only check.
pub fn parse_invitation(input: &str) -> Option<RoomInvitation> {
    let input = input.trim();
    let rest = input
        .strip_prefix(INVITATION_PREFIX)
        .or_else(|| input.strip_prefix("vlr1"))?;
    let rest = rest.strip_prefix(':')?;
    let (room_id, secret) = rest.split_once(':')?;
    let room_id = room_id.trim().to_ascii_uppercase();
    let secret = secret.trim().to_ascii_lowercase();
    if !valid_room_id(&room_id) || !valid_room_secret(&secret) {
        return None;
    }
    Some(RoomInvitation { room_id, secret })
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
    /// A persisted room is a room *restored after an app restart*: the
    /// Minecraft world that advertised `minecraft_port` and the host endpoint
    /// resolved by the previous session are gone. Both are cleared at load
    /// time and the cleared values are persisted — a restart must never show a
    /// stale "world is open" endpoint (host) or an old host address (guest)
    /// before discovery runs again.
    pub fn new(data_dir: &Path) -> Self {
        let dir = crate::rooms::rooms_dir(data_dir);
        let mut row = resolve_persisted(&dir).unwrap_or_default();
        let mut dirty = false;
        if row.host.is_some() || row.minecraft_port.is_some() {
            row.host = None;
            row.minecraft_port = None;
            dirty = true;
        }
        if row.role != RoomRole::Host && row.host_instance.is_some() {
            row.host_instance = None;
            dirty = true;
        }
        if row.role == RoomRole::Host
            && row.room_id.is_some()
            && !row.room_secret.as_deref().is_some_and(valid_room_secret)
        {
            // Host rooms created before the invitation secret existed (or
            // with a corrupted secret) stay hostable, but the old bare code
            // is no longer accepted by guests.
            row.room_secret = Some(generate_room_secret());
            dirty = true;
        }
        if row.role == RoomRole::Guest
            && (row.room_id.is_none() || !row.room_secret.as_deref().is_some_and(valid_room_secret))
        {
            // A guest room without a secret cannot authenticate its host. Do
            // not resume code-only discovery; ask for a fresh invitation.
            row.role = RoomRole::None;
            row.room_id = None;
            row.room_name = None;
            row.host = None;
            row.minecraft_port = None;
            row.host_instance = None;
            dirty = true;
        }
        // Re-save a surviving secret so a legacy plaintext value is migrated
        // to DPAPI on Windows. This is a rewrite of the same value, not a
        // protocol/state change.
        if row.room_secret.is_some() {
            dirty = true;
        }
        if dirty {
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
    /// Kept for rooms created by older builds that renamed the node; current
    /// builds never rename it.
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

    /// Create a room as host. Idempotent for an existing host room; creating a
    /// room while this machine is a guest always generates a fresh code and
    /// secret so the old room's invitation cannot leak into the new host room.
    pub fn create_room(&self, name: Option<String>) -> crate::error::Result<RoomStateRow> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let was_host = row.role == RoomRole::Host;
        row.role = RoomRole::Host;
        if !was_host || row.room_id.is_none() {
            row.room_id = Some(generate_room_id());
            row.room_secret = Some(generate_room_secret());
        } else if !row.room_secret.as_deref().is_some_and(valid_room_secret) {
            row.room_secret = Some(generate_room_secret());
        }
        if let Some(name) = name {
            if !name.trim().is_empty() {
                row.room_name = Some(name.trim().to_string());
            }
        }
        row.host = None;
        row.minecraft_port = None;
        row.host_instance = None;
        let out = row.clone();
        self.persist_locked(&row)?;
        Ok(out)
    }

    /// Join an existing room as guest given the host's full invitation. A bare
    /// room code is rejected because it cannot authenticate the host.
    pub fn join_room(&self, invitation: &str) -> crate::error::Result<RoomStateRow> {
        let parsed = parse_invitation(invitation).ok_or_else(|| {
            crate::error::LauncherError::Auth(
                "Invalid invitation. Paste the full invitation copied by the host (it starts with VLR1:)."
                    .to_string(),
            )
        })?;
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        row.role = RoomRole::Guest;
        row.room_id = Some(parsed.room_id);
        row.room_secret = Some(parsed.secret);
        row.host = None;
        row.minecraft_port = None;
        row.host_instance = None;
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
        row.host_instance = None;
        row.room_secret = None;
        row.previous_hostname = None;
        let out = row.clone();
        self.persist_locked(&row)?;
        Ok(out)
    }

    /// Secret used for the VoidLink challenge-response. Guests need it to
    /// verify the host; it must never be sent in discovery requests.
    pub fn room_secret(&self) -> Option<String> {
        self.snapshot().room_secret
    }

    /// Copyable invitation for the host. Only the host exposes it to the UI.
    pub fn host_invitation(&self) -> Option<String> {
        let row = self.snapshot();
        if row.role != RoomRole::Host {
            return None;
        }
        Some(format_invitation(
            row.room_id.as_deref()?,
            row.room_secret.as_deref()?,
        ))
    }

    pub fn set_host(&self, host: Option<HostInfo>) -> crate::error::Result<()> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Compare the whole endpoint, not just node_key. A node can stay the
        // same while its `online`, MagicDNS name or tailnet IP changes after a
        // reconnect; returning early on node_key alone left guests stuck on
        // the old endpoint/offline state forever.
        if row.host == host {
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

    /// Remember which instance owns the host-side room. Persisting it lets the
    /// launcher resume the port monitor after a restart and clear the port
    /// when exactly that game exits, even if the Rooms page is closed.
    pub fn set_host_instance(&self, instance: Option<String>) -> crate::error::Result<()> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if row.host_instance == instance {
            return Ok(());
        }
        row.host_instance = instance;
        // A port belongs to the previous game/world; never advertise it while
        // the newly selected instance is being inspected.
        row.minecraft_port = None;
        self.persist_locked(&row)
    }

    pub fn host_instance(&self) -> Option<String> {
        self.snapshot().host_instance
    }

    /// Clear the advertised LAN port only when `instance` is the game that
    /// currently owns the host room. A different running instance must never
    /// wipe the host's endpoint.
    pub fn clear_minecraft_port_if_host_instance(
        &self,
        instance: &str,
    ) -> crate::error::Result<bool> {
        let mut row = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if row.role != RoomRole::Host
            || row.host_instance.as_deref() != Some(instance)
            || row.minecraft_port.is_none()
        {
            return Ok(false);
        }
        row.minecraft_port = None;
        self.persist_locked(&row)?;
        Ok(true)
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
    bytes.len() == 9
        && bytes[4] == b'-'
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a valid invitation for `id` with freshly generated entropy.
    fn invite(id: &str) -> String {
        format_invitation(id, &generate_room_secret())
    }

    #[cfg(windows)]
    #[test]
    fn legacy_plaintext_secret_is_migrated_to_dpapi() {
        let dir = std::env::temp_dir().join(format!(
            "vl_rooms_dpapi_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let rooms_dir = crate::rooms::rooms_dir(&dir);
        std::fs::create_dir_all(&rooms_dir).unwrap();
        let secret = generate_room_secret();
        let legacy = format!(
            r#"{{
  "role": "host",
  "room_id": "ABCD-1234",
  "room_name": null,
  "host": null,
  "minecraft_port": null,
  "host_instance": null,
  "room_secret": "{}",
  "void_link_port": 48888,
  "previous_hostname": null
}}"#,
            secret
        );
        std::fs::write(rooms_dir.join(ROOMS_FILE), &legacy).unwrap();

        let manager = RoomStateManager::new(&dir);
        assert_eq!(manager.room_secret().as_deref(), Some(secret.as_str()));
        let persisted = std::fs::read_to_string(rooms_dir.join(ROOMS_FILE)).unwrap();
        assert!(
            !persisted.contains(&secret),
            "the legacy plaintext secret must not stay on disk"
        );
        assert!(
            persisted.contains("dpapi:"),
            "the migrated secret must be DPAPI-protected"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

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
                assert!(
                    !matches!(c, 'I' | 'O' | '0' | '1'),
                    "lookalike {} in {}",
                    c,
                    id
                );
            }
            let prefix = CODE_ALPHABET.iter().map(|b| *b as char).collect::<String>();
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
    fn invitation_roundtrips_and_rejects_missing_or_tampered_secrets() {
        let secret = generate_room_secret();
        assert_eq!(secret.len(), 64);
        assert!(valid_room_secret(&secret));
        let invitation = format_invitation("abcd-1234", &secret);
        let parsed = parse_invitation(&format!("  {invitation}  ")).expect("valid invitation");
        assert_eq!(parsed.room_id, "ABCD-1234");
        assert_eq!(parsed.secret, secret);
        assert_eq!(
            format_invitation(&parsed.room_id, &parsed.secret),
            invitation
        );

        // A bare code is not an invitation: the secret is what authenticates
        // the host, so accepting the code alone would silently downgrade.
        assert!(parse_invitation("ABCD-1234").is_none());
        assert!(parse_invitation("VLR1:ABCD-1234").is_none());
        assert!(parse_invitation("VLR1:ABCD-1234:short").is_none());
        assert!(parse_invitation(
            "VLR1:ABCD-1234:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
        )
        .is_none());
        assert!(parse_invitation("VLR2:ABCD-1234:abcd").is_none());
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
        assert_eq!(
            row.minecraft_port, None,
            "stale port must be cleared on restore"
        );
        assert!(
            row.host.is_none(),
            "a persisted host endpoint must not survive a restart"
        );
        // The pre-room hostname survives so a later `leave` can restore it.
        assert_eq!(row.previous_hostname.as_deref(), Some("gaming-pc"));

        // Leaving preserves nothing on the launcher side (share lives in Tailscale).
        loaded.leave().unwrap();
        let row = loaded.snapshot();
        assert_eq!(row.role, RoomRole::None);
        assert!(row.room_id.is_none());
        assert!(row.host.is_none());
        assert!(
            row.previous_hostname.is_none(),
            "hint origin cleared on leave"
        );

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
    fn legacy_host_room_gets_a_secret_and_legacy_guest_room_is_reset() {
        let dir = std::env::temp_dir().join(format!(
            "vl_rooms_legacy_secret_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let rooms_dir = crate::rooms::rooms_dir(&dir);
        std::fs::create_dir_all(&rooms_dir).unwrap();

        let legacy_host = RoomStateRow {
            role: RoomRole::Host,
            room_id: Some("ABCD-1234".into()),
            room_secret: None,
            ..Default::default()
        };
        persist_row(&rooms_dir, &legacy_host).unwrap();
        let host = RoomStateManager::new(&dir).snapshot();
        assert_eq!(host.room_id.as_deref(), Some("ABCD-1234"));
        assert!(
            host.room_secret.as_deref().is_some_and(valid_room_secret),
            "a legacy host room must get a fresh invitation secret"
        );

        let legacy_guest = RoomStateRow {
            role: RoomRole::Guest,
            room_id: Some("WXYZ-7890".into()),
            room_secret: None,
            ..Default::default()
        };
        persist_row(&rooms_dir, &legacy_guest).unwrap();
        let guest = RoomStateManager::new(&dir).snapshot();
        assert_eq!(
            guest.role,
            RoomRole::None,
            "a guest room without a secret cannot be authenticated"
        );
        assert!(guest.room_id.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persisted_guest_room_never_keeps_a_stale_host_endpoint() {
        let dir = std::env::temp_dir().join(format!(
            "vl_rooms_guest_stale_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mgr = RoomStateManager::new(&dir);
        mgr.join_room(&invite("ABCD-1234")).unwrap();
        mgr.set_minecraft_port(Some(45565)).unwrap();
        mgr.set_host(Some(HostInfo {
            node_key: "node-key-1".into(),
            host_name: "gaming-pc".into(),
            dns_name: "gaming-pc.tail1234.ts.net".into(),
            tailnet_ips: vec!["100.64.0.1".into()],
            online: true,
        }))
        .unwrap();

        // Restart: the room code and role survive so the user stays in the
        // room, but the old endpoint/port must not make the guest UI look
        // connected before discovery has actually found the host again.
        let reloaded = RoomStateManager::new(&dir);
        let row = reloaded.snapshot();
        assert_eq!(row.role, RoomRole::Guest);
        assert_eq!(row.room_id.as_deref(), Some("ABCD-1234"));
        assert!(
            row.room_secret.as_deref().is_some_and(valid_room_secret),
            "the invitation secret survives so discovery can authenticate again"
        );
        assert!(row.host.is_none(), "stale host cleared on restart");
        assert_eq!(row.minecraft_port, None, "stale port cleared on restart");

        let on_disk = resolve_persisted(&crate::rooms::rooms_dir(&dir)).expect("file");
        assert!(on_disk.host.is_none());
        assert_eq!(on_disk.minecraft_port, None);

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
        assert_eq!(
            reloaded.snapshot().minecraft_port,
            None,
            "cleared port must persist"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn join_room_normalizes_case_and_validates() {
        let dir = std::env::temp_dir().join(format!("vl_rooms_join_test_{}", std::process::id()));
        let mgr = RoomStateManager::new(&dir);
        let secret = generate_room_secret();
        mgr.join_room(&format!("  vlr1:abcd-1234:{secret}  "))
            .unwrap();
        assert_eq!(mgr.room_id().as_deref(), Some("ABCD-1234"));
        assert_eq!(mgr.role(), RoomRole::Guest);
        assert_eq!(mgr.room_secret().as_deref(), Some(secret.as_str()));
        assert!(mgr.join_room("nope").is_err());
        assert!(
            mgr.join_room("ABCD-1234").is_err(),
            "a bare room code must not be accepted"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn room_lifecycle_clears_all_transient_fields() {
        let dir = std::env::temp_dir().join(format!("vl_rooms_lifecycle_{}", std::process::id()));
        let mgr = RoomStateManager::new(&dir);

        let host = mgr.create_room(Some("Evening".into())).unwrap();
        assert_eq!(host.role, RoomRole::Host);
        assert!(host.room_id.is_some());
        assert!(host.host.is_none());
        assert!(host.minecraft_port.is_none());

        mgr.set_minecraft_port(Some(45565)).unwrap();
        mgr.leave().unwrap();
        let left = mgr.snapshot();
        assert_eq!(left.role, RoomRole::None);
        assert!(left.room_id.is_none());
        assert!(left.room_name.is_none());
        assert!(left.minecraft_port.is_none());
        assert!(left.host.is_none());
        assert!(
            left.room_secret.is_none(),
            "leave must clear the invitation secret"
        );

        let guest = mgr.join_room(&invite("WXYZ-7890")).unwrap();
        assert_eq!(guest.role, RoomRole::Guest);
        assert_eq!(guest.room_id.as_deref(), Some("WXYZ-7890"));
        mgr.leave().unwrap();
        assert_eq!(mgr.snapshot().role, RoomRole::None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn host_instance_scopes_port_clearing_and_is_cleared_on_leave() {
        let dir = std::env::temp_dir().join(format!(
            "vl_rooms_host_instance_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mgr = RoomStateManager::new(&dir);
        mgr.create_room(None).unwrap();
        mgr.set_host_instance(Some("HostPack".into())).unwrap();
        mgr.set_minecraft_port(Some(45565)).unwrap();

        assert!(!mgr
            .clear_minecraft_port_if_host_instance("OtherPack")
            .unwrap());
        assert_eq!(mgr.snapshot().minecraft_port, Some(45565));
        assert!(mgr
            .clear_minecraft_port_if_host_instance("HostPack")
            .unwrap());
        assert_eq!(mgr.snapshot().minecraft_port, None);

        // Switching the instance drops the old port immediately.
        mgr.set_minecraft_port(Some(45566)).unwrap();
        mgr.set_host_instance(Some("SecondPack".into())).unwrap();
        assert_eq!(mgr.snapshot().minecraft_port, None);
        assert_eq!(mgr.host_instance().as_deref(), Some("SecondPack"));

        // The selected instance survives a restart with the host room.
        let reloaded = RoomStateManager::new(&dir);
        assert_eq!(reloaded.host_instance().as_deref(), Some("SecondPack"));
        reloaded.leave().unwrap();
        assert!(reloaded.host_instance().is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persisted_guest_room_drops_a_legacy_host_instance() {
        let dir =
            std::env::temp_dir().join(format!("vl_rooms_guest_instance_{}", std::process::id()));
        let mgr = RoomStateManager::new(&dir);
        mgr.join_room(&invite("ABCD-1234")).unwrap();
        mgr.set_host_instance(Some("NotMine".into())).unwrap();

        let reloaded = RoomStateManager::new(&dir);
        assert!(
            reloaded.host_instance().is_none(),
            "a guest must never own a host instance"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn creating_a_host_room_after_a_guest_session_uses_a_fresh_code() {
        let dir = std::env::temp_dir().join(format!("vl_rooms_role_switch_{}", std::process::id()));
        let mgr = RoomStateManager::new(&dir);
        mgr.join_room(&invite("ABCD-1234")).unwrap();
        mgr.create_room(None).unwrap();
        assert_eq!(mgr.role(), RoomRole::Host);
        assert_ne!(
            mgr.room_id().as_deref(),
            Some("ABCD-1234"),
            "a guest room code must never become a host room code"
        );

        // Creating again while already host is idempotent (same code).
        let code = mgr.room_id();
        mgr.create_room(Some("Second".into())).unwrap();
        assert_eq!(mgr.room_id(), code);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_node_key_still_updates_online_dns_and_ip() {
        let dir = std::env::temp_dir().join(format!(
            "vl_rooms_host_update_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mgr = RoomStateManager::new(&dir);
        mgr.join_room(&invite("ABCD-1234")).unwrap();

        let host = |online: bool, dns: &str, ip: &str| HostInfo {
            node_key: "node-key-1".into(),
            host_name: "gaming-pc".into(),
            dns_name: dns.into(),
            tailnet_ips: vec![ip.into()],
            online,
        };

        mgr.set_host(Some(host(true, "old.tail.ts.net", "100.64.0.1")))
            .unwrap();
        // Same node_key, but the host went offline: guests must see it.
        mgr.set_host(Some(host(false, "old.tail.ts.net", "100.64.0.1")))
            .unwrap();
        assert!(!mgr.snapshot().host.unwrap().online);

        // Reconnect with a new MagicDNS name and IP.
        mgr.set_host(Some(host(true, "new.tail.ts.net", "100.64.0.9")))
            .unwrap();
        let current = mgr.snapshot().host.expect("host");
        assert!(current.online);
        assert_eq!(current.dns(), Some("new.tail.ts.net"));
        assert_eq!(current.ip(), Some("100.64.0.9"));

        // The live update is written to disk, but a restart is a new session:
        // the old endpoint must not be presented as a live host before
        // discovery runs again.
        let on_disk = resolve_persisted(&crate::rooms::rooms_dir(&dir)).expect("file");
        assert_eq!(
            on_disk.host.as_ref().and_then(|h| h.dns()),
            Some("new.tail.ts.net"),
            "the live session persists its resolved host"
        );
        let reloaded = RoomStateManager::new(&dir);
        assert!(
            reloaded.snapshot().host.is_none(),
            "a persisted endpoint is cleared on the next load"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

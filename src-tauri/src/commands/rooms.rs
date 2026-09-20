// ==================== Room Commands ====================
// HOST/GUEST room workflows backed by rooms/ modules. Everything here is a
// thin orchestration layer: it validates, delegates to the rooms modules,
// and forwards progress to the frontend as events.

use crate::events;
use crate::rooms::minecraft_connector::{
    compose_endpoint, parse_quick_play_confirmation, MinecraftConnector,
};
use crate::rooms::peer_discovery::{resolve_host_for_room, HostInfo};
use crate::rooms::room_state::{RoomRole, RoomStateManager, RoomStateRow};
use crate::rooms::tailscale::{TailscaleManager, TailscaleStatus, TailscaleStatusPublic};
use crate::AppState;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

fn data_dir_of(state: &State<'_, AppState>) -> std::path::PathBuf {
    state
        .config
        .lock()
        .map(|c| c.data_dir.clone())
        .unwrap_or_default()
}

/// Full room snapshot consumed by the Rooms UI.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomStatusPublic {
    pub role: String,
    pub room_id: Option<String>,
    pub room_name: Option<String>,
    pub tailscale: TailscaleStatusPublic,
    pub host: Option<HostInfo>,
    pub minecraft_port: Option<u16>,
    pub bridge_port: u16,
    pub endpoint: Option<String>,
    pub host_online: bool,
}

/// The host the room UI should display. Guests use the peer discovered over
/// the tailnet; the HOST machine has no peer to discover, so it presents its
/// own node — this is what lets the host UI show port-found/endpoint state
/// without ever having resolved a guest-side peer.
fn display_host_for(room: &RoomStateRow, status: Option<&TailscaleStatus>) -> Option<HostInfo> {
    room.host.clone().or_else(|| {
        if room.role == RoomRole::Host {
            status.and_then(TailscaleManager::self_host_info)
        } else {
            None
        }
    })
}

/// Pure status snapshot. Callers are responsible for keeping the bridge
/// alive (`cmd_room_status`, `cmd_room_create_host`).
pub fn build_status(state: &State<'_, AppState>) -> RoomStatusPublic {
    let room = state.room_state.snapshot();
    let ts = tailscale_manager_for(state);
    let status = ts.status().ok();
    let tailscale = if !ts.is_installed() {
        TailscaleStatusPublic {
            installed: false,
            service_ok: false,
            logged_in: false,
            version: None,
            tailnet: None,
            login_name: None,
            self_ip: None,
            auth_url: None,
        }
    } else {
        crate::rooms::tailscale::public_status(true, status.as_ref())
    };
    let host = display_host_for(&room, status.as_ref());
    let host_online = host.as_ref().map(|h| h.online).unwrap_or(false);
    let endpoint = match (&host, room.minecraft_port) {
        (Some(h), Some(port)) => {
            let addr = h.dns().or_else(|| h.ip()).unwrap_or_default();
            if addr.is_empty() {
                None
            } else {
                Some(compose_endpoint(&addr, port))
            }
        }
        _ => None,
    };

    let role = match room.role {
        RoomRole::Host => "host",
        RoomRole::Guest => "guest",
        RoomRole::None => "none",
    }
    .to_string();

    RoomStatusPublic {
        role,
        room_id: room.room_id,
        room_name: room.room_name,
        tailscale,
        host,
        minecraft_port: room.minecraft_port,
        bridge_port: room.void_link_port,
        endpoint,
        host_online,
    }
}

fn tailscale_manager_for(state: &State<'_, AppState>) -> TailscaleManager {
    TailscaleManager::new(data_dir_of(state))
}

/// Full room status snapshot. Also resumes the host bridge when a Host room
/// survives an app restart.
#[tauri::command]
pub async fn cmd_room_status(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RoomStatusPublic, String> {
    if state.room_state.role() == RoomRole::Host {
        if state.tailscale.is_installed() {
            if let Ok(status) = state.tailscale.status() {
                if TailscaleManager::is_logged_in(&status) {
                    if let Some(ip) = TailscaleManager::self_ip(&status) {
                        let _ = crate::rooms::void_link::ensure_void_link_server(
                            ip,
                            state.room_state.clone(),
                        )
                        .await;
                    }
                }
            }
        }
    }
    let snap = build_status(&state);
    // Diagnostics: while Tailscale setup is stuck, surface the raw facts so
    // the user can see in the launcher log what the CLI actually returned.
    if !snap.tailscale.logged_in {
        log_tailscale_diagnostics(&app, &state);
    }
    Ok(snap)
}

/// Best-effort diagnostics dumped to the launcher log / Terminal page while
/// the Tailscale setup is not ready. Never logs keys or tokens — only the
/// CLI path and parsed status fields, which are safe.
fn log_tailscale_diagnostics(app: &AppHandle, state: &State<'_, AppState>) {
    match crate::rooms::tailscale::tailscale_exe_path() {
        Some(path) => events::emit_log(
            app,
            "info",
            "rooms",
            &format!("Tailscale CLI found: {}", path.display()),
        ),
        None => events::emit_log(
            app,
            "warn",
            "rooms",
            "Tailscale CLI not found under Program Files",
        ),
    }
    match state.tailscale.status() {
        Ok(status) => events::emit_log(
            app,
            "info",
            "rooms",
            &format!(
                "tailscale status --json: backend_state={:?}, version={:?}, self_ip={:?}, login_name={:?}",
                status.backend_state,
                status.version,
                TailscaleManager::self_ip(&status),
                status
                    .user
                    .as_ref()
                    .and_then(|u| u.first())
                    .and_then(|u| u.login_name.clone()),
            ),
        ),
        Err(e) => events::emit_log(
            app,
            "warn",
            "rooms",
            &format!("tailscale status --json failed: {}", e),
        ),
    }
}

/// Make sure Tailscale is installed and the node is logged in.
///
/// * Not installed → spawns the download+install task (this call returns
///   immediately; progress arrives via `room_progress` events).
/// * Installed but not logged in → runs `tailscale up`; `auth_url` in the
///   status is the link the UI opens for browser approval.
#[tauri::command]
pub async fn cmd_room_check_link(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RoomStatusPublic, String> {
    if !state.tailscale.is_installed() {
        let ts = tailscale_manager_for(&state);
        let room = state.room_state.clone();
        tokio::spawn(async move {
            events::emit_room_progress("install", 0.02, "Tailscale is not installed. Installing…");
            let result = ts
                .install_msi(|frac, label| {
                    events::emit_room_progress("install", frac, label);
                })
                .await;
            let _ = room;
            match result {
                Ok(_) => {
                    events::emit_room_progress("install", 1.0, "Tailscale installed");
                    auto_login_and_open(&app, &ts).await;
                }
                Err(e) => {
                    let msg = format!("Install failed: {}", e);
                    // Launcher log file + Terminal page, and a global toast
                    // in the UI (see useRoomEvents → `room_error`).
                    events::emit_log(&app, "error", "rooms", &msg);
                    events::emit_room_error(&msg);
                }
            }
        });
        return Ok(build_status(&state));
    }

    if let Ok(status) = state.tailscale.status() {
        if !TailscaleManager::is_logged_in(&status) {
            events::emit_room_progress("login", 0.0, "Please approve this device in the browser.");
            state
                .tailscale
                .login()
                .map_err(|e| format!("Failed to start Tailscale login: {}", e))?;
        } else {
            events::emit_room_progress("login", 1.0, "Logged in");
        }
    }
    Ok(build_status(&state))
}

/// Create a room as host: stamp the room id, advertise the hostname hint,
/// and start the VoidLink bridge on the tailnet IP.
#[tauri::command]
pub async fn cmd_room_create_host(
    state: State<'_, AppState>,
    room_name: Option<String>,
) -> Result<RoomStatusPublic, String> {
    if !state.tailscale.is_installed() {
        return Err("Tailscale is not installed. Click 'Check connection' first.".into());
    }
    let status = state
        .tailscale
        .status()
        .map_err(|e| format!("Failed to read Tailscale status: {}", e))?;
    if !TailscaleManager::is_logged_in(&status) {
        return Err("You must log in to Tailscale before creating a room.".into());
    }
    let self_ip = TailscaleManager::self_ip(&status)
        .ok_or_else(|| "No tailnet IP — is the Tailscale connection healthy?".to_string())?;

    // Capture the machine's real name BEFORE the room hint renames it, so a
    // later `leave` can put it back (temporary discovery rename side effect).
    if let Some(original) = TailscaleManager::self_hostname(&status) {
        let _ = state.room_state.remember_original_hostname(&original);
    }

    state
        .room_state
        .create_room(room_name)
        .map_err(|e| e.to_string())?;

    if let Some(room_id) = state.room_state.room_id() {
        let hint = format!("{}-host", room_id.to_ascii_lowercase());
        if let Err(e) = state.tailscale.set_hostname(&hint) {
            tracing::warn!(target: "rooms", "Hostname hint not set: {}", e);
        }
    }

    if let Err(e) = crate::rooms::void_link::ensure_void_link_server(self_ip, state.room_state.clone())
        .await
    {
        // The bridge failed to bind (e.g. port 48888 already in use): roll
        // the half-created room back so the launcher is not left in a Host
        // state that has no working bridge. The error is reported to the UI.
        let _ = leave_room_and_restore(&state);
        return Err(e.to_string());
    }

    events::emit_room_progress("ready", 1.0, "Room is ready — share the code with the guest.");
    Ok(build_status(&state))
}

/// Join a room as guest: record the code and start the discovery loop that
/// watches the tailnet for the host machine.
#[tauri::command]
pub fn cmd_room_join_guest(
    app: AppHandle,
    state: State<'_, AppState>,
    room_code: String,
) -> Result<RoomStatusPublic, String> {
    if !state.tailscale.is_installed() {
        return Err("Tailscale is not installed. Click 'Check connection' first.".into());
    }
    let status = state
        .tailscale
        .status()
        .map_err(|e| format!("Failed to read Tailscale status: {}", e))?;
    if !TailscaleManager::is_logged_in(&status) {
        return Err("You must log in to Tailscale before joining a room.".into());
    }

    state
        .room_state
        .join_room(&room_code)
        .map_err(|e| e.to_string())?;

    let room = state.room_state.clone();
    let data_dir = data_dir_of(&state);
    spawn_discovery_loop(app, room, data_dir);

    events::emit_room_progress(
        "discovering",
        0.0,
        "Looking for the host on the tailnet…",
    );
    Ok(build_status(&state))
}

/// Leave the current room. The Tailscale Machine Sharing grant itself stays
/// (it is managed in the Tailscale admin console). The temporary hostname
/// hint is restored and the VoidLink bridge is torn down.
#[tauri::command]
pub fn cmd_room_leave(state: State<'_, AppState>) -> Result<RoomStatusPublic, String> {
    leave_room_and_restore(&state)?;
    Ok(build_status(&state))
}

/// Tear the room down: restore the machine's original hostname (if a room
/// hint renamed it), stop the host-side VoidLink bridge, then clear the room
/// state. Shared by `cmd_room_leave` and the create-host rollback.
fn leave_room_and_restore(state: &State<'_, AppState>) -> Result<(), String> {
    if let Some(name) = state.room_state.previous_hostname() {
        if let Err(e) = state.tailscale.set_hostname(&name) {
            tracing::warn!(target: "rooms", "Hostname restore failed: {}", e);
        }
    }
    crate::rooms::void_link::stop_void_link_server();
    state
        .room_state
        .leave()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Single discovery pass (guest): resolve the host, then read its advertised
/// Minecraft port over VoidLink.
#[tauri::command]
pub async fn cmd_room_refresh(state: State<'_, AppState>) -> Result<RoomStatusPublic, String> {
    if state.room_state.role() != RoomRole::Guest {
        return Ok(build_status(&state));
    }
    run_discovery_pass(&state.room_state, &data_dir_of(&state)).await;
    Ok(build_status(&state))
}

/// Host side: advertise the current Minecraft LAN port (or clear it with
/// `None` when the world is closed).
#[tauri::command]
pub fn cmd_room_report_minecraft_port(
    state: State<'_, AppState>,
    port: Option<u16>,
) -> Result<(), String> {
    state
        .room_state
        .set_minecraft_port(port)
        .map_err(|e| e.to_string())
}

/// Host side: scan the running game's log for an "Open to LAN" port line.
#[tauri::command]
pub fn cmd_room_detect_minecraft_port(_state: State<'_, AppState>) -> Result<Option<u16>, String> {
    let Some(path) = crate::game_logs::get_current_log_path() else {
        return Ok(None);
    };
    let text = crate::game_logs::read_game_log(&path, None).unwrap_or_default();
    let connector = MinecraftConnector::default();
    Ok(connector.detect_port(&text))
}

/// Build extra arguments to auto-join a host (`None` when the MC version
/// cannot be auto-jointed — use Direct Connect instead).
#[tauri::command]
pub fn cmd_room_join_args(
    mc_version: String,
    host: String,
    port: u16,
) -> Result<Option<Vec<String>>, String> {
    if host.trim().is_empty() {
        return Err("Empty host".into());
    }
    Ok(MinecraftConnector::default()
        .discovery()
        .build_join_args(&mc_version, host.trim(), port))
}

/// Confirm an auto-join actually connected. For Quick Play versions (1.20+)
/// the client writes a JSON log at the `--quickPlayPath` the launcher passed
/// it; this reads the current session's copy and reports the joined world.
/// `None` means no confirmation yet (game still loading, or the version has
/// no Quick Play join log — e.g. legacy `--server/--port` up to 1.19.4).
#[tauri::command]
pub fn cmd_room_quick_play_join_status() -> Result<Option<crate::rooms::minecraft_connector::QuickJoinInfo>, String> {
    let Some(path) = crate::game_logs::get_current_log_path() else {
        return Ok(None);
    };
    let quickplay_path = format!("{}.quickplay.json", path);
    let text = std::fs::read_to_string(&quickplay_path).unwrap_or_default();
    Ok(parse_quick_play_confirmation(&text))
}

/// Revoke access: open the Tailscale Admin Console where the owner removes
/// the guest's Machine Sharing grant.
#[tauri::command]
pub fn cmd_room_open_admin_console(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url("https://login.tailscale.com/admin/machines", None::<&str>)
        .map_err(|e| e.to_string())
}

/// Identity + port tuple produced by one discovery pass. The background loop
/// compares consecutive tuples to emit UI events only when something changed.
type DiscoverySnapshot = (Option<String>, bool, Option<u16>);

/// Background task for the guest: poll the tailnet, resolve the host, read
/// its Minecraft port over VoidLink, and notify the UI only on change.
fn spawn_discovery_loop(
    app: AppHandle,
    room: Arc<RoomStateManager>,
    data_dir: std::path::PathBuf,
) {
    tokio::spawn(async move {
        let mut last: Option<DiscoverySnapshot> = None;
        loop {
            if room.snapshot().role != RoomRole::Guest {
                break;
            }
            let current = run_discovery_pass(&room, &data_dir).await;
            if last.as_ref() != Some(&current) {
                last = Some(current);
                let _ = app.emit("room_status_changed", serde_json::json!({}));
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

/// One discovery pass (guest): resolve the host from the current tailnet
/// peers, read its advertised Minecraft port over VoidLink, persist both, and
/// return the resulting key tuple for emit-on-change deduplication. A flaky
/// tailnet or a briefly-unreachable host just yields an absent snapshot —
/// never an error the caller must handle.
async fn run_discovery_pass(
    room: &Arc<RoomStateManager>,
    data_dir: &std::path::Path,
) -> DiscoverySnapshot {
    let snapshot = room.snapshot();
    let code = snapshot.room_id.clone().unwrap_or_default();
    let ts = TailscaleManager::new(data_dir.to_path_buf());
    let Ok(status) = ts.status() else {
        return (None, false, None);
    };
    let peers = crate::rooms::peer_discovery::peers_from_status(&status);
    let Some(host) = resolve_host_for_room(&peers, snapshot.room_id.as_deref()) else {
        let _ = room.set_host(None);
        let _ = room.set_minecraft_port(None);
        return (None, false, None);
    };
    let _ = room.set_host(Some(host.clone()));
    let mc_port = match host.ip() {
        Some(ip) if !code.is_empty() => {
            match crate::rooms::void_link::query_host_status(
                ip,
                snapshot.void_link_port,
                &code,
            )
            .await
            {
                Ok(st) => st.mc_port,
                Err(_) => None,
            }
        }
        _ => None,
    };
    let _ = room.set_minecraft_port(mc_port);
    (Some(host.node_key.clone()), host.online, mc_port)
}

/// Kick off `tailscale up` (detached) right after a fresh install and open
/// the AuthURL in the system browser as soon as it appears, so the user can
/// approve the device without a manual extra step. Retries `tailscale up` a
/// few times because the daemon may still be registering immediately after
/// the MSI install. The "Open login" button in the setup view remains as a
/// fallback if the URL is slow to appear.
async fn auto_login_and_open(app: &AppHandle, ts: &TailscaleManager) {
    events::emit_room_progress("login", 0.0, "Please approve this device in the browser.");

    use tauri_plugin_opener::OpenerExt;
    let mut login_kicks = 0;
    let mut attempts = 0;
    loop {
        if login_kicks < 4 {
            login_kicks += 1;
            let _ = ts.login();
        }
        if let Ok(status) = ts.status() {
            if TailscaleManager::is_logged_in(&status) {
                events::emit_room_progress("login", 1.0, "Logged in");
                return;
            }
            if let Some(url) = TailscaleManager::auth_url(&status) {
                if !url.trim().is_empty() {
                    if app.opener().open_url(&url, None::<&str>).is_ok() {
                        tracing::info!(target: "rooms", "Opened Tailscale login URL in the system browser");
                    } else {
                        events::emit_log(
                            &app,
                            "warn",
                            "rooms",
                            "Could not open the Tailscale login URL automatically — use the Open login button.",
                        );
                    }
                    return;
                }
            }
        }
        attempts += 1;
        if attempts >= 40 {
            events::emit_log(
                &app,
                "warn",
                "rooms",
                "Tailscale login URL has not appeared yet — click 'Check connection' again.",
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rooms::room_state::RoomStateRow;
    use crate::rooms::tailscale::PeerNode;

    fn self_status() -> TailscaleStatus {
        TailscaleStatus {
            version: Some("1.102.4".into()),
            backend_state: Some("Running".into()),
            auth_url: None,
            current_tailnet: None,
            self_node: Some(PeerNode {
                node_key: Some("nodekey:self:me".into()),
                host_name: Some("gaming-pc".into()),
                dns_name: Some("gaming-pc.tail3e2a10.ts.net.".into()),
                tailnet_ips: Some(vec!["100.101.102.103".into()]),
                online: Some(true),
                last_seen: None,
                sharee_node: None,
                logged_in: None,
                user_id: None,
            }),
            peer: None,
            user: None,
            magic_dns_suffix: Some("tail3e2a10.ts.net".into()),
        }
    }

    #[test]
    fn host_role_renders_self_node_as_display_host() {
        let room = RoomStateRow {
            role: RoomRole::Host,
            ..Default::default()
        };
        let host = display_host_for(&room, Some(&self_status())).expect("host has a display host");
        assert_eq!(host.host_name, "gaming-pc");
        assert!(host.online, "self node is online");
    }

    #[test]
    fn guest_role_has_no_self_display_host() {
        let room = RoomStateRow {
            role: RoomRole::Guest,
            ..Default::default()
        };
        assert!(
            display_host_for(&room, Some(&self_status())).is_none(),
            "guests only use a discovered peer"
        );
    }

    #[test]
    fn discovered_peer_wins_over_self_host() {
        let mut room = RoomStateRow {
            role: RoomRole::Host,
            ..Default::default()
        };
        room.host = Some(HostInfo {
            node_key: "nodekey:peer:host".into(),
            host_name: "abcd-1234-host".into(),
            dns_name: "abcd-1234-host.ts.net".into(),
            tailnet_ips: vec!["100.64.0.10".into()],
            online: true,
        });
        let shown = display_host_for(&room, Some(&self_status()));
        assert_eq!(
            shown.as_ref().map(|h| h.node_key.as_str()),
            Some("nodekey:peer:host"),
            "a resolved peer always wins over the self node"
        );
    }

    #[test]
    fn no_self_node_means_no_display_host() {
        let room = RoomStateRow {
            role: RoomRole::Host,
            ..Default::default()
        };
        assert!(
            display_host_for(&room, Some(&TailscaleStatus {
                version: None,
                backend_state: None,
                auth_url: None,
                current_tailnet: None,
                self_node: None,
                peer: None,
                user: None,
                magic_dns_suffix: None,
            }))
            .is_none()
        );
    }
}
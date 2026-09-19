// ==================== Room Commands ====================
// HOST/GUEST room workflows backed by rooms/ modules. Everything here is a
// thin orchestration layer: it validates, delegates to the rooms modules,
// and forwards progress to the frontend as events.

use crate::events;
use crate::rooms::minecraft_connector::{compose_endpoint, MinecraftConnector};
use crate::rooms::peer_discovery::{resolve_host_for_room, HostInfo};
use crate::rooms::room_state::{RoomRole, RoomStateManager};
use crate::rooms::tailscale::{TailscaleManager, TailscaleStatusPublic};
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
    pub discovery_error: Option<String>,
}

fn ts_public_from(ts: &TailscaleManager) -> TailscaleStatusPublic {
    if !ts.is_installed() {
        return TailscaleStatusPublic {
            installed: false,
            service_ok: false,
            logged_in: false,
            version: None,
            tailnet: None,
            login_name: None,
            self_ip: None,
            auth_url: None,
        };
    }
    crate::rooms::tailscale::public_status(true, ts.status().ok().as_ref())
}

/// Pure status snapshot. Callers are responsible for keeping the bridge
/// alive (`cmd_room_status`, `cmd_room_create_host`).
pub fn build_status(state: &State<'_, AppState>) -> RoomStatusPublic {
    let room = state.room_state.snapshot();
    let tailscale = ts_public_from(&tailscale_manager_for(state));
    let host = room.host.clone();
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
        discovery_error: None,
    }
}

fn tailscale_manager_for(state: &State<'_, AppState>) -> TailscaleManager {
    TailscaleManager::new(data_dir_of(state))
}

/// Full room status snapshot. Also resumes the host bridge when a Host room
/// survives an app restart.
#[tauri::command]
pub async fn cmd_room_status(
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
    Ok(build_status(&state))
}

/// Make sure Tailscale is installed and the node is logged in.
///
/// * Not installed → spawns the download+install task (this call returns
///   immediately; progress arrives via `room_progress` events).
/// * Installed but not logged in → runs `tailscale up`; `auth_url` in the
///   status is the link the UI opens for browser approval.
#[tauri::command]
pub async fn cmd_room_check_link(
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
                Ok(_) => events::emit_room_progress("install", 1.0, "Tailscale installed"),
                Err(e) => events::emit_room_progress(
                    "install",
                    1.0,
                    &format!("Install failed: {}", e),
                ),
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
        let _ = state.room_state.leave();
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
/// (it is managed in the Tailscale admin console).
#[tauri::command]
pub fn cmd_room_leave(state: State<'_, AppState>) -> Result<RoomStatusPublic, String> {
    state.room_state.leave().map_err(|e| e.to_string())?;
    Ok(build_status(&state))
}

/// Single discovery pass (guest): resolve the host, then read its advertised
/// Minecraft port over VoidLink.
#[tauri::command]
pub async fn cmd_room_refresh(state: State<'_, AppState>) -> Result<RoomStatusPublic, String> {
    if state.room_state.role() != RoomRole::Guest {
        return Ok(build_status(&state));
    }
    run_discovery_pass(&state).await;
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

/// Revoke access: open the Tailscale Admin Console where the owner removes
/// the guest's Machine Sharing grant.
#[tauri::command]
pub fn cmd_room_open_admin_console(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url("https://login.tailscale.com/admin/machines", None::<&str>)
        .map_err(|e| e.to_string())
}

/// Background task for the guest: poll the tailnet, resolve the host, read
/// its Minecraft port over VoidLink, and notify the UI on any change.
fn spawn_discovery_loop(
    app: AppHandle,
    room: Arc<RoomStateManager>,
    data_dir: std::path::PathBuf,
) {
    tokio::spawn(async move {
        loop {
            let snapshot = room.snapshot();
            if snapshot.role != RoomRole::Guest {
                break;
            }
            let code = snapshot.room_id.clone().unwrap_or_default();
            let ts = TailscaleManager::new(data_dir.clone());
            match ts.status() {
                Ok(status) => {
                    let peers = crate::rooms::peer_discovery::peers_from_status(&status);
                    if let Some(host) =
                        resolve_host_for_room(&peers, snapshot.room_id.as_deref())
                    {
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
                    } else {
                        let _ = room.set_host(None);
                        let _ = room.set_minecraft_port(None);
                    }
                    let _ = app.emit("room_status_changed", serde_json::json!({}));
                }
                Err(_) => {}
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

/// One synchronous discovery pass (used by `cmd_room_refresh`).
async fn run_discovery_pass(state: &State<'_, AppState>) {
    let snapshot = state.room_state.snapshot();
    let code = snapshot.room_id.clone().unwrap_or_default();
    let ts = tailscale_manager_for(state);
    if let Ok(status) = ts.status() {
        let peers = crate::rooms::peer_discovery::peers_from_status(&status);
        if let Some(host) = resolve_host_for_room(&peers, snapshot.room_id.as_deref()) {
            let _ = state.room_state.set_host(Some(host.clone()));
            if let Some(ip) = host.ip() {
                if !code.is_empty() {
                    if let Ok(st) = crate::rooms::void_link::query_host_status(
                        ip,
                        snapshot.void_link_port,
                        &code,
                    )
                    .await
                    {
                        let _ = state.room_state.set_minecraft_port(st.mc_port);
                    }
                }
            }
        } else {
            let _ = state.room_state.set_host(None);
            let _ = state.room_state.set_minecraft_port(None);
        }
    }
}
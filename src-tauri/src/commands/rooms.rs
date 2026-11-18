// ==================== Room Commands ====================
// HOST/GUEST room workflows backed by rooms/ modules. Everything here is a
// thin orchestration layer: it validates, delegates to the rooms modules,
// and forwards progress to the frontend as events.

use crate::events;
use crate::rooms::minecraft_connector::{
    compose_endpoint, parse_quick_play_confirmation, LanPortTracker, MinecraftConnector,
};
use crate::rooms::peer_discovery::{resolve_host_for_room, HostInfo, PeerInfo};
use crate::rooms::room_state::{RoomRole, RoomStateManager, RoomStateRow};
use crate::rooms::tailscale::{TailscaleManager, TailscaleStatus, TailscaleStatusPublic};
use crate::rooms::void_link::VoidLinkStatus;
use crate::AppState;
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

fn data_dir_of(state: &State<'_, AppState>) -> std::path::PathBuf {
    state
        .config
        .lock()
        .map(|c| c.data_dir.clone())
        .unwrap_or_default()
}

/// Is the experimental Rooms feature enabled in Settings?
fn rooms_enabled(state: &State<'_, AppState>) -> bool {
    state
        .config
        .lock()
        .map(|c| c.rooms_enabled)
        .unwrap_or(false)
}

/// Rooms commands are unreachable from the UI while the feature is off, but a
/// stale IPC call must not create a room or touch Tailscale either.
fn require_rooms_enabled(state: &State<'_, AppState>) -> Result<(), String> {
    if rooms_enabled(state) {
        Ok(())
    } else {
        Err("Rooms are disabled. Enable them in Settings -> Experimental features.".to_string())
    }
}

/// A LAN port of 0 is never a real Minecraft server and must not be
/// advertised to guests as a joinable endpoint.
fn valid_minecraft_port(port: Option<u16>) -> bool {
    port != Some(0)
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
    /// Instance registered as the host of this room. A local instance name,
    /// never the invitation secret. The UI uses it to restore the selection
    /// instead of guessing the first instance in the list.
    pub host_instance: Option<String>,
}

/// The host the room UI should display. Guests use the peer discovered over
/// the tailnet; the HOST machine has no peer to discover, so it presents its
/// own node вЂ” this is what lets the host UI show port-found/endpoint state
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

/// Snapshot from an already-fetched Tailscale status. Async commands fetch the
/// status in `spawn_blocking` and call this, so no blocking process runs on the
/// async runtime.
fn build_status_with(
    state: &State<'_, AppState>,
    installed: bool,
    status: Option<&TailscaleStatus>,
) -> RoomStatusPublic {
    build_status_from_row(&state.room_state.snapshot(), installed, status)
}

/// Pure snapshot builder shared by the command path and the regression tests.
fn build_status_from_row(
    room: &RoomStateRow,
    installed: bool,
    status: Option<&TailscaleStatus>,
) -> RoomStatusPublic {
    let tailscale = if !installed {
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
        crate::rooms::tailscale::public_status(true, status)
    };
    let host = display_host_for(room, status);
    let host_online = host.as_ref().map(|h| h.online).unwrap_or(false);
    let endpoint = match (&host, room.minecraft_port) {
        (Some(h), Some(port)) => {
            let addr = h.dns().or_else(|| h.ip()).unwrap_or_default();
            if addr.is_empty() {
                None
            } else {
                Some(compose_endpoint(addr, port))
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
        room_id: room.room_id.clone(),
        room_name: room.room_name.clone(),
        tailscale,
        host,
        minecraft_port: room.minecraft_port,
        bridge_port: room.void_link_port,
        endpoint,
        host_online,
        host_instance: room.host_instance.clone(),
    }
}

/// Fetch Tailscale status off the async runtime, then build the snapshot.
async fn build_status_async(state: &State<'_, AppState>) -> RoomStatusPublic {
    let ts = tailscale_manager_for(state);
    let installed = ts.is_installed();
    let status = tauri::async_runtime::spawn_blocking(move || ts.status().ok())
        .await
        .ok()
        .flatten();
    build_status_with(state, installed, status.as_ref())
}

fn tailscale_manager_for(state: &State<'_, AppState>) -> TailscaleManager {
    TailscaleManager::new(data_dir_of(state))
}

/// Full room status snapshot. Resumes the host bridge or guest discovery loop
/// when a room survived an app restart. Never starts a bridge, login or
/// discovery while the experimental Rooms feature is disabled.
#[tauri::command]
pub async fn cmd_room_status(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RoomStatusPublic, String> {
    let ts = tailscale_manager_for(&state);
    let installed = ts.is_installed();
    let status = tauri::async_runtime::spawn_blocking(move || ts.status().ok())
        .await
        .ok()
        .flatten();

    if !rooms_enabled(&state) {
        return Ok(build_status_with(&state, installed, status.as_ref()));
    }

    let bridge_ip = if state.room_state.role() == RoomRole::Host && installed {
        status
            .as_ref()
            .filter(|s| TailscaleManager::is_logged_in(s))
            .and_then(TailscaleManager::self_ip)
    } else {
        None
    };
    if let Some(ip) = bridge_ip {
        let _ =
            crate::rooms::void_link::ensure_void_link_server(ip, state.room_state.clone()).await;
    }
    if state.room_state.role() == RoomRole::Host {
        if let Some(instance) = state.room_state.host_instance() {
            ensure_host_monitor(
                app.clone(),
                state.room_state.clone(),
                data_dir_of(&state),
                instance,
            )
            .await;
        }
    }
    if state.room_state.role() == RoomRole::Guest {
        // A guest room survives a restart; discovery must resume without the
        // user re-entering the code. `ensure` is idempotent, so the status poll
        // never spawns a second loop.
        ensure_discovery_loop(app.clone(), state.room_state.clone(), data_dir_of(&state)).await;
    }
    // Status polling stays silent: this command runs every few seconds while
    // the setup screen is open, so diagnostics here flooded the log with the
    // same warning. The explicit "check connection" action below logs once.
    Ok(build_status_with(&state, installed, status.as_ref()))
}

/// Best-effort diagnostics dumped to the launcher log / Terminal page while
/// the Tailscale setup is not ready. Never logs keys or tokens вЂ” only the
/// CLI path and parsed status fields, which are safe.
fn log_tailscale_diagnostics(app: &AppHandle) {
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
}

async fn log_tailscale_status_diagnostics(app: &AppHandle, state: &State<'_, AppState>) {
    let ts = state.tailscale.clone();
    let status = tauri::async_runtime::spawn_blocking(move || ts.status())
        .await
        .ok()
        .and_then(|r| r.ok());
    match status {
        Some(status) => events::emit_log(
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
        None => events::emit_log(
            app,
            "warn",
            "rooms",
            "tailscale status --json failed or timed out",
        ),
    }
}

/// Refresh the Tailscale snapshot. Side-effect free: it never installs or
/// signs in to Tailscale. Installation and login are separate commands that
/// the UI only calls after an explicit user confirmation.
#[tauri::command]
pub async fn cmd_room_check_link(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RoomStatusPublic, String> {
    require_rooms_enabled(&state)?;
    // Diagnostics belong to the explicit user action, not to the status poll.
    // The status call is bounded but still a blocking process: keep it off
    // the async runtime.
    log_tailscale_diagnostics(&app);
    log_tailscale_status_diagnostics(&app, &state).await;
    Ok(build_status_async(&state).await)
}

/// A single Tailscale install may run at a time: two rapid clicks must not
/// download the MSI twice or start a second msiexec behind the first.
static TAILSCALE_INSTALL_RUNNING: AtomicBool = AtomicBool::new(false);

/// One interactive Tailscale login flow at a time. The flag is released by the
/// URL watcher after success, error, timeout or cancellation.
struct LoginGate {
    running: AtomicBool,
    cancelled: AtomicBool,
}

impl LoginGate {
    const fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        }
    }

    fn try_begin(&self) -> bool {
        self.cancelled.store(false, Ordering::SeqCst);
        !self.running.swap(true, Ordering::SeqCst)
    }

    fn finish(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

static TAILSCALE_LOGIN: LoginGate = LoginGate::new();

/// Download, verify and install Tailscale after the user explicitly confirmed
/// it in the UI. Returns immediately; progress arrives via `room_progress`
/// events and the setup screen keeps polling `cmd_room_status`.
#[tauri::command]
pub async fn cmd_room_install_tailscale(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RoomStatusPublic, String> {
    require_rooms_enabled(&state)?;
    let ts = tailscale_manager_for(&state);
    if ts.is_installed() {
        return Ok(build_status_async(&state).await);
    }
    if TAILSCALE_INSTALL_RUNNING.swap(true, Ordering::SeqCst) {
        return Err("Tailscale installation is already in progress.".to_string());
    }
    tokio::spawn(async move {
        events::emit_room_progress(
            "install",
            0.02,
            "Installing Tailscale after confirmation...",
        );
        let result = ts
            .install_msi(|frac, label| {
                events::emit_room_progress("install", frac, label);
            })
            .await;
        TAILSCALE_INSTALL_RUNNING.store(false, Ordering::SeqCst);
        match result {
            Ok(_) => {
                events::emit_room_progress(
                    "install",
                    1.0,
                    "Tailscale installed - sign in to continue",
                );
                events::emit_log(
                    &app,
                    "info",
                    "rooms",
                    "Tailscale installed. Sign in explicitly to continue.",
                );
            }
            Err(e) => {
                let msg = format!("Tailscale install failed: {}", e);
                events::emit_log(&app, "error", "rooms", &msg);
                events::emit_room_error(&msg);
            }
        }
    });
    Ok(build_status_async(&state).await)
}

/// Start the interactive Tailscale login only after the user clicks "Sign in".
/// The browser approval itself remains the final, explicit consent step; the
/// launcher never asks for or stores Tailscale credentials.
#[tauri::command]
pub async fn cmd_room_login(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RoomStatusPublic, String> {
    require_rooms_enabled(&state)?;
    if !state.tailscale.is_installed() {
        return Err("Tailscale is not installed. Install it first.".to_string());
    }
    let status = {
        let ts = state.tailscale.clone();
        tauri::async_runtime::spawn_blocking(move || ts.status())
            .await
            .ok()
            .and_then(|r| r.ok())
    };
    if let Some(status) = status.as_ref() {
        if TailscaleManager::is_logged_in(status) {
            events::emit_room_progress("login", 1.0, "Logged in");
            return Ok(build_status_async(&state).await);
        }
    }
    if !TAILSCALE_LOGIN.try_begin() {
        return Err("Tailscale sign-in is already in progress.".to_string());
    }
    events::emit_room_progress("login", 0.0, "Opening the Tailscale sign-in page...");
    if let Err(e) = state.tailscale.login() {
        TAILSCALE_LOGIN.finish();
        return Err(format!("Failed to start Tailscale login: {}", e));
    }
    open_login_url_when_ready(app.clone(), tailscale_manager_for(&state));
    Ok(build_status_async(&state).await)
}

/// Create a room as host: generate the public room id, its invitation secret
/// and start the VoidLink bridge on the tailnet IP. The machine's Tailscale
/// hostname is never renamed: room discovery authenticates the host over the
/// bridge instead, so Rooms cannot silently change the tailnet identity of
/// this computer.
#[tauri::command]
pub async fn cmd_room_create_host(
    state: State<'_, AppState>,
    room_name: Option<String>,
) -> Result<RoomStatusPublic, String> {
    require_rooms_enabled(&state)?;
    if !state.tailscale.is_installed() {
        return Err("Tailscale is not installed. Open Rooms setup and install it first.".into());
    }
    let ts = tailscale_manager_for(&state);
    let status = tauri::async_runtime::spawn_blocking(move || ts.status())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("Failed to read Tailscale status: {}", e))?;
    if !TailscaleManager::is_logged_in(&status) {
        return Err("You must log in to Tailscale before creating a room.".into());
    }
    let self_ip = TailscaleManager::self_ip(&status)
        .ok_or_else(|| "No tailnet IP - is the Tailscale connection healthy?".to_string())?;

    // Switching from a guest session must not leave its discovery loop running.
    abort_discovery_loop().await;
    stop_host_monitor().await;
    state
        .room_state
        .create_room(room_name)
        .map_err(|e| e.to_string())?;

    if let Err(e) =
        crate::rooms::void_link::ensure_void_link_server(self_ip, state.room_state.clone()).await
    {
        // The bridge failed to bind (e.g. port 48888 already in use): roll
        // the half-created room back so the launcher is not left in a Host
        // state that has no working bridge. The error is reported to the UI.
        let _ = leave_room_and_restore(&state).await;
        return Err(e.to_string());
    }

    events::emit_room_progress(
        "ready",
        1.0,
        "Room is ready - share the invitation with the guest.",
    );
    Ok(build_status_async(&state).await)
}

/// Join a room as guest: verify the full invitation, record it and start the
/// discovery loop that watches the tailnet for the host machine.
#[tauri::command]
pub async fn cmd_room_join_guest(
    app: AppHandle,
    state: State<'_, AppState>,
    room_code: String,
) -> Result<RoomStatusPublic, String> {
    require_rooms_enabled(&state)?;
    if !state.tailscale.is_installed() {
        return Err("Tailscale is not installed. Open Rooms setup and install it first.".into());
    }

    // Validate the invitation BEFORE stopping an existing host bridge. A typo
    // must not tear down a working room.
    if crate::rooms::room_state::parse_invitation(&room_code).is_none() {
        return Err(
            "Invalid invitation. Paste the full invitation copied by the host (it starts with VLR1:)."
                .to_string(),
        );
    }

    let status = {
        let ts = TailscaleManager::new(data_dir_of(&state));
        tauri::async_runtime::spawn_blocking(move || ts.status())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("Failed to read Tailscale status: {}", e))?
    };
    if !TailscaleManager::is_logged_in(&status) {
        return Err("You must log in to Tailscale before joining a room.".into());
    }

    // Joining as a guest while this machine still hosts a room must stop the
    // host bridge; otherwise the old listener would outlive its room.
    if state.room_state.role() == RoomRole::Host {
        stop_host_monitor().await;
        crate::rooms::void_link::stop_void_link_server().await;
    }

    state
        .room_state
        .join_room(room_code.trim())
        .map_err(|e| e.to_string())?;

    let room = state.room_state.clone();
    let data_dir = data_dir_of(&state);
    start_discovery_loop(app, room, data_dir).await;

    events::emit_room_progress("discovering", 0.0, "Looking for the host on the tailnet...");
    Ok(build_status_async(&state).await)
}

/// Leave the current room. The Tailscale Machine Sharing grant itself stays
/// (it is managed in the Tailscale admin console). The temporary hostname
/// hint of a room created by an older build is restored and the VoidLink
/// bridge is torn down.
#[tauri::command]
pub async fn cmd_room_leave(state: State<'_, AppState>) -> Result<RoomStatusPublic, String> {
    leave_room_and_restore(&state).await?;
    Ok(build_status_async(&state).await)
}

/// Is there any room session (host or guest) right now? Used by Settings to
/// warn before the experimental Rooms feature is disabled. Never starts a
/// bridge or a discovery loop.
#[tauri::command]
pub fn cmd_room_session_active(state: State<'_, AppState>) -> bool {
    state.room_state.role() != RoomRole::None
}

/// Host side: return the copyable invitation (public room id + secret).
/// Deliberately separate from `cmd_room_status`: the status poll never carries
/// the secret, and only an explicit host action exposes the invitation.
#[tauri::command]
pub fn cmd_room_invitation(state: State<'_, AppState>) -> Result<String, String> {
    require_rooms_enabled(&state)?;
    state
        .room_state
        .host_invitation()
        .ok_or_else(|| "Only the host has a room invitation.".to_string())
}

/// Host side: register which instance owns this room. The backend then scans
/// exactly that instance's session log, so a second running game can never
/// advertise its LAN port and a closed page cannot leave a stale endpoint.
#[tauri::command]
pub async fn cmd_room_set_host_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    instance: Option<String>,
) -> Result<(), String> {
    require_rooms_enabled(&state)?;
    if state.room_state.role() != RoomRole::Host {
        return Err("Only the host can register a room instance.".to_string());
    }
    let instance = instance
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    state
        .room_state
        .set_host_instance(instance.clone())
        .map_err(|e| e.to_string())?;
    match instance {
        Some(instance) => {
            start_host_monitor(app, state.room_state.clone(), data_dir_of(&state), instance).await;
        }
        None => stop_host_monitor().await,
    }
    Ok(())
}

/// Tear the room down: restore the machine's original hostname (if a room
/// hint from an older build renamed it), stop the host-side VoidLink bridge,
/// stop background monitors, then clear the room state. Shared by
/// `cmd_room_leave`, the create-host rollback, config changes and app start.
pub(crate) async fn leave_room_and_restore(state: &State<'_, AppState>) -> Result<(), String> {
    abort_discovery_loop().await;
    stop_host_monitor().await;
    TAILSCALE_LOGIN.cancel();
    let original_hostname = state.room_state.previous_hostname();
    // Clear the room role BEFORE stopping the bridge and before the (possibly
    // slow) Tailscale hostname restore. A status poll that was already in
    // flight must see role=None when it reaches `ensure` and must not
    // recreate the bridge; and a room created while the CLI restore is still
    // running must not be cleared by this teardown.
    let leave = state
        .room_state
        .leave()
        .map(|_| ())
        .map_err(|e| e.to_string());
    crate::rooms::void_link::stop_void_link_server().await;
    if let Some(name) = original_hostname {
        let ts = state.tailscale.clone();
        match tauri::async_runtime::spawn_blocking(move || ts.set_hostname(&name)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(target: "rooms", "Hostname restore failed: {}", e),
            Err(e) => tracing::warn!(target: "rooms", "Hostname restore task failed: {}", e),
        }
    }
    leave
}

/// Best-effort teardown used when Rooms is disabled or at startup with a
/// stale persisted room. Returns the persistence error instead of silently
/// claiming that the room was cleared.
pub(crate) async fn teardown_room_session(state: &State<'_, AppState>) -> Result<(), String> {
    leave_room_and_restore(state).await
}

/// Single discovery pass (guest): resolve the host, then read its advertised
/// Minecraft port over VoidLink.
#[tauri::command]
pub async fn cmd_room_refresh(state: State<'_, AppState>) -> Result<RoomStatusPublic, String> {
    require_rooms_enabled(&state)?;
    if state.room_state.role() != RoomRole::Guest {
        return Ok(build_status_async(&state).await);
    }
    run_discovery_pass(&state.room_state, &data_dir_of(&state)).await;
    Ok(build_status_async(&state).await)
}

/// Host side: advertise the current Minecraft LAN port (or clear it with
/// `None` when the world is closed).
#[tauri::command]
pub fn cmd_room_report_minecraft_port(
    state: State<'_, AppState>,
    port: Option<u16>,
) -> Result<(), String> {
    require_rooms_enabled(&state)?;
    if state.room_state.role() != RoomRole::Host {
        return Err("Only the host can advertise a Minecraft port.".to_string());
    }
    if !valid_minecraft_port(port) {
        return Err("Minecraft LAN port 0 cannot be advertised.".to_string());
    }
    state
        .room_state
        .set_minecraft_port(port)
        .map_err(|e| e.to_string())
}

/// Host side: scan the session log of the instance that owns the room. A
/// missing/ambiguous session yields `None`; the global "last launched" log is
/// never used because another running instance could own it.
#[tauri::command]
pub async fn cmd_room_detect_minecraft_port(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Option<u16>, String> {
    require_rooms_enabled(&state)?;
    if state.room_state.role() != RoomRole::Host {
        return Ok(None);
    }
    let instance_name = instance_name.trim().to_string();
    if instance_name.is_empty()
        || state.room_state.host_instance().as_deref() != Some(&instance_name)
    {
        return Ok(None);
    }
    if state.launches.phase(&instance_name) != Some(crate::launch_state::LaunchPhase::Running) {
        return Ok(None);
    }
    let data_dir = data_dir_of(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = crate::game_logs::newest_session_for_instance(&data_dir, &instance_name)
        else {
            return Ok(None);
        };
        Ok(detect_lan_port_in_file(&path))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Build extra arguments to auto-join a host (`None` when the MC version
/// cannot be auto-jointed вЂ” use Direct Connect instead).
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
/// no Quick Play join log вЂ” e.g. legacy `--server/--port` up to 1.19.4).
#[tauri::command]
pub async fn cmd_room_quick_play_join_status(
) -> Result<Option<crate::rooms::minecraft_connector::QuickJoinInfo>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(path) = crate::game_logs::get_current_log_path() else {
            return Ok(None);
        };
        let quickplay_path = format!("{}.quickplay.json", path);
        let text = std::fs::read_to_string(&quickplay_path).unwrap_or_default();
        Ok(parse_quick_play_confirmation(&text))
    })
    .await
    .map_err(|e| e.to_string())?
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

/// Serialized owner of one background loop (guest discovery or host port
/// monitor). A generation token makes stale loops exit even if their task was
/// scheduled one last time after an abort.
struct LoopSupervisor {
    op: tokio::sync::Mutex<()>,
    slot: std::sync::Mutex<Option<(u64, tokio::task::JoinHandle<()>)>>,
    generation: std::sync::atomic::AtomicU64,
}

impl LoopSupervisor {
    fn new() -> Self {
        Self {
            op: tokio::sync::Mutex::new(()),
            slot: std::sync::Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    /// Replace whatever loop was running. The operation lock is the only
    /// lock held across an await; the std slot mutex is released before the
    /// old task is awaited.
    async fn start<F>(&self, spawn: F) -> u64
    where
        F: FnOnce(u64) -> tokio::task::JoinHandle<()>,
    {
        let _op = self.op.lock().await;
        let generation = self
            .generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let old = {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            slot.take()
        };
        if let Some((_, handle)) = old {
            handle.abort();
            let _ = handle.await;
        }
        let handle = spawn(generation);
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, handle));
        generation
    }

    /// Start only when no live loop exists. Serialized with `start`/`stop`, so
    /// a status poll cannot race a join and spawn a second loop.
    async fn ensure<F>(&self, spawn: F) -> bool
    where
        F: FnOnce(u64) -> tokio::task::JoinHandle<()>,
    {
        let _op = self.op.lock().await;
        let live = {
            let slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            slot.as_ref()
                .is_some_and(|(_, handle)| !handle.is_finished())
        };
        if live {
            return false;
        }
        let generation = self
            .generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let handle = spawn(generation);
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, handle));
        true
    }

    /// Cancel the current loop and invalidate its generation.
    async fn stop(&self) {
        let _op = self.op.lock().await;
        self.generation.fetch_add(1, Ordering::SeqCst);
        let old = {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            slot.take()
        };
        if let Some((_, handle)) = old {
            handle.abort();
            let _ = handle.await;
        }
    }

    #[cfg(test)]
    fn live(&self) -> bool {
        self.slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|(_, handle)| !handle.is_finished())
    }
}

static DISCOVERY_SUPERVISOR: std::sync::OnceLock<LoopSupervisor> = std::sync::OnceLock::new();
static HOST_MONITOR_SUPERVISOR: std::sync::OnceLock<LoopSupervisor> = std::sync::OnceLock::new();

fn discovery_supervisor() -> &'static LoopSupervisor {
    DISCOVERY_SUPERVISOR.get_or_init(LoopSupervisor::new)
}

fn host_monitor_supervisor() -> &'static LoopSupervisor {
    HOST_MONITOR_SUPERVISOR.get_or_init(LoopSupervisor::new)
}

/// Cancel any running guest discovery loop (leave, feature disable, host
/// creation or a new join). The generation token also stops a task that was
/// already scheduled when the abort was requested.
async fn abort_discovery_loop() {
    discovery_supervisor().stop().await;
}

/// Background task for the guest: poll the tailnet, resolve the host, read
/// its Minecraft port over VoidLink, and notify the UI only on change.
async fn discovery_loop(
    app: AppHandle,
    room: Arc<RoomStateManager>,
    data_dir: std::path::PathBuf,
    generation: u64,
) {
    let mut last: Option<DiscoverySnapshot> = None;
    loop {
        if !discovery_supervisor().is_current(generation) || room.snapshot().role != RoomRole::Guest
        {
            break;
        }
        let current = run_discovery_pass(&room, &data_dir).await;
        if !discovery_supervisor().is_current(generation) {
            break;
        }
        if last.as_ref() != Some(&current) {
            last = Some(current);
            let _ = app.emit("room_status_changed", serde_json::json!({}));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Replace the loop after a fresh join (new room code or new session).
async fn start_discovery_loop(
    app: AppHandle,
    room: Arc<RoomStateManager>,
    data_dir: std::path::PathBuf,
) {
    discovery_supervisor()
        .start(move |generation| tokio::spawn(discovery_loop(app, room, data_dir, generation)))
        .await;
}

/// Idempotent resume used by the status poll: a guest room restored after an
/// app restart starts discovery again, but a live loop is never duplicated.
async fn ensure_discovery_loop(
    app: AppHandle,
    room: Arc<RoomStateManager>,
    data_dir: std::path::PathBuf,
) {
    discovery_supervisor()
        .ensure(move |generation| tokio::spawn(discovery_loop(app, room, data_dir, generation)))
        .await;
}

/// Decide whether a persisted host room still has a verified live game.
/// `scan_games` confirms a process by executable path and exact creation time,
/// so a pid file left behind by a game that exited (or a reused pid) never
/// counts as "still running".
fn confirmed_running_game<'a>(
    scan: &'a crate::running_state::LiveScan,
    instance: &str,
) -> Option<&'a crate::running_state::RunningGame> {
    scan.confirmed
        .iter()
        .find(|game| game.instance.eq_ignore_ascii_case(instance))
}

/// Clear a host room whose registered game did not survive the restart. The
/// port and the instance binding are both stale once no verified process owns
/// them; opening Rooms again re-registers the selected instance.
fn clear_stale_host_resume(room: &RoomStateManager) {
    let _ = room.set_minecraft_port(None);
    let _ = room.set_host_instance(None);
}

/// Resume the host-side control plane after an app restart without waiting
/// for the Rooms page to open. The persisted `host_instance` is only a hint;
/// the game must still be present in the live process table with the exact
/// identity recorded at launch. If it is gone (or cannot be verified), the
/// stale endpoint is cleared instead of being trusted.
pub(crate) async fn resume_host_after_startup(app: AppHandle, state: State<'_, AppState>) {
    if !rooms_enabled(&state) || state.room_state.role() != RoomRole::Host {
        return;
    }
    let Some(instance) = state.room_state.host_instance() else {
        return;
    };
    let data_dir = data_dir_of(&state);
    let scan_dir = data_dir.clone();
    let scan =
        tauri::async_runtime::spawn_blocking(move || crate::running_state::scan_games(&scan_dir))
            .await
            .ok();

    // The user may have left or switched rooms while the scan was running.
    if !rooms_enabled(&state)
        || state.room_state.role() != RoomRole::Host
        || state.room_state.host_instance().as_deref() != Some(instance.as_str())
    {
        return;
    }

    let Some(game) = scan
        .as_ref()
        .and_then(|s| confirmed_running_game(s, &instance))
    else {
        // No verified game session: the old LAN port must not survive the
        // restart. Clear the stale instance binding too; opening Rooms again
        // re-registers the actually selected instance.
        clear_stale_host_resume(&state.room_state);
        return;
    };

    // Register the recovered process in the existing launch registry before
    // starting the monitor. The monitor reads the registry (not the pid file)
    // on every tick, so this is the same path a normal launch uses.
    state.launches.adopt_running(&instance, game.identity());

    // The room survived, so its control bridge should too. This is the same
    // `ensure_void_link_server` path the Rooms status poll uses; without it a
    // guest would stay disconnected until the host happened to open Rooms.
    let ts = tailscale_manager_for(&state);
    let status = tauri::async_runtime::spawn_blocking(move || ts.status().ok())
        .await
        .ok()
        .flatten();
    if let Some(ip) = status
        .as_ref()
        .filter(|status| TailscaleManager::is_logged_in(status))
        .and_then(TailscaleManager::self_ip)
    {
        if let Err(e) =
            crate::rooms::void_link::ensure_void_link_server(ip, state.room_state.clone()).await
        {
            tracing::warn!(target: "rooms", "Could not resume the host bridge after restart: {}", e);
        }
    }

    ensure_host_monitor(app, state.room_state.clone(), data_dir, instance).await;
}

/// How much new log data one monitor tick may consume. A session that is
/// already large is caught up over subsequent ticks instead of blocking the
/// async runtime with an unbounded read.
const HOST_LOG_READ_LIMIT: u64 = 8 * 1024 * 1024;
/// Maximum buffered partial line. LAN lines are short; a pathological
/// unterminated line must not grow the monitor's memory without bound.
const HOST_LOG_CARRY_LIMIT: usize = 64 * 1024;
/// A process identity that cannot be read on this many consecutive monitor
/// ticks is treated as unverifiable; keeping a stale LAN endpoint is worse
/// than clearing it and letting the host re-detect the port later.
const HOST_IDENTITY_UNKNOWN_LIMIT: u32 = 3;

/// Incremental, stateful reader for one Minecraft session log.
///
/// It keeps the byte offset, the unfinished last line and the last confirmed
/// LAN state. The newest session path is supplied on every call; a different
/// path (new game session), a shorter file (truncation/rotation) or an explicit
/// reset discards the previous state so it can never leak across instances or
/// sessions. A chunk without LAN events leaves the state untouched.
struct LanLogCursor {
    path: Option<PathBuf>,
    offset: u64,
    carry: String,
    tracker: LanPortTracker,
    /// Identity/content token for the acknowledged prefix, used to notice a
    /// same-path replacement that is longer than the old offset.
    file_token: String,
}

impl LanLogCursor {
    fn new() -> Self {
        Self {
            path: None,
            offset: 0,
            carry: String::new(),
            tracker: LanPortTracker::new(),
            file_token: String::new(),
        }
    }

    fn reset_session(&mut self) {
        self.path = None;
        self.offset = 0;
        self.carry.clear();
        self.tracker.reset();
        self.file_token.clear();
    }

    fn consume(&mut self, chunk: &[u8]) {
        self.carry.push_str(&String::from_utf8_lossy(chunk));
        let carry = std::mem::take(&mut self.carry);
        let mut consumed = 0usize;
        for line in carry.split_inclusive('\n') {
            if !line.ends_with('\n') {
                break;
            }
            self.tracker
                .observe_line(line.trim_end_matches('\n').trim_end_matches('\r'));
            consumed += line.len();
        }
        let mut rest = carry[consumed..].to_string();
        if rest.len() > HOST_LOG_CARRY_LIMIT {
            let drop = rest.len() - HOST_LOG_CARRY_LIMIT;
            rest.drain(..drop);
        }
        self.carry = rest;
    }

    /// Consume new bytes of `path` and return the currently confirmed LAN
    /// port. Read errors on the same file keep the last known port; a missing
    /// file is treated as no state and the caller decides what to publish.
    fn observe(&mut self, path: &Path) -> Option<u16> {
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
            self.offset = 0;
            self.carry.clear();
            self.tracker.reset();
            self.file_token.clear();
        }

        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(_) => return self.tracker.port(),
        };
        let Ok(len) = file.metadata().map(|metadata| metadata.len()) else {
            return self.tracker.port();
        };
        // Truncation or an in-place rewrite: the previous events no longer
        // describe this file.
        if len < self.offset {
            self.offset = 0;
            self.carry.clear();
            self.tracker.reset();
            self.file_token.clear();
        }
        // A same-path replacement can be longer than the old offset, so a
        // length check alone would miss it. The token covers file identity
        // and the acknowledged prefix.
        if !self.file_token.is_empty() {
            let token = crate::game_logs::compute_file_token(&file, self.offset);
            if token != self.file_token {
                self.offset = 0;
                self.carry.clear();
                self.tracker.reset();
                self.file_token.clear();
            }
        }
        if self.offset >= len {
            return self.tracker.port();
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return self.tracker.port();
        }

        let mut remaining = (len - self.offset).min(HOST_LOG_READ_LIMIT);
        let mut buffer = [0u8; 64 * 1024];
        while remaining > 0 {
            let take = remaining.min(buffer.len() as u64) as usize;
            let Ok(read) = file.read(&mut buffer[..take]) else {
                break;
            };
            if read == 0 {
                break;
            }
            self.offset += read as u64;
            remaining -= read as u64;
            self.consume(&buffer[..read]);
        }
        self.file_token = crate::game_logs::compute_file_token(&file, self.offset);
        self.tracker.port()
    }
}

/// One-shot full-file scan for the explicit "detect world port" action. It
/// reads the whole session log chronologically, so an old LAN line is not lost
/// just because the log has grown past an arbitrary tail limit.
fn detect_lan_port_in_file(path: &Path) -> Option<u16> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut tracker = LanPortTracker::new();
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buffer);
                tracker.observe_line(line.trim_end_matches('\n').trim_end_matches('\r'));
            }
            Err(_) => break,
        }
    }
    tracker.port()
}

/// A host monitor is useful only while the registered game is running or a
/// launch is still in flight. Once the session ends it clears the endpoint and
/// exits instead of polling a dead instance forever.
fn host_monitor_should_continue(
    running: bool,
    phase: Option<crate::launch_state::LaunchPhase>,
) -> bool {
    running
        || matches!(
            phase,
            Some(crate::launch_state::LaunchPhase::Starting)
                | Some(crate::launch_state::LaunchPhase::Preparing)
        )
}

/// Background host-side port monitor. It does not depend on the Rooms page
/// being mounted: while the room is Host and its registered instance is the
/// running game, the latest session log for exactly that instance is scanned
/// incrementally for Open to LAN / Stopping server. A closed LAN world
/// therefore clears the advertised port even if the user is on another page.
async fn host_monitor_loop(
    app: AppHandle,
    room: Arc<RoomStateManager>,
    data_dir: std::path::PathBuf,
    instance: String,
    generation: u64,
) {
    // One cursor per monitor (one registered instance/session). A new
    // instance gets a new monitor via LoopSupervisor, so port state is never
    // carried across instances.
    let lan_cursor = Arc::new(std::sync::Mutex::new(LanLogCursor::new()));
    let mut identity_unknown_checks = 0u32;

    loop {
        if !host_monitor_supervisor().is_current(generation) {
            break;
        }
        let state = app.state::<AppState>();
        let enabled = state
            .config
            .lock()
            .map(|config| config.rooms_enabled)
            .unwrap_or(false);
        let still_host = enabled
            && room.role() == RoomRole::Host
            && room.host_instance().as_deref() == Some(instance.as_str());
        if !still_host {
            break;
        }

        let phase = state.launches.phase(&instance);
        let mut running = phase == Some(crate::launch_state::LaunchPhase::Running);
        if running {
            if let Some(identity) = state.launches.process_identity(&instance) {
                match crate::running_state::identity_liveness(&identity) {
                    crate::running_state::IdentityLiveness::Dead => {
                        if state
                            .launches
                            .clear_running_if_identity(&instance, &identity)
                        {
                            identity_unknown_checks = 0;
                            running = false;
                            events::emit_log(
                                &app,
                                "warn",
                                "rooms",
                                &format!(
                                    "Host game process for '{}' is gone or its pid was reused; clearing the LAN endpoint.",
                                    instance
                                ),
                            );
                        }
                    }
                    crate::running_state::IdentityLiveness::Unknown => {
                        identity_unknown_checks = identity_unknown_checks.saturating_add(1);
                        if identity_unknown_checks >= HOST_IDENTITY_UNKNOWN_LIMIT
                            && state
                                .launches
                                .clear_running_if_identity(&instance, &identity)
                        {
                            identity_unknown_checks = 0;
                            running = false;
                            events::emit_log(
                                &app,
                                "warn",
                                "rooms",
                                &format!(
                                    "Could not verify the host game process for '{}' after repeated attempts; clearing the LAN endpoint.",
                                    instance
                                ),
                            );
                        }
                    }
                    crate::running_state::IdentityLiveness::Alive => {
                        identity_unknown_checks = 0;
                    }
                }
            } else {
                identity_unknown_checks = 0;
            }
        } else {
            identity_unknown_checks = 0;
        }
        let detected = if running {
            let data_dir = data_dir.clone();
            let instance = instance.clone();
            let cursor = lan_cursor.clone();
            tokio::task::spawn_blocking(move || {
                let Some(path) =
                    crate::game_logs::newest_session_for_instance(&data_dir, &instance)
                else {
                    cursor
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .reset_session();
                    return None;
                };
                let mut cursor = cursor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                cursor.observe(&path)
            })
            .await
            .ok()
        } else {
            // The game is not running: there is no world to advertise.
            Some(None)
        };
        // The game can exit while the log read is in flight; never publish a
        // port after it is no longer the registered running instance.
        let Some(detected) = detected else {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            continue;
        };
        let detected =
            if state.launches.phase(&instance) == Some(crate::launch_state::LaunchPhase::Running) {
                detected
            } else {
                None
            };

        if room.snapshot().minecraft_port != detected {
            let _ = room.set_minecraft_port(detected);
            let _ = app.emit("room_status_changed", serde_json::json!({}));
        }

        // Once the registered session is gone (or was never a live game),
        // the endpoint is cleared and this monitor has nothing left to watch.
        // A later launch/registration starts a fresh monitor with a fresh
        // log cursor, so the task cannot spin forever after the game ends.
        if !host_monitor_should_continue(running, phase) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    }
}

/// Start or replace the host port monitor for `instance`.
async fn start_host_monitor(
    app: AppHandle,
    room: Arc<RoomStateManager>,
    data_dir: std::path::PathBuf,
    instance: String,
) {
    host_monitor_supervisor()
        .start(move |generation| {
            tokio::spawn(host_monitor_loop(app, room, data_dir, instance, generation))
        })
        .await;
}

/// Resume the monitor after a restart when the host room already has a
/// registered instance.
async fn ensure_host_monitor(
    app: AppHandle,
    room: Arc<RoomStateManager>,
    data_dir: std::path::PathBuf,
    instance: String,
) {
    host_monitor_supervisor()
        .ensure(move |generation| {
            tokio::spawn(host_monitor_loop(app, room, data_dir, instance, generation))
        })
        .await;
}

async fn stop_host_monitor() {
    host_monitor_supervisor().stop().await;
}

/// Does a previously resolved host still exist as an online peer? Reusing it
/// avoids re-probing every peer on every discovery pass once the host is known.
fn cached_host_still_online(host: &HostInfo, peers: &[PeerInfo]) -> bool {
    host.online
        && peers.iter().any(|peer| {
            peer.online
                && ((!host.node_key.is_empty() && peer.node_key == host.node_key)
                    || host
                        .tailnet_ips
                        .iter()
                        .any(|ip| peer.tailnet_ips.iter().any(|peer_ip| peer_ip == ip)))
        })
}

/// Hard cap for one discovery fan-out. Machine Sharing normally exposes a
/// handful of nodes; a larger tailnet is still bounded per pass.
const MAX_PROBE_PEERS: usize = 16;
/// One peer normally publishes one IPv4 and one IPv6 tailnet address. Cap the
/// fan-out per peer so a malformed status document cannot create unbounded
/// control requests.
const MAX_PROBE_IPS_PER_PEER: usize = 4;

/// Peers worth probing for the room's VoidLink bridge: online, with an
/// address, and explicitly shared with this account (`ShareeNode: true`).
/// Unknown/own-account peers must not receive even the public room id just
/// because they happen to be online. The explicit single-peer fallback still
/// handles older status documents, but a broadcast occurs only for Machine
/// Sharing peers.
fn probe_candidates(peers: &[PeerInfo]) -> Vec<&PeerInfo> {
    peers
        .iter()
        .filter(|p| p.online && !p.tailnet_ips.is_empty() && p.sharee_node == Some(true))
        .take(MAX_PROBE_PEERS)
        .collect()
}

/// Does the verified bridge response belong to the room the guest joined?
fn room_code_matches(status: &VoidLinkStatus, room_code: &str) -> bool {
    !room_code.is_empty() && status.room_id.eq_ignore_ascii_case(room_code)
}

/// A host may only be published after its bridge returned a valid,
/// challenge-bound proof for the room. This applies equally to a hostname
/// hint, a cached endpoint and the single-peer fallback: compatibility must
/// not turn an unrelated peer into "host found".
fn confirmed_host_status(status: Option<&VoidLinkStatus>, room_code: &str) -> bool {
    status.is_some_and(|status| room_code_matches(status, room_code))
}

/// Clear all volatile discovery state. Used when Tailscale is unavailable or
/// when the bridge did not confirm the room; the next successful pass can
/// populate it again.
fn clear_discovery_state(room: &RoomStateManager) {
    let _ = room.set_host(None);
    let _ = room.set_minecraft_port(None);
}

/// Ask eligible tailnet peers which room their VoidLink bridge serves. Each
/// request carries only the public room id, a fresh challenge and an HMAC
/// proving the guest knows the invitation secret - the secret itself never
/// leaves this machine. A peer that only knows the public id cannot forge the
/// host proof.
async fn probe_hosts_for_room(
    peers: &[PeerInfo],
    room_code: &str,
    port: u16,
    room_secret: &str,
) -> Option<(HostInfo, VoidLinkStatus)> {
    let candidates = probe_candidates(peers);
    if candidates.is_empty() {
        return None;
    }
    let mut requests = Vec::with_capacity(candidates.len() * 2);
    for peer in candidates {
        let host = HostInfo::from_peer(peer);
        for ip in crate::rooms::void_link::allowed_void_link_ips(&peer.tailnet_ips)
            .into_iter()
            .take(MAX_PROBE_IPS_PER_PEER)
        {
            let code = room_code.to_string();
            let secret = room_secret.to_string();
            let host = host.clone();
            requests.push(async move {
                let status = crate::rooms::void_link::query_host_status(&ip, port, &code, &secret)
                    .await
                    .ok()?;
                room_code_matches(&status, &code).then_some((host, status))
            });
        }
    }
    futures::future::join_all(requests)
        .await
        .into_iter()
        .flatten()
        .next()
}

/// Confirm a candidate host by querying every valid tailnet address it
/// publishes. The proof is checked against the address that actually answered,
/// so trying an alternate family cannot weaken authentication.
async fn confirm_host_status(
    host: &HostInfo,
    room_code: &str,
    port: u16,
    room_secret: &str,
) -> Option<VoidLinkStatus> {
    for ip in crate::rooms::void_link::allowed_void_link_ips(&host.tailnet_ips)
        .into_iter()
        .take(MAX_PROBE_IPS_PER_PEER)
    {
        if let Ok(status) =
            crate::rooms::void_link::query_host_status(&ip, port, room_code, room_secret).await
        {
            if room_code_matches(&status, room_code) {
                return Some(status);
            }
        }
    }
    None
}

/// One discovery pass (guest): resolve the host from the current tailnet
/// peers, read its advertised Minecraft port over VoidLink, persist both, and
/// return the resulting key tuple for emit-on-change deduplication. A flaky
/// tailnet or a briefly-unreachable host just yields an absent snapshot -
/// never an error the caller must handle.
async fn run_discovery_pass(
    room: &Arc<RoomStateManager>,
    data_dir: &std::path::Path,
) -> DiscoverySnapshot {
    let snapshot = room.snapshot();
    let code = snapshot.room_id.clone().unwrap_or_default();
    // A guest without the invitation secret cannot authenticate any host.
    // Never resume code-only discovery from a legacy/partial state.
    let Some(secret) = snapshot.room_secret.clone() else {
        clear_discovery_state(room);
        return (None, false, None);
    };
    let ts = TailscaleManager::new(data_dir.to_path_buf());
    let status = match tokio::task::spawn_blocking(move || ts.status()).await {
        Ok(Ok(status)) => status,
        _ => {
            clear_discovery_state(room);
            return (None, false, None);
        }
    };
    // A stopped/unauthenticated daemon cannot confirm any endpoint. Keeping
    // the previous host visible here was the source of false "world is open"
    // states after a Tailscale disconnect.
    if !TailscaleManager::is_logged_in(&status) {
        clear_discovery_state(room);
        return (None, false, None);
    }
    let peers = crate::rooms::peer_discovery::peers_from_status(&status);
    // 1. The host found by the previous pass is reused while it is still an
    //    online peer, so a multi-peer room does not re-probe the whole tailnet
    //    every two seconds.
    // 2. A hostname hint from a room created by an older build, or a single
    //    shared peer, is only a candidate.
    // 3. Otherwise ask each eligible peer's bridge which room it serves.
    let cached = snapshot
        .host
        .clone()
        .filter(|host| cached_host_still_online(host, &peers));
    let mut probed_status = None;
    let host = match cached {
        Some(host) => Some(host),
        None => match resolve_host_for_room(&peers, snapshot.room_id.as_deref()) {
            Some(host) => Some(host),
            None if !code.is_empty() => {
                probe_hosts_for_room(&peers, &code, snapshot.void_link_port, &secret)
                    .await
                    .map(|(host, status)| {
                        probed_status = Some(status);
                        host
                    })
            }
            None => None,
        },
    };
    let Some(host) = host else {
        clear_discovery_state(room);
        return (None, false, None);
    };
    let confirmed = match probed_status {
        Some(status) => Some(status),
        None if !code.is_empty() => {
            confirm_host_status(&host, &code, snapshot.void_link_port, &secret).await
        }
        None => None,
    };
    if !confirmed_host_status(confirmed.as_ref(), &code) {
        clear_discovery_state(room);
        return (None, false, None);
    }
    let mc_port = confirmed.and_then(|status| status.mc_port);
    let _ = room.set_host(Some(host.clone()));
    let _ = room.set_minecraft_port(mc_port);
    (Some(host.node_key.clone()), host.online, mc_port)
}

/// Open the AuthURL in the system browser once it appears after an explicit
/// "Sign in" click. Never re-runs `tailscale up`: repeated detached login
/// processes were a hidden side effect and could leave orphans behind.
fn open_login_url_when_ready(app: AppHandle, ts: TailscaleManager) {
    tokio::spawn(async move {
        use tauri_plugin_opener::OpenerExt;
        let mut opened = false;
        for _ in 0..40 {
            if TAILSCALE_LOGIN.is_cancelled() {
                break;
            }
            let enabled = app
                .state::<AppState>()
                .config
                .lock()
                .map(|config| config.rooms_enabled)
                .unwrap_or(false);
            if !enabled {
                TAILSCALE_LOGIN.cancel();
                break;
            }
            let status = {
                let ts = ts.clone();
                match tokio::task::spawn_blocking(move || ts.status()).await {
                    Ok(Ok(status)) => Some(status),
                    _ => None,
                }
            };
            if let Some(status) = status {
                if TailscaleManager::is_logged_in(&status) {
                    events::emit_room_progress("login", 1.0, "Logged in");
                    opened = true;
                    break;
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
                                "Could not open the Tailscale login URL automatically - use the Open login button.",
                            );
                        }
                        opened = true;
                        break;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        }
        if !opened && !TAILSCALE_LOGIN.is_cancelled() {
            events::emit_log(
                &app,
                "warn",
                "rooms",
                "Tailscale login URL has not appeared yet - click 'Refresh status' again.",
            );
        }
        TAILSCALE_LOGIN.finish();
    });
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
    fn public_status_exposes_host_instance_but_never_the_invitation_secret() {
        let row = RoomStateRow {
            role: RoomRole::Host,
            room_id: Some("ABCD-1234".into()),
            room_name: Some("QA room".into()),
            host_instance: Some("Pack".into()),
            minecraft_port: Some(45565),
            room_secret: Some(crate::rooms::room_state::generate_room_secret()),
            ..Default::default()
        };
        let status = build_status_from_row(&row, false, None);
        assert_eq!(status.room_id.as_deref(), Some("ABCD-1234"));
        assert_eq!(status.host_instance.as_deref(), Some("Pack"));
        assert_eq!(status.minecraft_port, Some(45565));

        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["hostInstance"], "Pack");
        assert!(json.get("roomSecret").is_none());
        assert!(json.get("room_secret").is_none());
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
        assert!(display_host_for(
            &room,
            Some(&TailscaleStatus {
                version: None,
                backend_state: None,
                auth_url: None,
                current_tailnet: None,
                self_node: None,
                peer: None,
                user: None,
                magic_dns_suffix: None,
            })
        )
        .is_none());
    }

    fn peer(online: bool, sharee: Option<bool>, ips: &[&str]) -> PeerInfo {
        PeerInfo {
            node_key: "node".into(),
            host_name: "host".into(),
            dns_name: "host.tail.ts.net".into(),
            tailnet_ips: ips.iter().map(|ip| ip.to_string()).collect(),
            online,
            last_seen: None,
            sharee_node: sharee,
        }
    }

    fn invite(room_id: &str) -> String {
        crate::rooms::room_state::format_invitation(
            room_id,
            &crate::rooms::room_state::generate_room_secret(),
        )
    }

    fn status_for(room_id: &str) -> VoidLinkStatus {
        VoidLinkStatus {
            room_id: room_id.into(),
            mc_port: Some(45565),
            host_ip: "100.64.0.9".into(),
            challenge: "a".repeat(32),
            proof: "b".repeat(64),
        }
    }

    #[test]
    fn port_zero_is_never_advertised() {
        assert!(valid_minecraft_port(None));
        assert!(valid_minecraft_port(Some(1)));
        assert!(valid_minecraft_port(Some(65535)));
        assert!(!valid_minecraft_port(Some(0)));
    }

    #[test]
    fn probe_candidates_skip_offline_own_and_addressless_peers() {
        let peers = vec![
            peer(true, Some(false), &["100.64.0.7"]), // guest's own device
            peer(false, Some(true), &["100.64.0.8"]), // offline
            peer(true, Some(true), &[]),              // no address
            peer(true, Some(true), &["100.64.0.9"]),  // eligible shared host
            peer(true, None, &["100.64.0.10"]),       // unknown flag: no code broadcast
        ];
        let candidates = probe_candidates(&peers);
        assert_eq!(
            candidates
                .iter()
                .map(|p| p.tailnet_ips[0].as_str())
                .collect::<Vec<_>>(),
            vec!["100.64.0.9"]
        );
    }

    #[test]
    fn probe_candidates_are_capped() {
        let ips: Vec<String> = (0..20).map(|i| format!("100.64.1.{i}")).collect();
        let peers: Vec<PeerInfo> = ips
            .iter()
            .map(|ip| peer(true, Some(true), &[ip.as_str()]))
            .collect();
        assert_eq!(probe_candidates(&peers).len(), MAX_PROBE_PEERS);
    }

    #[test]
    fn a_resolved_host_is_reused_only_while_it_stays_online() {
        let host = HostInfo {
            node_key: "node-key-1".into(),
            host_name: "gaming-pc".into(),
            dns_name: "gaming-pc.tail.ts.net".into(),
            tailnet_ips: vec!["100.64.0.9".into()],
            online: true,
        };
        let mut online = peer(true, Some(true), &["100.64.0.9"]);
        online.node_key = "node-key-1".into();
        assert!(cached_host_still_online(&host, &[online.clone()]));

        // Same node, changed address: still the same machine.
        let mut moved = peer(true, Some(true), &["100.64.0.42"]);
        moved.node_key = "node-key-1".into();
        assert!(cached_host_still_online(&host, &[moved]));

        // Offline peers never keep a room connected.
        let mut offline = online.clone();
        offline.online = false;
        assert!(!cached_host_still_online(&host, &[offline]));

        // A completely different machine is not the cached host.
        assert!(!cached_host_still_online(
            &host,
            &[peer(true, Some(true), &["100.64.0.77"])]
        ));

        let mut offline_host = host;
        offline_host.online = false;
        assert!(!cached_host_still_online(&offline_host, &[online]));
    }

    #[test]
    fn bridge_answers_only_for_the_joined_room_code() {
        let status = status_for("ABCD-1234");
        assert!(room_code_matches(&status, "ABCD-1234"));
        assert!(room_code_matches(&status, "abcd-1234"));
        assert!(!room_code_matches(&status, "WXYZ-9012"));
        assert!(!room_code_matches(&status, ""));
        assert!(confirmed_host_status(Some(&status), "ABCD-1234"));
        assert!(!confirmed_host_status(Some(&status), "WXYZ-9012"));
        assert!(!confirmed_host_status(None, "ABCD-1234"));
    }

    #[test]
    fn single_peer_fallback_is_not_published_without_a_matching_code() {
        let peers = vec![peer(true, Some(true), &["100.64.0.9"])];
        let candidate = resolve_host_for_room(&peers, Some("ABCD-1234"));
        assert!(candidate.is_some(), "single peer is a candidate");
        let wrong = status_for("WXYZ-9012");
        assert!(
            !confirmed_host_status(Some(&wrong), "ABCD-1234"),
            "an unrelated bridge must not be published"
        );
        let right = status_for("abcd-1234");
        assert!(confirmed_host_status(Some(&right), "ABCD-1234"));
    }

    #[test]
    fn restart_resume_requires_a_confirmed_matching_game() {
        use crate::running_state::{LiveScan, RunningGame};

        let mut scan = LiveScan::default();
        scan.unverified.push(RunningGame {
            instance: "HostPack".into(),
            pid: 77,
            started_at: "2026-01-01T12:00:00+00:00".into(),
            exe: None,
            created: None,
        });
        assert!(
            confirmed_running_game(&scan, "HostPack").is_none(),
            "an unverifiable pid record must never resume the monitor"
        );

        scan.confirmed.push(RunningGame {
            instance: "hostpack".into(),
            pid: 78,
            started_at: "2026-01-01T12:00:00+00:00".into(),
            exe: Some("C:\\Java\\bin\\javaw.exe".into()),
            created: Some("133000000000000000".into()),
        });
        assert!(
            confirmed_running_game(&scan, "HostPack").is_some(),
            "Windows instance names are case-insensitive"
        );
        assert!(
            confirmed_running_game(&scan, "OtherPack").is_none(),
            "another confirmed game must not own this room"
        );
    }

    #[test]
    fn restart_without_a_live_game_clears_the_stale_room_endpoint() {
        use crate::running_state::LiveScan;

        let dir = std::env::temp_dir().join(format!(
            "vl_room_resume_clear_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let room = RoomStateManager::new(&dir);
        room.create_room(None).unwrap();
        room.set_host_instance(Some("HostPack".into())).unwrap();
        room.set_minecraft_port(Some(45565)).unwrap();

        let scan = LiveScan::default();
        assert!(confirmed_running_game(&scan, "HostPack").is_none());
        clear_stale_host_resume(&room);

        let snapshot = room.snapshot();
        assert_eq!(snapshot.minecraft_port, None);
        assert_eq!(snapshot.host_instance, None);
        assert_eq!(snapshot.role, RoomRole::Host, "the room itself stays host");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discovery_failure_clears_a_previously_confirmed_host() {
        let dir = std::env::temp_dir().join(format!(
            "vl_room_clear_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let room = RoomStateManager::new(&dir);
        room.join_room(&invite("ABCD-1234")).unwrap();
        room.set_host(Some(HostInfo {
            node_key: "node-1".into(),
            host_name: "gaming-pc".into(),
            dns_name: "gaming-pc.tail.ts.net".into(),
            tailnet_ips: vec!["100.64.0.9".into()],
            online: true,
        }))
        .unwrap();
        room.set_minecraft_port(Some(45565)).unwrap();

        clear_discovery_state(&room);
        let snapshot = room.snapshot();
        assert!(snapshot.host.is_none());
        assert_eq!(snapshot.minecraft_port, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn login_gate_rejects_a_second_flow_and_recovers_after_finish() {
        let gate = LoginGate::new();
        assert!(gate.try_begin());
        assert!(!gate.try_begin(), "a concurrent login must be rejected");
        gate.finish();
        assert!(gate.try_begin(), "a finished flow must be retryable");
        gate.cancel();
        assert!(gate.is_cancelled());
        gate.finish();
    }

    struct RunningGuard(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for RunningGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_start_and_ensure_keep_only_one_supervisor_loop() {
        let supervisor = Arc::new(LoopSupervisor::new());
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let running = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        fn make_spawn(
            started: Arc<std::sync::atomic::AtomicUsize>,
            running: Arc<std::sync::atomic::AtomicUsize>,
        ) -> impl FnOnce(u64) -> tokio::task::JoinHandle<()> {
            move |_generation| {
                started.fetch_add(1, Ordering::SeqCst);
                let running = running.clone();
                tokio::spawn(async move {
                    running.fetch_add(1, Ordering::SeqCst);
                    let _guard = RunningGuard(running);
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                })
            }
        }

        let a = supervisor.start(make_spawn(started.clone(), running.clone()));
        let b = supervisor.ensure(make_spawn(started.clone(), running.clone()));
        let (_, _) = tokio::join!(a, b);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(
            supervisor.live(),
            "the supervisor must retain one live loop"
        );
        assert_eq!(
            running.load(Ordering::SeqCst),
            1,
            "old loops must be cancelled when start/ensure race"
        );
        assert!(
            started.load(Ordering::SeqCst) <= 2,
            "at most one replacement is expected"
        );

        supervisor.stop().await;
        assert!(!supervisor.live());
    }

    #[test]
    fn host_monitor_stops_after_the_registered_game_session_ends() {
        use crate::launch_state::LaunchPhase;

        assert!(host_monitor_should_continue(
            true,
            Some(LaunchPhase::Running)
        ));
        assert!(
            host_monitor_should_continue(false, Some(LaunchPhase::Starting)),
            "a launch that is still preparing must keep the monitor alive"
        );
        assert!(host_monitor_should_continue(
            false,
            Some(LaunchPhase::Preparing)
        ));
        assert!(
            !host_monitor_should_continue(false, Some(LaunchPhase::Running)),
            "a dead identity clears the endpoint and the monitor must exit"
        );
        assert!(!host_monitor_should_continue(false, None));
    }

    #[test]
    fn host_lan_cursor_keeps_the_port_after_more_than_5000_lines() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!(
            "vl_room_lan_cursor_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("HostPack_20260101_120000.log");
        std::fs::write(&path, "[..] Local game hosted on port 45565\n").unwrap();

        let mut cursor = LanLogCursor::new();
        assert_eq!(cursor.observe(&path), Some(45565));

        // Reproduce the original defect: the LAN line is now older than any
        // fixed tail window. The cursor must keep the confirmed port because
        // no stop event was written.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        for index in 0..6_000 {
            writeln!(file, "[..] [Render thread/INFO]: unrelated line {index}").unwrap();
        }
        assert_eq!(
            cursor.observe(&path),
            Some(45565),
            "unrelated lines must not clear a valid LAN port"
        );

        writeln!(file, "[..] [Server thread/INFO]: Stopping server").unwrap();
        assert_eq!(cursor.observe(&path), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn host_lan_cursor_handles_partial_lines_and_reopening() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!(
            "vl_room_lan_partial_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("HostPack_20260101_120000.log");
        std::fs::write(&path, "[..] Local game hosted on port 4").unwrap();

        let mut cursor = LanLogCursor::new();
        assert_eq!(cursor.observe(&path), None, "a split line is not an event");

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "5565").unwrap();
        assert_eq!(cursor.observe(&path), Some(45565));

        writeln!(file, "[..] [Server thread/INFO]: Stopping server").unwrap();
        assert_eq!(cursor.observe(&path), None);
        writeln!(file, "[..] Local game hosted on port 22222").unwrap();
        assert_eq!(cursor.observe(&path), Some(22222));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn host_lan_cursor_resets_on_truncation_and_new_session() {
        let dir = std::env::temp_dir().join(format!(
            "vl_room_lan_reset_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("HostPack_20260101_120000.log");
        let second = dir.join("HostPack_20260102_120000.log");
        let mut first_contents = String::from("[..] Local game hosted on port 11111\n");
        for _ in 0..100 {
            first_contents.push_str("[..] unrelated filler\n");
        }
        std::fs::write(&first, first_contents).unwrap();

        let mut cursor = LanLogCursor::new();
        assert_eq!(cursor.observe(&first), Some(11111));

        // A different newest session must not inherit the previous state.
        std::fs::write(&second, "[..] Done (2.1s)! For help, type \"help\"\n").unwrap();
        assert_eq!(cursor.observe(&second), None);
        // The new session starts from its own beginning, not from the old
        // session's byte offset: the LAN line added after a long filler must
        // still be discovered.
        let mut second_contents = format!("{}\n", "x".repeat(200));
        second_contents.push_str("[..] Local game hosted on port 22222\n");
        std::fs::write(&second, second_contents).unwrap();
        assert_eq!(cursor.observe(&second), Some(22222));

        // Truncation/rewrite of the same path resets the cursor.
        std::fs::write(&second, "[..] Local game hosted on port 33333\n").unwrap();
        assert_eq!(cursor.observe(&second), Some(33333));

        cursor.reset_session();
        assert_eq!(
            cursor.observe(&second),
            Some(33333),
            "a fresh cursor rescans from the beginning"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn host_lan_cursor_detects_a_same_path_replacement_longer_than_the_old_offset() {
        let dir = std::env::temp_dir().join(format!(
            "vl_room_lan_replace_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("HostPack_20260101_120000.log");
        let mut original = String::from("[..] Local game hosted on port 11111\n");
        for _ in 0..100 {
            original.push_str("[..] original filler\n");
        }
        std::fs::write(&path, original).unwrap();

        let mut cursor = LanLogCursor::new();
        assert_eq!(cursor.observe(&path), Some(11111));

        // The replacement is longer than the old offset and puts the new LAN
        // event at the very beginning. A plain seek would skip it and keep the
        // old port; the file token forces a rescan.
        let mut replacement = String::from("[..] Local game hosted on port 22222\n");
        replacement.push_str(&"y".repeat(8_000));
        replacement.push('\n');
        std::fs::write(&path, replacement).unwrap();
        assert_eq!(cursor.observe(&path), Some(22222));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manual_detection_reads_a_long_log_from_the_beginning() {
        let dir = std::env::temp_dir().join(format!(
            "vl_room_lan_manual_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("HostPack_20260101_120000.log");
        let mut contents = String::from("[..] Local game hosted on port 45565\n");
        for index in 0..6_000 {
            contents.push_str(&format!(
                "[..] [Render thread/INFO]: unrelated line {index}\n"
            ));
        }
        std::fs::write(&path, contents).unwrap();

        assert_eq!(detect_lan_port_in_file(&path), Some(45565));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn host_lan_monitor_reads_only_the_registered_instances_log() {
        let dir = std::env::temp_dir().join(format!(
            "vl_room_lan_scope_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let logs = dir.join("logs").join("game");
        std::fs::create_dir_all(&logs).unwrap();
        let host = logs.join("HostPack_20260101_120000.log");
        let other = logs.join("OtherPack_20260101_120000.log");
        std::fs::write(&host, "[..] Local game hosted on port 45565\n").unwrap();
        std::fs::write(&other, "[..] Local game hosted on port 55555\n").unwrap();
        // Make the unrelated session newer: instance scoping must ignore it.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&other, "[..] Local game hosted on port 55556\n").unwrap();

        let selected = crate::game_logs::newest_session_for_instance(&dir, "HostPack");
        assert_eq!(selected.as_deref(), Some(host.as_path()));
        let mut cursor = LanLogCursor::new();
        assert_eq!(cursor.observe(selected.as_deref().unwrap()), Some(45565));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_monitor_ensure_starts_only_one_loop() {
        let supervisor = Arc::new(LoopSupervisor::new());
        let running = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        fn spawn_monitor(
            running: Arc<std::sync::atomic::AtomicUsize>,
        ) -> impl FnOnce(u64) -> tokio::task::JoinHandle<()> {
            move |_generation| {
                let running = running.clone();
                tokio::spawn(async move {
                    running.fetch_add(1, Ordering::SeqCst);
                    let _guard = RunningGuard(running);
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                })
            }
        }

        assert!(
            supervisor.ensure(spawn_monitor(running.clone())).await,
            "the first resume starts the monitor"
        );
        assert!(
            !supervisor.ensure(spawn_monitor(running.clone())).await,
            "a repeated resume must not duplicate the monitor"
        );
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(
            running.load(Ordering::SeqCst),
            1,
            "exactly one host monitor task may exist"
        );

        supervisor.stop().await;
        assert!(!supervisor.live());
    }
}

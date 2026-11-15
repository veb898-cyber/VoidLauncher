//! Server list commands: read and edit the instance's servers.dat.

use crate::launch_state::{LaunchBusy, LaunchPhase, LaunchToken};
use crate::running_state::IdentityProbe;
use crate::servers::{self, ServerEntry};
use crate::AppState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

/// Structured write error. The renderer maps `kind` to a localized message
/// instead of parsing backend prose.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerCommandError {
    /// The tracked Minecraft for this instance is running.
    InstanceRunning,
    /// A launch is still being prepared; the process may appear at any moment.
    InstanceStarting,
    /// Another launcher-side server-list edit already holds the instance.
    AnotherEdit,
    /// `servers.dat` changed after it was read and before it was replaced.
    Conflict,
    /// A normal validation/IO error. Kept separate so the renderer can still
    /// show the technical detail without interpreting every message.
    Message { message: String },
}

impl From<String> for ServerCommandError {
    fn from(message: String) -> Self {
        Self::Message { message }
    }
}

impl From<crate::error::LauncherError> for ServerCommandError {
    fn from(error: crate::error::LauncherError) -> Self {
        match error {
            crate::error::LauncherError::Conflict(message) => {
                tracing::info!(target: "launcher", "servers.dat conflict: {}", message);
                Self::Conflict
            }
            other => Self::Message {
                message: other.to_string(),
            },
        }
    }
}

fn servers_file(
    state: &State<'_, AppState>,
    instance_name: &str,
) -> Result<std::path::PathBuf, String> {
    super::instances::validate_instance_name(instance_name)?;
    let instances_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.instances_dir()
    };
    servers::servers_file(&instances_dir, instance_name).map_err(|e| e.to_string())
}

fn write_context(
    state: &State<'_, AppState>,
    instance_name: &str,
) -> Result<(PathBuf, PathBuf), ServerCommandError> {
    super::instances::validate_instance_name(instance_name)?;
    let (data_dir, instances_dir) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        (config.data_dir.clone(), config.instances_dir())
    };
    let path = servers::servers_file(&instances_dir, instance_name).map_err(|e| {
        ServerCommandError::Message {
            message: e.to_string(),
        }
    })?;
    Ok((data_dir, path))
}

/// A tracked game with this identity is definitely gone. `Unavailable` is
/// deliberately NOT treated as gone: a permission error must never let a
/// server edit through while the game may still be running.
fn identity_is_dead(identity: &crate::running_state::ProcessIdentity) -> bool {
    match crate::running_state::read_identity(identity.pid) {
        IdentityProbe::Gone => true,
        IdentityProbe::Found(live) => live.exe != identity.exe || live.created != identity.created,
        IdentityProbe::Unavailable => false,
    }
}

/// Reserve the instance for the whole read-modify-write operation.
///
/// This is the backend-side lock, not merely a disabled button. It also
/// prevents a launch from starting in the tiny window between the running
/// check and the atomic file replacement.
struct ServerEditGuard<'a> {
    state: &'a AppState,
    instance_name: String,
    token: LaunchToken,
}

impl Drop for ServerEditGuard<'_> {
    fn drop(&mut self) {
        self.state.launches.release(&self.instance_name, self.token);
    }
}

fn begin_server_edit<'a>(
    state: &'a AppState,
    data_dir: &Path,
    instance_name: &str,
    game_dir: &Path,
) -> Result<ServerEditGuard<'a>, ServerCommandError> {
    begin_server_edit_with(
        state,
        data_dir,
        instance_name,
        game_dir,
        &crate::external_minecraft::SystemExternalGameProbe,
    )
}

fn begin_server_edit_with<'a>(
    state: &'a AppState,
    data_dir: &Path,
    instance_name: &str,
    game_dir: &Path,
    external: &dyn crate::external_minecraft::ExternalGameProbe,
) -> Result<ServerEditGuard<'a>, ServerCommandError> {
    // A Running entry with a proven-dead identity is stale. Clear it so a
    // crashed exit watcher does not block edits forever. An unreadable
    // identity is treated as live and blocks (fail closed).
    if state.launches.phase(instance_name) == Some(LaunchPhase::Running) {
        if let Some(identity) = state.launches.process_identity(instance_name) {
            if identity_is_dead(&identity) {
                state.launches.clear_running(instance_name);
            }
        }
    }

    // An orphaned game from a previous launcher run has no in-memory entry.
    // Adopt only records whose process identity is confirmed by the OS.
    let scan = crate::running_state::scan_games(data_dir);

    // A live record whose identity cannot be confirmed may still be this
    // instance's game (legacy record, access denied). Fail closed here too:
    // launching and instance edits already treat it as running.
    if scan
        .unverified
        .iter()
        .any(|game| game.instance == instance_name)
    {
        return Err(ServerCommandError::InstanceRunning);
    }

    for game in &scan.confirmed {
        state
            .launches
            .adopt_running(&game.instance, game.identity());
    }

    let token = match state.launches.begin_edit(instance_name) {
        Ok(token) => token,
        Err(LaunchBusy::Running) => return Err(ServerCommandError::InstanceRunning),
        Err(LaunchBusy::Starting) => return Err(ServerCommandError::InstanceStarting),
        Err(LaunchBusy::Editing) => return Err(ServerCommandError::AnotherEdit),
        Err(LaunchBusy::Preparing) => {
            return Err(ServerCommandError::Message {
                message: format!(
                    "Instance '{}' is still being installed or prepared. Wait for setup to finish before changing its server list.",
                    instance_name
                ),
            })
        }
    };
    let guard = ServerEditGuard {
        state,
        instance_name: instance_name.to_string(),
        token,
    };

    // A game started outside VoidLauncher has no pid record. Match its exact
    // --gameDir (or, when absent, its working directory) against this
    // instance. The edit reservation is already held, so a launcher-side
    // launch cannot start while this best-effort scan runs.
    if let Some(game) = external.find_for_game_dir(game_dir) {
        tracing::info!(
            target: "launcher",
            "Blocking server edit for '{}': external Minecraft PID {} uses {:?}",
            instance_name,
            game.identity.pid,
            game.game_dir
        );
        drop(guard);
        return Err(ServerCommandError::InstanceRunning);
    }

    Ok(guard)
}

#[tauri::command]
pub async fn cmd_list_servers(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<Vec<ServerEntry>, String> {
    let path = servers_file(&state, &instance_name)?;
    tauri::async_runtime::spawn_blocking(move || servers::list(&path).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cmd_add_server(
    state: State<'_, AppState>,
    instance_name: String,
    name: String,
    address: String,
) -> Result<Vec<ServerEntry>, ServerCommandError> {
    let (data_dir, path) = write_context(&state, &instance_name)?;
    let game_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let _guard = begin_server_edit(state.inner(), &data_dir, &instance_name, &game_dir)?;
    tauri::async_runtime::spawn_blocking(move || {
        servers::add(&path, &name, &address).map_err(ServerCommandError::from)
    })
    .await
    .map_err(|e| ServerCommandError::Message {
        message: e.to_string(),
    })?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_update_server(
    state: State<'_, AppState>,
    instance_name: String,
    index: usize,
    name: String,
    address: String,
    expected_name: Option<String>,
    expected_ip: Option<String>,
    expected_fingerprint: Option<String>,
) -> Result<Vec<ServerEntry>, ServerCommandError> {
    let (data_dir, path) = write_context(&state, &instance_name)?;
    let game_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let _guard = begin_server_edit(state.inner(), &data_dir, &instance_name, &game_dir)?;
    tauri::async_runtime::spawn_blocking(move || {
        match (expected_fingerprint, expected_name, expected_ip) {
            (Some(expected_fingerprint), _, _) => {
                servers::update_fingerprint(&path, index, &expected_fingerprint, &name, &address)
            }
            (None, Some(expected_name), Some(expected_ip)) => {
                servers::update_checked(&path, index, &expected_name, &expected_ip, &name, &address)
            }
            // No row identity: refuse instead of modifying/deleting an index
            // that may have shifted since the caller read the list.
            _ => {
                return Err(ServerCommandError::Conflict);
            }
        }
        .map_err(ServerCommandError::from)
    })
    .await
    .map_err(|e| ServerCommandError::Message {
        message: e.to_string(),
    })?
}

#[tauri::command]
pub async fn cmd_delete_server(
    state: State<'_, AppState>,
    instance_name: String,
    index: usize,
    expected_name: Option<String>,
    expected_ip: Option<String>,
    expected_fingerprint: Option<String>,
) -> Result<Vec<ServerEntry>, ServerCommandError> {
    let (data_dir, path) = write_context(&state, &instance_name)?;
    let game_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let _guard = begin_server_edit(state.inner(), &data_dir, &instance_name, &game_dir)?;
    tauri::async_runtime::spawn_blocking(move || {
        match (expected_fingerprint, expected_name, expected_ip) {
            (Some(expected_fingerprint), _, _) => {
                servers::delete_fingerprint(&path, index, &expected_fingerprint)
            }
            (None, Some(expected_name), Some(expected_ip)) => {
                servers::delete_checked(&path, index, &expected_name, &expected_ip)
            }
            _ => {
                return Err(ServerCommandError::Conflict);
            }
        }
        .map_err(ServerCommandError::from)
    })
    .await
    .map_err(|e| ServerCommandError::Message {
        message: e.to_string(),
    })?
}

fn address_is_saved(path: &Path, address: &str) -> Result<bool, String> {
    servers::list(path)
        .map(|list| list.iter().any(|server| server.ip.trim() == address))
        .map_err(|e| e.to_string())
}

/// Status check (Server List Ping) for one saved server.
///
/// Reading and pinging never take the edit guard: they are safe while the
/// game is running. The address must be present in the instance's saved
/// server list, so this command cannot become an arbitrary host:port probe.
#[tauri::command]
pub async fn cmd_ping_server(
    state: State<'_, AppState>,
    instance_name: String,
    address: String,
) -> Result<crate::server_ping::PingResult, String> {
    let path = servers_file(&state, &instance_name)?;
    let wanted = address.trim().to_string();
    let saved = tauri::async_runtime::spawn_blocking({
        let wanted = wanted.clone();
        move || address_is_saved(&path, &wanted)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !saved {
        return Err("address is not in this instance's server list".to_string());
    }
    let (host, port) = crate::server_ping::parse_host_port(&wanted)?;
    Ok(crate::server_ping::ping_server(&host, port).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_minecraft::{ExternalGame, ExternalGameProbe};
    use crate::launch_state::{LaunchPhase, LaunchRegistry};
    use crate::running_state::{IdentityProbe, ProcessIdentity};

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vl-servers-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn test_state(data_dir: &Path) -> AppState {
        let config = crate::AppConfig {
            data_dir: data_dir.to_path_buf(),
            ..Default::default()
        };
        AppState {
            config: std::sync::Mutex::new(config),
            auth_state: std::sync::Mutex::new(crate::auth::AuthState::default()),
            launches: LaunchRegistry::new(),
            pack_watcher: std::sync::Mutex::new(None),
            active_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            room_state: std::sync::Arc::new(crate::rooms::room_state::RoomStateManager::new(
                data_dir,
            )),
            tailscale: crate::rooms::tailscale::TailscaleManager::new(data_dir.to_path_buf()),
        }
    }

    fn running(state: &AppState, instance: &str) {
        let token = state.launches.begin_launch(instance).unwrap();
        assert!(state.launches.mark_running(instance, token, None));
    }

    fn edit_error(state: &AppState, data_dir: &Path, instance: &str) -> ServerCommandError {
        match begin_server_edit(state, data_dir, instance, data_dir) {
            Ok(_) => panic!("expected server editing to be blocked"),
            Err(error) => error,
        }
    }

    #[test]
    fn running_instance_blocks_server_edits() {
        let dir = temp_dir("running");
        let state = test_state(&dir);
        running(&state, "Pack");

        let error = edit_error(&state, &dir, "Pack");
        assert!(
            matches!(error, ServerCommandError::InstanceRunning),
            "expected InstanceRunning, got {error:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn starting_instance_blocks_server_edits() {
        let dir = temp_dir("starting");
        let state = test_state(&dir);
        let _token = state.launches.begin_launch("Pack").unwrap();

        let error = edit_error(&state, &dir, "Pack");
        assert!(matches!(error, ServerCommandError::InstanceStarting));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_instance_stays_editable() {
        let dir = temp_dir("other");
        let state = test_state(&dir);
        running(&state, "Other");

        let guard = begin_server_edit(&state, &dir, "Pack", &dir).expect("Pack is not running");
        assert_eq!(state.launches.phase("Pack"), Some(LaunchPhase::Editing));
        assert_eq!(state.launches.phase("Other"), Some(LaunchPhase::Running));
        drop(guard);
        assert_eq!(state.launches.phase("Pack"), None);
        assert_eq!(state.launches.phase("Other"), Some(LaunchPhase::Running));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editing_is_allowed_again_after_the_game_is_gone() {
        let dir = temp_dir("finished");
        let state = test_state(&dir);
        running(&state, "Pack");
        assert!(matches!(
            begin_server_edit(&state, &dir, "Pack", &dir),
            Err(ServerCommandError::InstanceRunning)
        ));

        state.launches.clear_running("Pack");
        let guard =
            begin_server_edit(&state, &dir, "Pack", &dir).expect("editing allowed after exit");
        drop(guard);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_persisted_confirmed_game_is_adopted_and_blocks_edits() {
        let dir = temp_dir("persisted");
        let state = test_state(&dir);
        let IdentityProbe::Found(identity) =
            crate::running_state::read_identity(std::process::id())
        else {
            eprintln!("skipping: current process identity is unavailable");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        crate::running_state::write_pid_file(&dir, "Pack", std::process::id(), Some(&identity))
            .unwrap();

        let error = edit_error(&state, &dir, "Pack");
        assert!(matches!(error, ServerCommandError::InstanceRunning));
        assert_eq!(state.launches.phase("Pack"), Some(LaunchPhase::Running));

        crate::running_state::remove_pid_file(&dir, "Pack");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unverified_live_record_blocks_server_edits() {
        struct NoExternal;
        impl ExternalGameProbe for NoExternal {
            fn find_for_game_dir(&self, _: &Path) -> Option<ExternalGame> {
                None
            }
        }

        let dir = temp_dir("unverified");
        let state = test_state(&dir);
        crate::running_state::write_pid_file(&dir, "Pack", std::process::id(), None)
            .expect("legacy record written");

        let error = match begin_server_edit_with(&state, &dir, "Pack", &dir, &NoExternal) {
            Ok(_) => panic!("an unverified live record must block edits"),
            Err(error) => error,
        };
        assert!(
            matches!(error, ServerCommandError::InstanceRunning),
            "expected InstanceRunning, got {error:?}"
        );

        crate::running_state::remove_pid_file(&dir, "Pack");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reading_and_ping_precheck_are_allowed_while_running() {
        let dir = temp_dir("read");
        let state = test_state(&dir);
        let path = dir.join("servers.dat");
        servers::add(&path, "A", "a.example.com").unwrap();
        running(&state, "Pack");

        let listed = servers::list(&path).expect("reading must not be blocked");
        assert_eq!(listed.len(), 1);
        assert!(address_is_saved(&path, "a.example.com").unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn stale_running_entry_with_a_dead_identity_is_not_a_permanent_lock() {
        let dir = temp_dir("stale");
        let state = test_state(&dir);
        let token = state.launches.begin_launch("Pack").unwrap();
        let dead = ProcessIdentity {
            pid: u32::MAX,
            exe: r"c:\java\bin\java.exe".to_string(),
            created: 1,
        };
        assert!(state.launches.mark_running("Pack", token, Some(dead)));

        let guard = begin_server_edit(&state, &dir, "Pack", &dir)
            .expect("a dead recorded process must not block forever");
        drop(guard);

        let _ = std::fs::remove_dir_all(&dir);
    }

    struct FakeExternal {
        game_dir: PathBuf,
        identity: ProcessIdentity,
    }

    impl ExternalGameProbe for FakeExternal {
        fn find_for_game_dir(&self, game_dir: &Path) -> Option<ExternalGame> {
            (crate::external_minecraft::normalize_game_dir(game_dir)
                == crate::external_minecraft::normalize_game_dir(&self.game_dir))
            .then(|| ExternalGame {
                game_dir: self.game_dir.clone(),
                identity: self.identity.clone(),
            })
        }
    }

    #[test]
    fn external_minecraft_with_the_matching_game_dir_blocks_edits() {
        let dir = temp_dir("external");
        let pack_dir = dir.join("Pack").join(".minecraft");
        let other_dir = dir.join("Other").join(".minecraft");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::create_dir_all(&other_dir).unwrap();
        let state = test_state(&dir);
        let probe = FakeExternal {
            game_dir: pack_dir.clone(),
            identity: ProcessIdentity {
                pid: 4242,
                exe: r"c:\java\bin\java.exe".to_string(),
                created: 1,
            },
        };

        let error = match begin_server_edit_with(&state, &dir, "Pack", &pack_dir, &probe) {
            Ok(_) => panic!("external game must block the matching instance"),
            Err(error) => error,
        };
        assert!(matches!(error, ServerCommandError::InstanceRunning));

        let guard = begin_server_edit_with(&state, &dir, "Other", &other_dir, &probe)
            .expect("another instance must stay editable");
        drop(guard);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unrelated_java_process_does_not_block_editing() {
        let dir = temp_dir("no-external");
        let state = test_state(&dir);
        struct NoExternal;
        impl ExternalGameProbe for NoExternal {
            fn find_for_game_dir(&self, _: &Path) -> Option<ExternalGame> {
                None
            }
        }
        let guard = begin_server_edit_with(&state, &dir, "Pack", &dir, &NoExternal)
            .expect("no matching Minecraft means editing is allowed");
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn conflict_is_mapped_to_the_structured_kind() {
        let error =
            ServerCommandError::from(crate::error::LauncherError::Conflict("changed".to_string()));
        assert!(matches!(error, ServerCommandError::Conflict));
    }

    #[test]
    fn structured_errors_serialize_with_a_stable_kind() {
        let value = serde_json::to_value(ServerCommandError::InstanceRunning).unwrap();
        assert_eq!(value["kind"], "instance_running");
        let value = serde_json::to_value(ServerCommandError::Conflict).unwrap();
        assert_eq!(value["kind"], "conflict");
    }
}

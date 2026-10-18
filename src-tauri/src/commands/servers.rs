//! Server list commands: read and edit the instance's servers.dat.

use crate::servers::{self, ServerEntry};
use crate::AppState;
use tauri::State;

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
) -> Result<Vec<ServerEntry>, String> {
    let path = servers_file(&state, &instance_name)?;
    tauri::async_runtime::spawn_blocking(move || {
        servers::add(&path, &name, &address).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cmd_update_server(
    state: State<'_, AppState>,
    instance_name: String,
    index: usize,
    name: String,
    address: String,
) -> Result<Vec<ServerEntry>, String> {
    let path = servers_file(&state, &instance_name)?;
    tauri::async_runtime::spawn_blocking(move || {
        servers::update(&path, index, &name, &address).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cmd_delete_server(
    state: State<'_, AppState>,
    instance_name: String,
    index: usize,
) -> Result<Vec<ServerEntry>, String> {
    let path = servers_file(&state, &instance_name)?;
    tauri::async_runtime::spawn_blocking(move || {
        servers::delete(&path, index).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Status check (Server List Ping) for one saved server.
///
/// The address must be present in the instance's saved server list: this
/// command is reachable from the webview, so it must not become an
/// arbitrary host:port probe. Callers pass the full saved address; any port
/// is parsed here exactly like the vanilla client does.
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
        move || servers::list(&path).map(|list| list.iter().any(|s| s.ip.trim() == wanted))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    if !saved {
        return Err("address is not in this instance's server list".to_string());
    }
    let (host, port) = crate::server_ping::parse_host_port(&wanted)?;
    Ok(crate::server_ping::ping_server(&host, port).await)
}

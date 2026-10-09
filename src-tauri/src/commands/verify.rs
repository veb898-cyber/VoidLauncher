//! Verify/repair instance commands: offline integrity scan and
//! launcher-managed file restoration.

use crate::instances;
use crate::verify;
use crate::AppState;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn cmd_verify_instance(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<verify::VerifyReport, String> {
    super::instances::validate_instance_name(&instance_name)?;
    let (instances_dir, config) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        (config.instances_dir(), config.clone())
    };
    tauri::async_runtime::spawn_blocking(move || {
        let instance =
            instances::get_instance(&instances_dir, &instance_name).map_err(|e| e.to_string())?;
        Ok(verify::verify_instance(&config, &instance))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cmd_repair_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<verify::RepairSummary, String> {
    super::instances::validate_instance_name(&instance_name)?;
    let config = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.clone()
    };
    let instance =
        instances::get_instance(&config.instances_dir(), &instance_name).map_err(|e| e.to_string())?;
    verify::repair_instance(&app, &config, &instance)
        .await
        .map_err(|e| e.to_string())
}

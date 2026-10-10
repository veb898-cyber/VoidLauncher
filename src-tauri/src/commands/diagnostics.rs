//! Diagnostics commands: system/instance health report and crash analysis.

use crate::diagnostics::{self, CrashAnalysis, DiagReport};
use crate::AppState;
use tauri::State;

/// Run the full diagnostics (system inventory + automatic problem
/// detection; instance checks when `instance_name` is given). Read-only.
#[tauri::command]
pub async fn cmd_run_diagnostics(
    state: State<'_, AppState>,
    instance_name: Option<String>,
) -> Result<DiagReport, String> {
    if let Some(name) = &instance_name {
        super::instances::validate_instance_name(name)?;
    }
    let config = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.clone()
    };
    Ok(diagnostics::run_diagnostics(config, instance_name).await)
}

/// Classify the most recent crash of an instance (crash report or log
/// tail) into a likely cause with a confidence verdict. Read-only.
#[tauri::command]
pub async fn cmd_analyze_crash(
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<CrashAnalysis, String> {
    super::instances::validate_instance_name(&instance_name)?;
    let config = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.clone()
    };
    tauri::async_runtime::spawn_blocking(move || {
        diagnostics::analyze_crash(&config, &instance_name)
    })
    .await
    .map_err(|e| e.to_string())?
}

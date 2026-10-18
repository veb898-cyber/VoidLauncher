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

/// Classify a specific run of an instance (crash report inside the run's
/// time window, or the tail of the run's own session log) into a likely
/// cause with a confidence verdict. `session_path` identifies the run; when
/// omitted, the newest session of the instance is used (auto-analysis right
/// after exit). Read-only.
#[tauri::command]
pub async fn cmd_analyze_crash(
    state: State<'_, AppState>,
    instance_name: String,
    session_path: Option<String>,
) -> Result<CrashAnalysis, String> {
    super::instances::validate_instance_name(&instance_name)?;
    let config = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.clone()
    };

    // The renderer picks the session file; keep it inside the launcher data
    // directory so a crafted path cannot make the backend read arbitrary
    // files (game logs live under <data_dir>/logs/game).
    let session_path = match session_path {
        Some(p) => {
            let data_dir = config
                .data_dir
                .canonicalize()
                .map_err(|_| "Invalid launcher data directory".to_string())?;
            match std::path::Path::new(&p).canonicalize() {
                Ok(canon) if canon.starts_with(&data_dir) => {
                    Some(canon.to_string_lossy().to_string())
                }
                Ok(_) => {
                    return Err(
                        "Access denied: session log is outside the launcher data directory"
                            .to_string(),
                    )
                }
                // Missing file: analyze as if no specific session was given.
                Err(_) => None,
            }
        }
        None => None,
    };

    tauri::async_runtime::spawn_blocking(move || {
        diagnostics::analyze_crash(&config, &instance_name, session_path.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

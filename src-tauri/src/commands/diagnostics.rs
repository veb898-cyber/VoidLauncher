//! Diagnostics commands: system/instance health report and crash analysis.

use crate::diagnostics::{self, CrashAnalysis, DiagReport};
use crate::instances;
use crate::recovery;
use crate::AppState;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

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

/// Apply one safe recovery action from a crash report. The webview sends only
/// a stable action kind and its argument; both are re-validated here.
#[tauri::command]
pub async fn cmd_apply_crash_action(
    state: State<'_, AppState>,
    instance_name: String,
    action: String,
    arg: String,
) -> Result<(), String> {
    super::instances::validate_instance_name(&instance_name)?;
    let config = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.clone()
    };
    // A disable is a mod-set edit: hold the same per-instance slot a repair
    // holds, so a launch cannot start between the running check and rename.
    // Take it before reading the instance so a concurrent delete cannot make
    // this command act on a stale directory.
    let edit_guard = if action == "disable_mod" {
        Some(
            state
                .launches
                .edit_guard(&instance_name)
                .map_err(|busy| super::verify::modification_busy(&instance_name, busy))?,
        )
    } else {
        None
    };
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let result = tauri::async_runtime::spawn_blocking(move || {
        recovery::execute(&config, &instance, &action, &arg)
    })
    .await
    .map_err(|e| e.to_string())?;
    drop(edit_guard);
    result
}

/// Open one of the official driver pages offered by the crash analyzer.
/// The host/scheme allowlist is re-checked here so a crafted action cannot
/// turn the opener into an arbitrary-URL primitive.
#[tauri::command]
pub fn cmd_open_official_url(app: AppHandle, url: String) -> Result<(), String> {
    recovery::validate_official_url(&url)?;
    app.opener()
        .open_url(&url, None::<&str>)
        .map_err(|e| e.to_string())
}

//! Verify/repair instance commands: offline integrity scan and
//! launcher-managed file restoration.

use crate::instances;
use crate::launch_state::LaunchBusy;
use crate::verify;
use crate::AppState;
use tauri::{AppHandle, State};

pub(crate) fn modification_busy(instance: &str, busy: LaunchBusy) -> String {
    match busy {
        LaunchBusy::Starting => format!(
            "Instance '{}' is starting. Wait for the game to finish preparing before repairing its files.",
            instance
        ),
        LaunchBusy::Running => format!(
            "Instance '{}' is running. Close the game before repairing its files.",
            instance
        ),
        LaunchBusy::Editing => format!(
            "Instance '{}' is already being modified. Wait for the current operation to finish.",
            instance
        ),
        LaunchBusy::Preparing => format!(
            "Instance '{}' is still being installed or prepared. Wait for setup to finish before repairing its files.",
            instance
        ),
    }
}

#[tauri::command]
pub async fn cmd_verify_instance(
    state: State<'_, AppState>,
    instance_name: String,
    scope: Option<String>,
    mode: Option<String>,
) -> Result<verify::VerifyReport, String> {
    super::instances::validate_instance_name(&instance_name)?;
    let scope =
        verify::VerifyScope::parse(scope.as_deref().unwrap_or("all")).map_err(|e| e.to_string())?;
    let mode =
        verify::VerifyMode::parse(mode.as_deref().unwrap_or("deep")).map_err(|e| e.to_string())?;
    let (instances_dir, config) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        (config.instances_dir(), config.clone())
    };
    tauri::async_runtime::spawn_blocking(move || {
        let instance =
            instances::get_instance(&instances_dir, &instance_name).map_err(|e| e.to_string())?;
        let report = verify::verify_instance_scoped_mode(&config, &instance, scope, mode);
        // A user-visible "verify failed" is otherwise impossible to diagnose
        // from a log: print exactly which category and file failed. Paths are
        // instance-relative and contain no tokens.
        if !report.healthy {
            let categories: Vec<String> = report
                .categories
                .iter()
                .map(|c| format!("{}={}/{}", c.id, c.issue_count, c.checked))
                .collect();
            tracing::warn!(
                target: "launcher",
                "Verify '{}' (scope {:?}, mode {:?}) unhealthy: repairable={}, content={}, categories: {}",
                instance.name,
                scope,
                mode,
                report.repairable_count,
                report.content_issues,
                categories.join(", ")
            );
            for category in &report.categories {
                for issue in &category.issues {
                    tracing::warn!(
                        target: "launcher",
                        "Verify issue [{}] {} kind={} repairable={}",
                        category.id,
                        issue.path,
                        issue.kind,
                        issue.repairable
                    );
                }
            }
        }
        Ok(report)
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
    // Reserve the instance before reading its config: a delete/save that
    // started just before this command must not let repair recreate files in
    // a disappeared instance directory.
    let reservation = state
        .launches
        .edit_guard(&instance_name)
        .map_err(|busy| modification_busy(&instance_name, busy))?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let result = verify::repair_instance(&app, &config, &instance).await;
    drop(reservation);
    result.map_err(|e| e.to_string())
}

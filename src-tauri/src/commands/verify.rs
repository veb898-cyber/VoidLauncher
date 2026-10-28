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
    scope: Option<String>,
) -> Result<verify::VerifyReport, String> {
    super::instances::validate_instance_name(&instance_name)?;
    let scope =
        verify::VerifyScope::parse(scope.as_deref().unwrap_or("all")).map_err(|e| e.to_string())?;
    let (instances_dir, config) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        (config.instances_dir(), config.clone())
    };
    tauri::async_runtime::spawn_blocking(move || {
        let instance =
            instances::get_instance(&instances_dir, &instance_name).map_err(|e| e.to_string())?;
        let report = verify::verify_instance_scoped(&config, &instance, scope);
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
                "Verify '{}' (scope {:?}) unhealthy: repairable={}, content={}, categories: {}",
                instance.name,
                scope,
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
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    verify::repair_instance(&app, &config, &instance)
        .await
        .map_err(|e| e.to_string())
}

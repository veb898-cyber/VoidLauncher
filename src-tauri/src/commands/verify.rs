//! Verify/repair instance commands: offline integrity scan and
//! launcher-managed file restoration.

use crate::instances;
use crate::launch_state::{EditGuard, LaunchBusy, LaunchRegistry, OwnedEditGuard};
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

/// The exact reservation acquired by `cmd_repair_instance` before reading or
/// changing any instance file. Keeping it in one helper lets tests prove the
/// command path is blocked while Verify owns the edit guard.
pub(crate) fn acquire_repair_reservation<'a>(
    state: &'a AppState,
    instance_name: &str,
) -> Result<EditGuard<'a>, String> {
    state
        .launches
        .edit_guard(instance_name)
        .map_err(|busy| modification_busy(instance_name, busy))
}

/// Command preflight shared by the real `cmd_repair_instance`: name validation,
/// config snapshot and the edit reservation all happen before reading the
/// instance or touching files.
pub(crate) fn acquire_repair_command<'a>(
    state: &'a AppState,
    instance_name: &str,
) -> Result<(crate::config::AppConfig, EditGuard<'a>), String> {
    super::instances::validate_instance_name(instance_name)?;
    let config = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.clone()
    };
    let reservation = acquire_repair_reservation(state, instance_name)?;
    Ok((config, reservation))
}

/// Reserve an instance for the whole read-only scan. `Editing` is the
/// existing mutation phase; holding its RAII guard prevents Repair/Install/
/// Update/launch preparation from starting midway through Verify. The guard is
/// dropped on every return path, including a scope parse error or a panic in
/// the blocking scan.
fn verify_busy_message(instance_name: &str, busy: LaunchBusy) -> String {
    let reason = match busy {
        LaunchBusy::Starting => "it is still starting",
        LaunchBusy::Running => "Minecraft is running",
        LaunchBusy::Editing => "another operation is already modifying it",
        LaunchBusy::Preparing => "it is still being installed or prepared",
    };
    format!(
        "Cannot verify instance '{}': {}. Wait for the current operation to finish.",
        instance_name, reason
    )
}

#[cfg(test)]
pub(crate) fn acquire_verify_reservation<'a>(
    launches: &'a LaunchRegistry,
    instance_name: &str,
) -> Result<EditGuard<'a>, String> {
    launches
        .edit_guard(instance_name)
        .map_err(|busy| verify_busy_message(instance_name, busy))
}

/// Owned reservation for the blocking Verify worker. The worker keeps this
/// guard until the scan has actually finished, not merely until the async
/// command future that spawned it stops being polled.
pub(crate) fn acquire_verify_reservation_owned(
    launches: &LaunchRegistry,
    instance_name: &str,
) -> Result<OwnedEditGuard, String> {
    launches
        .edit_guard_owned(instance_name)
        .map_err(|busy| verify_busy_message(instance_name, busy))
}

#[tauri::command]
pub async fn cmd_verify_instance(
    state: State<'_, AppState>,
    instance_name: String,
    scope: Option<String>,
    mode: Option<String>,
) -> Result<verify::VerifyReport, String> {
    super::instances::validate_instance_name(&instance_name)?;
    // Verification is read-only, but install/repair/launch can replace the
    // same files. Reserve the instance atomically for the entire scan so the
    // report cannot mix pre- and post-publication states.
    let verify_reservation = acquire_verify_reservation_owned(&state.launches, &instance_name)?;
    let scope =
        verify::VerifyScope::parse(scope.as_deref().unwrap_or("all")).map_err(|e| e.to_string())?;
    let mode =
        verify::VerifyMode::parse(mode.as_deref().unwrap_or("deep")).map_err(|e| e.to_string())?;
    let (instances_dir, config) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        (config.instances_dir(), config.clone())
    };
    tauri::async_runtime::spawn_blocking(move || {
        // This guard is owned by the worker, not by the async command frame.
        // Dropping the command future therefore cannot release the instance
        // while the scanner still reads it.
        let _verify_reservation = verify_reservation;
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
    // Reserve the instance before reading its config: a delete/save that
    // started just before this command must not let repair recreate files in
    // a disappeared instance directory.
    let (config, reservation) = acquire_repair_command(state.inner(), &instance_name)?;
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    let result = verify::repair_instance(&app, &config, &instance).await;
    drop(reservation);
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch_state::LaunchRegistry;
    use crate::{AppConfig, AppState};
    use std::future::Future;
    use std::path::Path;

    fn test_state(dir: &Path) -> AppState {
        std::fs::create_dir_all(dir).unwrap();
        AppState {
            config: std::sync::Mutex::new(AppConfig {
                data_dir: dir.to_path_buf(),
                ..AppConfig::default()
            }),
            auth_state: std::sync::Mutex::new(crate::auth::AuthState::default()),
            launches: LaunchRegistry::new(),
            pack_watcher: std::sync::Mutex::new(None),
            active_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            room_state: std::sync::Arc::new(crate::rooms::room_state::RoomStateManager::new(dir)),
            tailscale: crate::rooms::tailscale::TailscaleManager::new(dir.to_path_buf()),
        }
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "vl_verify_command_{}_{}",
            label,
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn verify_reservation_blocks_writers_until_dropped() {
        let launches = LaunchRegistry::new();
        let reservation = acquire_verify_reservation(&launches, "Pack").expect("free instance");
        assert!(launches.is_busy("Pack"));
        assert!(launches.begin_launch("Pack").is_err());
        assert!(launches.begin_edit("Pack").is_err());
        assert!(launches.begin_prepare("Pack").is_err());
        drop(reservation);
        assert!(!launches.is_busy("Pack"));
        let token = launches
            .begin_launch("Pack")
            .expect("released after verify");
        assert!(launches.release("Pack", token));
    }

    #[test]
    fn verify_reservation_reports_each_busy_phase() {
        for phase in ["starting", "running", "editing", "preparing"] {
            let launches = LaunchRegistry::new();
            let token = match phase {
                "starting" => launches.begin_launch("Pack").unwrap(),
                "running" => {
                    let token = launches.begin_launch("Pack").unwrap();
                    assert!(launches.mark_running("Pack", token, None));
                    token
                }
                "editing" => launches.begin_edit("Pack").unwrap(),
                "preparing" => launches.begin_prepare("Pack").unwrap(),
                _ => unreachable!(),
            };
            let error = match acquire_verify_reservation(&launches, "Pack") {
                Ok(_) => panic!("busy instance must not be verified"),
                Err(error) => error,
            };
            assert!(
                error.contains("Cannot verify instance 'Pack'"),
                "{phase}: {error}"
            );
            assert!(launches.release("Pack", token));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_command_preflights_and_verify_command_reject_busy_before_side_effects() {
        use tauri::Manager;

        let dir = temp_dir("command_busy");
        let app = tauri::test::mock_app();
        app.manage(test_state(&dir));
        let state = app.state::<AppState>();

        let verify = acquire_verify_reservation_owned(&state.launches, "Pack").unwrap();

        let install =
            crate::commands::launcher::acquire_loader_install_context(state.inner(), "Pack");
        assert!(
            install
                .as_ref()
                .is_err_and(|error| error.contains("modifying")),
            "install preflight must return Busy"
        );

        let update = crate::commands::launcher::acquire_update_loader_context(
            state.inner(),
            "Pack",
            "47.4.1",
            "en",
        );
        assert!(
            update
                .as_ref()
                .is_err_and(|error| error.contains("modifying")),
            "update preflight must return Busy"
        );

        let repair = acquire_repair_command(state.inner(), "Pack");
        assert!(
            repair
                .as_ref()
                .is_err_and(|error| error.contains("modified")),
            "repair preflight must return Busy"
        );

        let launch = crate::commands::launcher::acquire_launch_gate(state.inner(), "Pack");
        assert!(
            launch
                .as_ref()
                .is_err_and(|error| error.contains("being modified")),
            "launch gate must return Busy"
        );
        assert!(!dir.join("instances").join("Pack").exists());
        assert!(!dir.join("logs").join("Pack").exists());

        drop(verify);
        let other = state.launches.begin_edit("Other").expect("free instance");
        let verify_busy = cmd_verify_instance(
            app.state::<AppState>(),
            "Other".into(),
            Some("all".into()),
            Some("deep".into()),
        )
        .await;
        assert!(
            verify_busy
                .as_ref()
                .is_err_and(|error| error.contains("Cannot verify instance")),
            "verify command must return Busy: {verify_busy:?}"
        );
        assert!(state.launches.release("Other", other));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn verify_command_keeps_reservation_until_blocking_scan_finishes() {
        use crate::instances::{Instance, LoaderType};
        use std::sync::{mpsc, Arc, Barrier};
        use std::time::Duration;
        use tauri::Manager;

        let dir = temp_dir("verify_cancel_worker");
        let app = tauri::test::mock_app();
        app.manage(test_state(&dir));
        let state = app.state::<AppState>();
        let instance = Instance::new("CancelHookPack", "1.20.1", LoaderType::Vanilla, None);
        instances::create_instance(
            &state.config.lock().unwrap().instances_dir(),
            &instance,
            &state.launches,
        )
        .unwrap();

        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new(Barrier::new(2));
        crate::verify::set_verify_scan_hook(Some(crate::verify::VerifyScanHook {
            instance_name: "CancelHookPack".to_string(),
            started: started_tx,
            release: release.clone(),
        }));

        let mut future = Box::pin(cmd_verify_instance(
            app.state::<AppState>(),
            "CancelHookPack".into(),
            Some("all".into()),
            Some("deep".into()),
        ));
        let waker = futures::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        assert!(future.as_mut().poll(&mut context).is_pending());

        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("blocking scan started");
        assert!(state.launches.is_busy("CancelHookPack"));
        drop(future);
        assert!(
            state.launches.is_busy("CancelHookPack"),
            "dropping the command must not release a running blocking scan"
        );

        release.wait();
        for _ in 0..200 {
            if !state.launches.is_busy("CancelHookPack") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !state.launches.is_busy("CancelHookPack"),
            "reservation must be released after the scan actually finishes"
        );
        crate::verify::set_verify_scan_hook(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_update_repair_and_launch_paths_are_busy_during_verify() {
        let dir = temp_dir("busy_paths");
        let state = test_state(&dir);
        let verify = acquire_verify_reservation(&state.launches, "Pack").unwrap();

        let install = match crate::commands::launcher::LoaderInstallContext::acquire(&state, "Pack")
        {
            Ok(_) => panic!("Install path must be blocked by Verify"),
            Err(error) => error,
        };
        assert!(
            install.contains("another operation is already modifying it"),
            "install error: {install}"
        );

        let update = match crate::commands::launcher::LoaderInstallContext::acquire(&state, "Pack")
        {
            Ok(_) => panic!("Update path must be blocked by Verify"),
            Err(error) => error,
        };
        assert!(
            update.contains("another operation is already modifying it"),
            "update error: {update}"
        );

        let repair = match acquire_repair_reservation(&state, "Pack") {
            Ok(_) => panic!("Repair path must be blocked by Verify"),
            Err(error) => error,
        };
        assert!(
            repair.contains("already being modified"),
            "repair error: {repair}"
        );

        let launch = match crate::commands::launcher::acquire_launch_gate(&state, "Pack") {
            Ok(_) => panic!("Launch preparation must be blocked by Verify"),
            Err(error) => error,
        };
        assert!(launch.contains("being modified"), "launch error: {launch}");

        drop(verify);
        assert!(!state.launches.is_busy("Pack"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn verify_reservation_is_released_when_the_operation_is_cancelled() {
        let dir = temp_dir("cancel");
        let state = test_state(&dir);
        {
            let mut operation = Box::pin(async {
                let _reservation = acquire_verify_reservation(&state.launches, "Pack").unwrap();
                std::future::pending::<()>().await;
            });
            let waker = futures::task::noop_waker();
            let mut context = std::task::Context::from_waker(&waker);
            assert!(operation.as_mut().poll(&mut context).is_pending());
            assert!(state.launches.is_busy("Pack"));
            // Dropping the future cancels the verification operation and must
            // release the RAII reservation.
            drop(operation);
        }
        assert!(!state.launches.is_busy("Pack"));
        let token = state
            .launches
            .begin_launch("Pack")
            .expect("cancelled Verify must release the instance");
        assert!(state.launches.release("Pack", token));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_reservation_is_released_on_panic() {
        let launches = LaunchRegistry::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _reservation = acquire_verify_reservation(&launches, "Pack").unwrap();
            panic!("simulated verification panic");
        }));
        assert!(result.is_err());
        assert!(!launches.is_busy("Pack"));
        let token = launches
            .begin_launch("Pack")
            .expect("a panicking Verify must release the instance");
        assert!(launches.release("Pack", token));
    }

    #[test]
    fn verify_reservation_is_released_on_early_return() {
        let dir = temp_dir("early_return");
        let state = test_state(&dir);
        let body = |state: &AppState| -> Result<(), String> {
            let _reservation = acquire_verify_reservation(&state.launches, "Pack")?;
            // Mirrors an early scope-parse / get_instance error in
            // `cmd_verify_instance`: the RAII guard must be dropped here.
            Err("early verification error".to_string())
        };
        assert!(body(&state).is_err());
        assert!(!state.launches.is_busy("Pack"));
        let token = state
            .launches
            .begin_launch("Pack")
            .expect("reservation must be released after an early return");
        assert!(state.launches.release("Pack", token));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

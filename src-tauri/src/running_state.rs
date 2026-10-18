use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Persisted record of a launched Minecraft process.
///
/// The launcher's in-memory `running_instances` list dies with the process
/// (and is empty after an app restart or an auto-update relaunch), which
/// used to orphan a still-running game: no "Running" badge, no way to stop
/// it, and a second launch of the same instance would start a second game
/// on the same directory. These pid files survive launcher restarts and are
/// reconciled against the live process table whenever the UI asks for the
/// launch state (and before every launch).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RunningGame {
    pub instance: String,
    pub pid: u32,
    pub started_at: String,
}

fn running_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("running")
}

fn pid_file(data_dir: &Path, instance: &str) -> PathBuf {
    running_dir(data_dir).join(format!(
        "{}.json",
        crate::game_logs::sanitize_instance_name(instance)
    ))
}

/// Best-effort: record that `instance` is running as `pid`.
pub fn write_pid_file(data_dir: &Path, instance: &str, pid: u32) {
    let _ = std::fs::create_dir_all(running_dir(data_dir));
    let rec = RunningGame {
        instance: instance.to_string(),
        pid,
        started_at: chrono::Local::now().to_rfc3339(),
    };
    if let Ok(json) = serde_json::to_string(&rec) {
        let _ = std::fs::write(pid_file(data_dir, instance), json);
    }
}

/// Best-effort: drop the pid file for `instance` (called on game exit / stop).
pub fn remove_pid_file(data_dir: &Path, instance: &str) {
    let _ = std::fs::remove_file(pid_file(data_dir, instance));
}

/// Liveness check for a pid via the OS process table. Used to reconcile pid
/// files after a launcher restart and to avoid killing a recycled pid when a
/// stale record points at a since-exited process.
pub fn is_process_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).is_some()
}

/// Scan the running directory. Records whose process is gone are deleted
/// (self-healing after crashes); live ones are returned.
pub fn scan_live_games(data_dir: &Path) -> Vec<RunningGame> {
    let dir = running_dir(data_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let parsed: Option<RunningGame> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        match parsed {
            Some(rec) if is_process_alive(rec.pid) => live.push(rec),
            _ => {
                // Dead, corrupt or unreadable — the game is not running.
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    live.sort_by(|a, b| a.instance.cmp(&b.instance));
    live
}

/// Kill a game process and its children. `taskkill /T` is used on Windows
/// because the JVM may own helper processes (crash reporter, AWT windows)
/// that `Child::kill()` would leave behind.
pub fn kill_process_tree(pid: u32) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let status = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| format!("Failed to run taskkill: {}", e))?;
        if status.success() {
            Ok(())
        } else {
            // The process may have already exited between the liveness check
            // and taskkill — that is not an error for our purposes.
            if is_process_alive(pid) {
                Err(format!("taskkill failed for pid {}", pid))
            } else {
                Ok(())
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let status = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .map_err(|e| format!("Failed to run kill: {}", e))?;
        if status.success() || !is_process_alive(pid) {
            Ok(())
        } else {
            Err(format!("kill failed for pid {}", pid))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_round_trip_and_liveness_reconciliation() {
        let dir = std::env::temp_dir().join(format!("vl_running_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // Our own process is alive by definition.
        let own = std::process::id();
        write_pid_file(&dir, "My Pack [1.21]", own);
        assert!(pid_file(&dir, "My Pack [1.21]").exists());

        // A definitely-dead pid: spawn and reap a child.
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();
        write_pid_file(&dir, "Dead", dead_pid);

        let live = scan_live_games(&dir);
        assert_eq!(live.len(), 1, "only the live record survives");
        assert_eq!(live[0].instance, "My Pack [1.21]");
        assert_eq!(live[0].pid, own);
        assert!(
            !pid_file(&dir, "Dead").exists(),
            "dead records must be cleaned up"
        );

        remove_pid_file(&dir, "My Pack [1.21]");
        assert!(scan_live_games(&dir).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_pid_file_is_removed_without_panic() {
        let dir = std::env::temp_dir().join(format!("vl_running_corrupt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rdir = running_dir(&dir);
        std::fs::create_dir_all(&rdir).unwrap();
        std::fs::write(rdir.join("broken.json"), "{not json").unwrap();

        assert!(scan_live_games(&dir).is_empty());
        assert!(!rdir.join("broken.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

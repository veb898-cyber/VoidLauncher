//! Persistent readiness state for an instance directory.
//!
//! A directory, `instance.json` or `instance.cfg` alone does NOT prove that an
//! instance is complete: importers write metadata before loader/game files are
//! prepared, and a crash can leave any intermediate shape behind. Readiness is
//! therefore recorded explicitly:
//!
//! - every create/import/install writes [`PENDING_MARKER`] before it writes
//!   any instance content;
//! - the marker is retired only after the importer, pack-lock write and
//!   preparation have all finished;
//! - a launch refuses an instance whose marker still exists.
//!
//! Older instances have no marker. Absence means "legacy, already complete",
//! which keeps every existing library launchable after the update.
//!
//! The marker is not the only gate: the in-memory [`LaunchRegistry`] holds a
//! `Preparing` slot for the whole operation, so a launch cannot slip in
//! between the readiness check and the first file write.
//!
//! [`LaunchRegistry`]: crate::launch_state::LaunchRegistry

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::instances::write_file_atomic;

/// Written inside the instance directory while it is being created/imported.
pub(crate) const PENDING_MARKER: &str = ".void-instance-pending";

/// Where the instance is in its setup lifecycle. The stage is persisted so an
/// interrupted operation can be described after a restart:
/// `Importing` cannot be resumed (the pack archive must be imported again),
/// while `Preparing` can safely rerun the idempotent file preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStage {
    Importing,
    Preparing,
}

impl SetupStage {
    pub fn as_str(self) -> &'static str {
        match self {
            SetupStage::Importing => "importing",
            SetupStage::Preparing => "preparing",
        }
    }
}

/// On-disk marker contents. The token proves which operation owns the
/// directory, so a stale operation can never retire a newer one's marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingSetup {
    pub stage: SetupStage,
    /// Human-readable operation kind ("import", "install", "create").
    pub operation: String,
    /// RFC 3339 timestamp of the original reservation.
    pub started_at: String,
    pub token: String,
}

/// UI-facing state for an instance that is not launchable yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceSetupState {
    pub operation: String,
    pub stage: String,
    pub started_at: String,
}

/// Read the pending marker.
///
/// A missing marker means "ready or legacy". Any other read/parse failure is
/// treated as pending with an unknown stage: the file exists, so the launcher
/// must fail closed instead of guessing that the instance is complete.
pub(crate) fn read_pending(dir: &Path) -> Option<PendingSetup> {
    let path = dir.join(PENDING_MARKER);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(
                target: "launcher",
                "Instance readiness marker {} is unreadable ({}); treating the instance as incomplete",
                path.display(),
                e
            );
            return Some(PendingSetup {
                stage: SetupStage::Importing,
                operation: "unknown".to_string(),
                started_at: String::new(),
                token: String::new(),
            });
        }
    };
    match serde_json::from_str::<PendingSetup>(&text) {
        Ok(pending) => Some(pending),
        Err(e) => {
            tracing::warn!(
                target: "launcher",
                "Instance readiness marker {} is corrupt ({}); treating the instance as incomplete",
                path.display(),
                e
            );
            Some(PendingSetup {
                stage: SetupStage::Importing,
                operation: "unknown".to_string(),
                started_at: String::new(),
                token: String::new(),
            })
        }
    }
}

/// UI-facing readiness state for an instance directory.
pub fn setup_state(dir: &Path) -> Option<InstanceSetupState> {
    read_pending(dir).map(|pending| InstanceSetupState {
        operation: pending.operation,
        stage: pending.stage.as_str().to_string(),
        started_at: pending.started_at,
    })
}

/// Does this directory look like an operation interrupted *before* metadata
/// was written? Such a directory is hidden from the instance list (it has no
/// usable name/version), so a retry of the same import parks it instead of
/// reporting a confusing "already exists" error.
pub(crate) fn is_incomplete_target(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    if dir.join("instance.json").is_file() || dir.join("instance.cfg").is_file() {
        return false;
    }
    // Either marker proves an operation owned this directory; an owner marker
    // without a pending marker is a crash between the two writes in an older
    // build. Both are safe to park and retry.
    dir.join(crate::import::IMPORT_OWNER_MARKER).is_file() || read_pending(dir).is_some()
}

/// Write the marker atomically. The caller must already own the directory and
/// the `Preparing` registry slot.
pub(crate) fn write_pending(dir: &Path, pending: &PendingSetup) -> Result<()> {
    let json = serde_json::to_string_pretty(pending)?;
    write_file_atomic(&dir.join(PENDING_MARKER), json.as_bytes())
}

/// Retire the marker only when it still belongs to `token`.
pub(crate) fn clear_pending(dir: &Path, token: &str) -> Result<()> {
    let path = dir.join(PENDING_MARKER);
    match read_pending(dir) {
        Some(current) if current.token == token => {}
        Some(_) => {
            return Err(crate::error::LauncherError::Instance(format!(
                "The setup marker in {} belongs to another operation; not touching it.",
                dir.display()
            )))
        }
        None => return Ok(()),
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Remove crash-leftover `.void-setup-*.tmp` staging directories from the
/// instances directory. They are hidden from the instance list by design and
/// only exist between reservation and the atomic rename into place, so they
/// never hold published user data. Safe to call once at startup.
///
/// `.void-recovery-*` folders are deliberately NOT touched: they preserve an
/// interrupted import's files for manual recovery.
pub fn cleanup_abandoned_setup_dirs(instances_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(instances_dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(".void-setup-") || !name.ends_with(".tmp") {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                removed += 1;
                tracing::warn!(
                    target: "launcher",
                    "Removed abandoned setup folder from an interrupted operation: {}",
                    path.display()
                );
            }
            Err(e) => tracing::warn!(
                target: "launcher",
                "Could not remove abandoned setup folder {}: {}",
                path.display(),
                e
            ),
        }
    }
    removed
}

/// Directories that preserve the remains of an interrupted import. They stay
/// on disk until the user decides what to do; this lets the launcher report
/// them instead of silently pretending nothing happened.
pub fn recovery_dirs(instances_dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(instances_dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".void-recovery-")
                && entry.path().is_dir()
        })
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

/// Backend launch gate. The in-memory `Preparing` slot is the race-free part;
/// this persistent marker is what survives a crash and app restart.
pub(crate) fn ensure_launch_ready(dir: &Path, instance_name: &str) -> Result<()> {
    if let Some(pending) = read_pending(dir) {
        let detail = match (pending.stage, pending.operation.as_str()) {
            (SetupStage::Importing, "import") => {
                "the import did not finish. Delete the incomplete instance and import the pack again."
            }
            (SetupStage::Importing, _) => {
                "the instance was not fully created. Delete the incomplete instance and try again."
            }
            (SetupStage::Preparing, _) => {
                "the game files are still being prepared. Wait for setup to finish."
            }
        };
        return Err(crate::error::LauncherError::Instance(format!(
            "Instance '{}' is not ready to launch: {}",
            instance_name, detail
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vl_readiness_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pending(stage: SetupStage, token: &str) -> PendingSetup {
        PendingSetup {
            stage,
            operation: "import".to_string(),
            started_at: "2026-11-14T00:00:00Z".to_string(),
            token: token.to_string(),
        }
    }

    #[test]
    fn a_missing_marker_means_ready_for_legacy_instances() {
        let dir = temp_dir("missing");
        assert!(read_pending(&dir).is_none());
        assert!(ensure_launch_ready(&dir, "Legacy").is_ok());
        assert!(setup_state(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pending_import_blocks_launch_until_the_marker_is_retired() {
        let dir = temp_dir("pending");
        write_pending(&dir, &pending(SetupStage::Importing, "owner-1")).unwrap();
        assert!(ensure_launch_ready(&dir, "Pack").is_err());
        assert_eq!(
            setup_state(&dir).unwrap().stage,
            "importing",
            "the UI must be able to explain an interrupted import"
        );

        // A different operation cannot retire our marker.
        assert!(clear_pending(&dir, "owner-2").is_err());
        assert!(read_pending(&dir).is_some());

        clear_pending(&dir, "owner-1").unwrap();
        assert!(ensure_launch_ready(&dir, "Pack").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn metadata_without_a_marker_is_not_parked_as_incomplete() {
        let dir = temp_dir("metadata");
        std::fs::write(dir.join("instance.json"), b"{}").unwrap();
        assert!(!is_incomplete_target(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_without_metadata_and_with_pending_marker_is_parkable() {
        let dir = temp_dir("incomplete");
        write_pending(&dir, &pending(SetupStage::Importing, "owner")).unwrap();
        assert!(is_incomplete_target(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_owner_marker_without_metadata_is_parkable_too() {
        let dir = temp_dir("owner-only");
        std::fs::write(
            dir.join(crate::import::IMPORT_OWNER_MARKER),
            b"crashed-before-readiness-marker",
        )
        .unwrap();
        assert!(
            is_incomplete_target(&dir),
            "a crash between the ownership and readiness markers must not block a retry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_marker_fails_closed() {
        let dir = temp_dir("corrupt");
        std::fs::write(dir.join(PENDING_MARKER), b"not json").unwrap();
        assert!(read_pending(&dir).is_some());
        assert!(ensure_launch_ready(&dir, "Pack").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abandoned_setup_dirs_are_removed_but_recovery_data_is_kept() {
        let dir = temp_dir("staging_cleanup");
        let setup = dir.join(".void-setup-abc123.tmp");
        std::fs::create_dir_all(&setup).unwrap();
        std::fs::write(setup.join("partial"), b"x").unwrap();
        let recovery = dir.join(".void-recovery-Pack-123");
        std::fs::create_dir_all(&recovery).unwrap();
        std::fs::write(recovery.join("instance.json"), b"{}").unwrap();
        let real = dir.join("Real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("instance.json"), b"{}").unwrap();

        assert_eq!(cleanup_abandoned_setup_dirs(&dir), 1);
        assert!(!setup.exists(), "abandoned staging is transient");
        assert!(
            recovery.exists(),
            "interrupted-import data must never be deleted automatically"
        );
        assert!(real.exists());
        assert_eq!(recovery_dirs(&dir), vec![recovery]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

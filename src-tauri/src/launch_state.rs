//! Single synchronized model of what the launcher is currently doing with an
//! instance.
//!
//! An instance can be in exactly one of three phases, and every transition
//! happens under one mutex, so a concurrent `Stop`, a second `Play`, or an
//! instance mutation can never observe a half-updated state:
//!
//! - [`LaunchPhase::Starting`] — `cmd_launch_game` is still preparing the run
//!   (account, Java, downloads, natives, launch profile). The instance is
//!   reserved, so nothing else may modify or delete it, but no game process
//!   exists yet, so it must NOT be reported as a running game.
//! - [`LaunchPhase::Running`] — the game process was spawned and its pid
//!   recorded. This is the only phase the UI shows as a running game.
//! - [`LaunchPhase::Editing`] — a config/resource-pack mutation holds the
//!   instance; a launch must wait for it.
//!
//! Each reservation carries a [`LaunchToken`]: a reservation may only release
//! the entry it created, never an entry of a newer run of the same instance.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// What the launcher is doing with one instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchPhase {
    Starting,
    Running,
    Editing,
}

/// Why an instance cannot be reserved right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchBusy {
    Starting,
    Running,
    Editing,
}

impl LaunchBusy {
    pub fn phase(self) -> LaunchPhase {
        match self {
            LaunchBusy::Starting => LaunchPhase::Starting,
            LaunchBusy::Running => LaunchPhase::Running,
            LaunchBusy::Editing => LaunchPhase::Editing,
        }
    }
}

impl From<LaunchPhase> for LaunchBusy {
    fn from(phase: LaunchPhase) -> Self {
        match phase {
            LaunchPhase::Starting => LaunchBusy::Starting,
            LaunchPhase::Running => LaunchBusy::Running,
            LaunchPhase::Editing => LaunchBusy::Editing,
        }
    }
}

/// Ownership token of one reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchToken(u64);

/// Token of an entry recovered from a persisted pid file. It never belongs to
/// a live reservation, so a `release` with a real token cannot remove it.
const ADOPTED: u64 = 0;

#[derive(Debug, Clone, Copy)]
struct Entry {
    token: u64,
    phase: LaunchPhase,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    next_token: u64,
}

/// One mutex for every per-instance phase: `Starting`, `Running` and the
/// config-mutation lock are decided together, never by separate locks.
pub struct LaunchRegistry {
    inner: Mutex<Inner>,
}

impl Default for LaunchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LaunchRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned registry only means some other thread panicked while
        // holding it; the map itself is still consistent. Recovering keeps
        // `Drop` infallible and never leaves a reservation stuck forever.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn reserve(&self, instance: &str, phase: LaunchPhase) -> Result<LaunchToken, LaunchBusy> {
        let mut inner = self.lock();
        if let Some(entry) = inner.entries.get(instance) {
            return Err(entry.phase.into());
        }
        inner.next_token += 1;
        let token = LaunchToken(inner.next_token);
        inner.entries.insert(
            instance.to_string(),
            Entry {
                token: token.0,
                phase,
            },
        );
        Ok(token)
    }

    /// Reserve the instance for a launch: it enters `Starting` and stays
    /// reserved until the launch either spawns ([`Self::mark_running`]) or
    /// fails ([`Self::release`]).
    pub fn begin_launch(&self, instance: &str) -> Result<LaunchToken, LaunchBusy> {
        self.reserve(instance, LaunchPhase::Starting)
    }

    /// Reserve the instance for a config/resource-pack mutation.
    pub fn begin_edit(&self, instance: &str) -> Result<LaunchToken, LaunchBusy> {
        self.reserve(instance, LaunchPhase::Editing)
    }

    /// Transition our own reservation `Starting` -> `Running`.
    ///
    /// Returns `false` when the entry no longer belongs to this token (it was
    /// already released or replaced), in which case nothing is changed.
    pub fn mark_running(&self, instance: &str, token: LaunchToken) -> bool {
        let mut inner = self.lock();
        match inner.entries.get_mut(instance) {
            Some(entry) if entry.token == token.0 && entry.phase == LaunchPhase::Starting => {
                entry.phase = LaunchPhase::Running;
                true
            }
            _ => false,
        }
    }

    /// Release the entry this token created. A newer reservation of the same
    /// instance is never touched.
    pub fn release(&self, instance: &str, token: LaunchToken) -> bool {
        let mut inner = self.lock();
        match inner.entries.get(instance) {
            Some(entry) if entry.token == token.0 => {
                inner.entries.remove(instance);
                true
            }
            _ => false,
        }
    }

    /// Forget a `Running` entry whose process is gone (stale in-memory record).
    ///
    /// `Starting` and `Editing` entries are deliberately left alone: they
    /// belong to work that is still in flight.
    pub fn clear_running(&self, instance: &str) -> bool {
        let mut inner = self.lock();
        match inner.entries.get(instance) {
            Some(entry) if entry.phase == LaunchPhase::Running => {
                inner.entries.remove(instance);
                true
            }
            _ => false,
        }
    }

    /// Register a game recovered from a persisted pid file. Never downgrades a
    /// live reservation: a launch that is still preparing stays `Starting`.
    pub fn adopt_running(&self, instance: &str) {
        let mut inner = self.lock();
        inner.entries.entry(instance.to_string()).or_insert(Entry {
            token: ADOPTED,
            phase: LaunchPhase::Running,
        });
    }

    pub fn phase(&self, instance: &str) -> Option<LaunchPhase> {
        self.lock().entries.get(instance).map(|entry| entry.phase)
    }

    /// Is the instance reserved for anything (launch preparation, running
    /// game, or a config mutation)?
    pub fn is_busy(&self, instance: &str) -> bool {
        self.phase(instance).is_some()
    }

    /// Names whose game process exists. `Starting` and `Editing` are not
    /// reported: the UI must not present a preparing instance as a running one.
    pub fn running_instances(&self) -> Vec<String> {
        let inner = self.lock();
        let mut names: Vec<String> = inner
            .entries
            .iter()
            .filter(|(_, entry)| entry.phase == LaunchPhase::Running)
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_reservation_is_starting_and_blocks_a_second_launch() {
        let registry = LaunchRegistry::new();
        let token = registry
            .begin_launch("Pack")
            .expect("first launch reserves");
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Starting));
        assert_eq!(
            registry.begin_launch("Pack"),
            Err(LaunchBusy::Starting),
            "a second launch must not start while the first is preparing"
        );
        registry.release("Pack", token);
        assert!(!registry.is_busy("Pack"));
    }

    #[test]
    fn starting_is_not_reported_as_a_running_game() {
        let registry = LaunchRegistry::new();
        let _token = registry.begin_launch("Pack").expect("reserve");
        assert!(registry.running_instances().is_empty());
        let edit = LaunchRegistry::new();
        let _edit_token = edit.begin_edit("Pack").expect("reserve");
        assert!(edit.running_instances().is_empty());
    }

    #[test]
    fn mark_running_publishes_the_instance_and_keeps_it_reserved() {
        let registry = LaunchRegistry::new();
        let token = registry.begin_launch("Pack").expect("reserve");
        assert!(registry.mark_running("Pack", token));
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Running));
        assert_eq!(registry.running_instances(), vec!["Pack".to_string()]);
        assert!(!registry.mark_running("Pack", token), "already running");
    }

    #[test]
    fn a_stale_token_never_releases_a_newer_reservation() {
        let registry = LaunchRegistry::new();
        let first = registry.begin_launch("Pack").expect("reserve");
        assert!(registry.release("Pack", first));

        let second = registry.begin_launch("Pack").expect("reserve again");
        assert!(
            !registry.release("Pack", first),
            "the previous run must not free the new reservation"
        );
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Starting));
        assert!(registry.release("Pack", second));
    }

    #[test]
    fn stop_heal_only_clears_running_entries() {
        let registry = LaunchRegistry::new();
        let token = registry.begin_launch("Pack").expect("reserve");

        // Preparation in flight: healing must not free the reservation.
        assert!(!registry.clear_running("Pack"));
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Starting));

        // Spawned game whose bookkeeping is stale: healing removes it.
        assert!(registry.mark_running("Pack", token));
        assert!(registry.clear_running("Pack"));
        assert!(!registry.is_busy("Pack"));
    }

    #[test]
    fn editing_blocks_launch_until_released() {
        let registry = LaunchRegistry::new();
        let edit = registry.begin_edit("Pack").expect("reserve for edit");
        assert_eq!(registry.begin_launch("Pack"), Err(LaunchBusy::Editing));
        assert!(registry.release("Pack", edit));
        assert!(registry.begin_launch("Pack").is_ok());
    }

    #[test]
    fn adopting_a_pid_file_does_not_downgrade_a_starting_launch() {
        let registry = LaunchRegistry::new();
        let token = registry.begin_launch("Pack").expect("reserve");
        registry.adopt_running("Pack");
        assert_eq!(
            registry.phase("Pack"),
            Some(LaunchPhase::Starting),
            "a pid file from the previous run must not skip preparation"
        );
        assert!(registry.release("Pack", token));

        registry.adopt_running("Pack");
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Running));
        assert_eq!(registry.running_instances(), vec!["Pack".to_string()]);
    }

    #[test]
    fn a_concurrent_stop_sees_the_starting_phase() {
        let registry = LaunchRegistry::new();
        let token = registry.begin_launch("Pack").expect("reserve");

        std::thread::scope(|scope| {
            // Runs while the launch is still preparing, exactly like a Stop
            // click or a second Play arriving during the download phase.
            scope.spawn(|| {
                assert_eq!(registry.begin_launch("Pack"), Err(LaunchBusy::Starting));
                assert!(!registry.clear_running("Pack"));
                assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Starting));
            });
        });

        assert!(registry.mark_running("Pack", token));
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Running));
    }
}

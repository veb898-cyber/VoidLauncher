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
//! - [`LaunchPhase::Preparing`] — an import/install/creation owns the
//!   instance until its files are fully prepared and the on-disk readiness
//!   marker is retired. A launch must never start before that.
//!
//! Instance names are matched case-insensitively, because the instance
//! directory is a Windows path: `Pack` and `pack` address the same folder and
//! must address the same lifecycle entry too.
//!
//! Each reservation carries a [`LaunchToken`]: a reservation may only release
//! the entry it created, never an entry of a newer run of the same instance.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use crate::running_state::ProcessIdentity;

/// What the launcher is doing with one instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchPhase {
    Starting,
    Running,
    Editing,
    Preparing,
}

/// Why an instance cannot be reserved right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchBusy {
    Starting,
    Running,
    Editing,
    Preparing,
}

impl LaunchBusy {
    pub fn phase(self) -> LaunchPhase {
        match self {
            LaunchBusy::Starting => LaunchPhase::Starting,
            LaunchBusy::Running => LaunchPhase::Running,
            LaunchBusy::Editing => LaunchPhase::Editing,
            LaunchBusy::Preparing => LaunchPhase::Preparing,
        }
    }
}

impl From<LaunchPhase> for LaunchBusy {
    fn from(phase: LaunchPhase) -> Self {
        match phase {
            LaunchPhase::Starting => LaunchBusy::Starting,
            LaunchPhase::Running => LaunchBusy::Running,
            LaunchPhase::Editing => LaunchBusy::Editing,
            LaunchPhase::Preparing => LaunchBusy::Preparing,
        }
    }
}

/// Ownership token of one reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchToken(u64);

/// Token of an entry recovered from a persisted pid file. It never belongs to
/// a live reservation, so a `release` with a real token cannot remove it.
const ADOPTED: u64 = 0;

#[derive(Debug, Clone)]
struct Entry {
    /// Original spelling of the instance name. The map key is its lowercase
    /// form, but the UI must keep seeing the name as the user wrote it.
    name: String,
    token: u64,
    phase: LaunchPhase,
    /// Exact identity of the game process once it exists. Recorded together
    /// with the `Starting` -> `Running` transition, so the pid and the identity
    /// it was published with can never disagree.
    identity: Option<ProcessIdentity>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    next_token: u64,
}

/// One mutex for every per-instance phase: `Starting`, `Running`, `Preparing`
/// and the config-mutation lock are decided together, never by separate locks.
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

    /// Canonical map key. Windows instance directories are case-insensitive,
    /// so every lifecycle operation must agree that `Pack` and `pack` are the
    /// same instance.
    fn key(instance: &str) -> String {
        instance.to_lowercase()
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
        let key = Self::key(instance);
        let mut inner = self.lock();
        if let Some(entry) = inner.entries.get(&key) {
            return Err(entry.phase.into());
        }
        inner.next_token += 1;
        let token = LaunchToken(inner.next_token);
        inner.entries.insert(
            key,
            Entry {
                name: instance.to_string(),
                token: token.0,
                phase,
                identity: None,
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

    /// Reserve the instance for the whole import/install/creation lifecycle.
    ///
    /// This is what makes readiness atomic: a launch either wins the slot and
    /// the installer aborts before touching anything, or the installer wins
    /// and every launch is refused until preparation is complete.
    pub fn begin_prepare(&self, instance: &str) -> Result<LaunchToken, LaunchBusy> {
        self.reserve(instance, LaunchPhase::Preparing)
    }

    /// RAII form of [`Self::begin_prepare`]. The slot is released on every
    /// return path, including an early error while the directory is created.
    pub fn prepare_guard(&self, instance: &str) -> Result<PrepareGuard<'_>, LaunchBusy> {
        let token = self.begin_prepare(instance)?;
        Ok(PrepareGuard {
            registry: self,
            instance: instance.to_string(),
            token: Some(token),
        })
    }

    /// RAII form of [`Self::begin_edit`] for commands that hold the slot
    /// across an await. The token is released on every return path.
    pub fn edit_guard(&self, instance: &str) -> Result<EditGuard<'_>, LaunchBusy> {
        let token = self.begin_edit(instance)?;
        Ok(EditGuard {
            registry: self,
            instance: instance.to_string(),
            token: Some(token),
        })
    }

    /// Transition our own reservation `Starting` -> `Running` and record the
    /// identity of the spawned process in the same step.
    ///
    /// Returns `false` when the entry no longer belongs to this token (it was
    /// already released or replaced), in which case nothing is changed.
    pub fn mark_running(
        &self,
        instance: &str,
        token: LaunchToken,
        identity: Option<ProcessIdentity>,
    ) -> bool {
        let mut inner = self.lock();
        match inner.entries.get_mut(&Self::key(instance)) {
            Some(entry) if entry.token == token.0 && entry.phase == LaunchPhase::Starting => {
                entry.phase = LaunchPhase::Running;
                entry.identity = identity;
                true
            }
            _ => false,
        }
    }

    /// Release the entry this token created. A newer reservation of the same
    /// instance is never touched.
    pub fn release(&self, instance: &str, token: LaunchToken) -> bool {
        let key = Self::key(instance);
        let mut inner = self.lock();
        match inner.entries.get(&key) {
            Some(entry) if entry.token == token.0 => {
                inner.entries.remove(&key);
                true
            }
            _ => false,
        }
    }

    /// Forget a `Running` entry whose process is gone (stale in-memory record).
    ///
    /// `Starting`, `Preparing` and `Editing` entries are deliberately left
    /// alone: they belong to work that is still in flight.
    pub fn clear_running(&self, instance: &str) -> bool {
        let key = Self::key(instance);
        let mut inner = self.lock();
        match inner.entries.get(&key) {
            Some(entry) if entry.phase == LaunchPhase::Running => {
                inner.entries.remove(&key);
                true
            }
            _ => false,
        }
    }

    /// Like [`Self::clear_running`], but only when the entry still belongs to
    /// the exact process identity the caller probed. This prevents a lifecycle
    /// checker from deleting a newer run that reused the same instance name.
    pub fn clear_running_if_identity(&self, instance: &str, identity: &ProcessIdentity) -> bool {
        let key = Self::key(instance);
        let mut inner = self.lock();
        match inner.entries.get(&key) {
            Some(entry)
                if entry.phase == LaunchPhase::Running
                    && entry.identity.as_ref() == Some(identity) =>
            {
                inner.entries.remove(&key);
                true
            }
            _ => false,
        }
    }

    /// Register a game recovered from a persisted pid file, together with the
    /// identity that was verified for it. Never downgrades a live reservation:
    /// a launch that is still preparing stays `Starting`.
    pub fn adopt_running(&self, instance: &str, identity: Option<ProcessIdentity>) {
        let mut inner = self.lock();
        inner.entries.entry(Self::key(instance)).or_insert(Entry {
            name: instance.to_string(),
            token: ADOPTED,
            phase: LaunchPhase::Running,
            identity,
        });
    }

    /// Identity of the running game, if one was recorded. `cmd_stop_game`
    /// verifies exactly this process before terminating anything.
    pub fn process_identity(&self, instance: &str) -> Option<ProcessIdentity> {
        self.lock()
            .entries
            .get(&Self::key(instance))
            .and_then(|entry| entry.identity.clone())
    }

    pub fn phase(&self, instance: &str) -> Option<LaunchPhase> {
        self.lock()
            .entries
            .get(&Self::key(instance))
            .map(|entry| entry.phase)
    }

    /// Is the instance reserved for anything (launch preparation, running
    /// game, import/install, or a config mutation)?
    pub fn is_busy(&self, instance: &str) -> bool {
        self.phase(instance).is_some()
    }

    /// Names whose game process exists. `Starting`, `Preparing` and `Editing`
    /// are not reported: the UI must not present a preparing instance as a
    /// running one.
    pub fn running_instances(&self) -> Vec<String> {
        let inner = self.lock();
        let mut names: Vec<String> = inner
            .entries
            .values()
            .filter(|entry| entry.phase == LaunchPhase::Running)
            .map(|entry| entry.name.clone())
            .collect();
        names.sort();
        names
    }
}

/// Owns a `Preparing` reservation (import/install/creation) and releases
/// exactly its own token on drop.
pub struct PrepareGuard<'a> {
    registry: &'a LaunchRegistry,
    instance: String,
    token: Option<LaunchToken>,
}

impl Drop for PrepareGuard<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.registry.release(&self.instance, token);
        }
    }
}

/// Owns an `Editing` reservation and releases exactly its own token on drop.
pub struct EditGuard<'a> {
    registry: &'a LaunchRegistry,
    instance: String,
    token: Option<LaunchToken>,
}

impl Drop for EditGuard<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.registry.release(&self.instance, token);
        }
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
        assert!(registry.mark_running("Pack", token, None));
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Running));
        assert_eq!(registry.running_instances(), vec!["Pack".to_string()]);
        assert!(
            !registry.mark_running("Pack", token, None),
            "already running"
        );
    }

    #[test]
    fn the_running_transition_records_the_process_identity() {
        let registry = LaunchRegistry::new();
        let token = registry.begin_launch("Pack").expect("reserve");
        assert!(registry.process_identity("Pack").is_none());
        let identity = crate::running_state::ProcessIdentity {
            pid: 4242,
            exe: r"c:\java\bin\java.exe".to_string(),
            created: 134_512_345_678_901_234,
        };

        assert!(registry.mark_running("Pack", token, Some(identity.clone())));
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Running));
        assert_eq!(registry.process_identity("Pack"), Some(identity));
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
    fn identity_scoped_clear_does_not_remove_a_newer_run() {
        let registry = LaunchRegistry::new();
        let token = registry.begin_launch("Pack").expect("reserve");
        let first = ProcessIdentity {
            pid: 42,
            exe: "c:\\java\\bin\\java.exe".into(),
            created: 100,
        };
        let other = ProcessIdentity {
            created: 200,
            ..first.clone()
        };
        assert!(registry.mark_running("Pack", token, Some(first.clone())));
        assert!(
            !registry.clear_running_if_identity("Pack", &other),
            "a different process identity must not clear the entry"
        );
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Running));
        assert!(registry.clear_running_if_identity("Pack", &first));
        assert_eq!(registry.phase("Pack"), None);
    }

    #[test]
    fn stop_heal_only_clears_running_entries() {
        let registry = LaunchRegistry::new();
        let token = registry.begin_launch("Pack").expect("reserve");

        // Preparation in flight: healing must not free the reservation.
        assert!(!registry.clear_running("Pack"));
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Starting));

        // Spawned game whose bookkeeping is stale: healing removes it.
        assert!(registry.mark_running("Pack", token, None));
        assert!(registry.clear_running("Pack"));
        assert!(!registry.is_busy("Pack"));
    }

    #[test]
    fn edit_guard_releases_the_instance_on_drop() {
        let registry = LaunchRegistry::new();
        {
            let _guard = registry.edit_guard("Pack").expect("reserve for edit");
            assert_eq!(registry.begin_launch("Pack"), Err(LaunchBusy::Editing));
        }
        assert!(registry.begin_launch("Pack").is_ok());
    }

    #[test]
    fn independent_instances_can_be_edited_at_the_same_time() {
        let registry = LaunchRegistry::new();
        let _a = registry.edit_guard("A").expect("A edit");
        let _b = registry.edit_guard("B").expect("B edit");
        assert_eq!(registry.begin_launch("A"), Err(LaunchBusy::Editing));
        assert_eq!(registry.begin_launch("B"), Err(LaunchBusy::Editing));
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
        registry.adopt_running("Pack", None);
        assert_eq!(
            registry.phase("Pack"),
            Some(LaunchPhase::Starting),
            "a pid file from the previous run must not skip preparation"
        );
        assert!(registry.release("Pack", token));

        registry.adopt_running("Pack", None);
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

        assert!(registry.mark_running("Pack", token, None));
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Running));
    }

    #[test]
    fn prepare_blocks_launch_edit_and_a_second_import() {
        let registry = LaunchRegistry::new();
        let guard = registry.prepare_guard("Pack").expect("prepare slot");
        assert_eq!(registry.phase("Pack"), Some(LaunchPhase::Preparing));
        assert_eq!(registry.begin_launch("Pack"), Err(LaunchBusy::Preparing));
        assert_eq!(registry.begin_edit("Pack"), Err(LaunchBusy::Preparing));
        assert_eq!(registry.begin_prepare("Pack"), Err(LaunchBusy::Preparing));
        assert!(registry.running_instances().is_empty());
        drop(guard);
        assert!(!registry.is_busy("Pack"));
        assert!(registry.begin_launch("Pack").is_ok());
    }

    #[test]
    fn launch_or_edit_wins_the_prepare_race_exactly_once() {
        let registry = LaunchRegistry::new();
        let launch = registry.begin_launch("Pack").expect("launch wins");
        assert_eq!(registry.begin_prepare("Pack"), Err(LaunchBusy::Starting));
        assert!(registry.release("Pack", launch));

        let edit = registry.begin_edit("Pack").expect("edit wins");
        assert_eq!(registry.begin_prepare("Pack"), Err(LaunchBusy::Editing));
        assert!(registry.release("Pack", edit));

        let prepare = registry.begin_prepare("Pack").expect("prepare wins");
        assert!(registry.release("Pack", prepare));
    }

    #[test]
    fn instance_names_are_case_insensitive_like_windows_paths() {
        let registry = LaunchRegistry::new();
        let token = registry.begin_launch("Pack").expect("reserve");
        assert_eq!(registry.phase("pack"), Some(LaunchPhase::Starting));
        assert_eq!(registry.begin_prepare("PACK"), Err(LaunchBusy::Starting));
        assert_eq!(registry.begin_launch("pAcK"), Err(LaunchBusy::Starting));
        assert!(registry.release("PACK", token));
        assert!(!registry.is_busy("Pack"));

        // The display name keeps the spelling of the first reservation.
        let running = registry.begin_launch("MyPack").expect("reserve");
        assert!(registry.mark_running("mypack", running, None));
        assert_eq!(
            registry.running_instances(),
            vec!["MyPack".to_string()],
            "the UI must see the original spelling"
        );
    }
}

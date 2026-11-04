//! Persisted records of launched games and the identity of their processes.
//!
//! A pid is not an identity: Windows reuses pids, so a pid file left behind by
//! a game that has exited can point at a completely unrelated process. Acting
//! on such a record would show a foreign process as "running", block a
//! relaunch, or — worst — make `cmd_stop_game` kill an innocent process.
//!
//! Every record therefore stores the game's identity: the pid, the full path
//! of the executable and the exact creation time of the process, read from the
//! process handle with `GetProcessTimes` (100-ns `FILETIME` ticks). A record is
//! trusted only when the live process at that pid matches all three values.
//!
//! Records written by older launcher versions have no identity. They are
//! *unverified*: they are never reported as a running game, never grant the
//! right to kill anything, and are kept on disk until the process at that pid
//! either disappears or is proven to be a different process (pid reuse).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Exact creation time of a process: 100-ns ticks since 1601-01-01 UTC, as
/// returned by `GetProcessTimes`. No precision is lost.
pub type CreationTicks = u64;

/// Identity of a game process: what distinguishes it from any later process
/// that happened to receive the same pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// Full executable path, normalized for comparison (Windows paths are
    /// case-insensitive).
    pub exe: String,
    /// Creation time in `FILETIME` ticks.
    pub created: CreationTicks,
}

/// Snapshot of the process currently holding a pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityProbe {
    /// The process exists and its identity was read.
    Found(ProcessIdentity),
    /// No process holds this pid.
    Gone,
    /// The pid may be in use, but its identity cannot be read (access denied,
    /// unsupported platform, ...). Such a process is never trusted.
    Unavailable,
}

/// Source of process identities.
///
/// The launcher uses [`SystemProbe`]; tests use a fixed table so identity
/// checks can be exercised without inspecting or terminating real processes.
pub trait ProcessProbe {
    fn probe(&self, pid: u32) -> IdentityProbe;
}

/// Reads the real OS process table.
pub struct SystemProbe;

impl ProcessProbe for SystemProbe {
    fn probe(&self, pid: u32) -> IdentityProbe {
        read_identity(pid)
    }
}

/// Persisted record of a launched Minecraft process.
///
/// The launcher's in-memory launch state dies with the process (and is empty
/// after an app restart or an auto-update relaunch), which used to orphan a
/// still-running game: no "Running" badge, no way to stop it, and a second
/// launch of the same instance would start a second game on the same
/// directory. These pid files survive launcher restarts and are reconciled
/// against the live process table whenever the UI asks for the launch state
/// (and before every launch).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct RunningGame {
    pub instance: String,
    pub pid: u32,
    pub started_at: String,
    /// Executable path recorded at launch. Absent in records written by
    /// launcher versions that predate identity checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// `FILETIME` creation time as a decimal string, so no precision is lost
    /// when the record is read back. Absent in legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

impl RunningGame {
    /// Identity stored in the record, if it has a complete one.
    pub fn identity(&self) -> Option<ProcessIdentity> {
        let exe = self.exe.as_deref()?;
        let created = self.created.as_deref()?.parse::<u64>().ok()?;
        if exe.is_empty() {
            return None;
        }
        Some(ProcessIdentity {
            pid: self.pid,
            exe: exe.to_string(),
            created,
        })
    }
}

/// What a persisted record means for the live process table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    /// The pid is held by exactly the process the record describes.
    Confirmed,
    /// Nothing holds the pid: the recorded game has exited.
    Gone,
    /// The pid is held by a different process (pid reuse).
    Reused,
    /// The record cannot be verified (legacy format or unreadable identity).
    /// It must never be reported as running or used to kill anything.
    Unverified,
}

/// A legacy record has no identity, but it does have the wall-clock time at
/// which the launcher wrote it — milliseconds after the game process was
/// created. A live process created minutes away from that moment cannot be the
/// game the record describes, so the pid was reused.
///
/// This is used only to *reject* a record, never to confirm one, and the
/// tolerance is far wider than the write delay, so a genuine game is never
/// rejected. (Confirmation of new records uses the exact creation ticks.)
const LEGACY_START_TOLERANCE_SECS: i64 = 300;

fn legacy_start_mismatch(rec: &RunningGame, live: &ProcessIdentity) -> bool {
    let Ok(written) = chrono::DateTime::parse_from_rfc3339(&rec.started_at) else {
        // Unparseable timestamp: no way to reject, so the record stays
        // unverified (never confirmed, never killable).
        return false;
    };
    // FILETIME epoch is 1601-01-01; convert ticks to Unix seconds.
    let created_at = (live.created / 10_000_000) as i64 - 11_644_473_600;
    (created_at - written.timestamp()).abs() > LEGACY_START_TOLERANCE_SECS
}

/// Decide what a record means, given the live process table.
///
/// A record is confirmed only when the executable path and the exact creation
/// time both match. A mismatch proves pid reuse; an unreadable identity or a
/// legacy record can never be confirmed.
pub fn check_record(rec: &RunningGame, probe: &dyn ProcessProbe) -> RecordState {
    match probe.probe(rec.pid) {
        IdentityProbe::Gone => RecordState::Gone,
        IdentityProbe::Unavailable => RecordState::Unverified,
        IdentityProbe::Found(live) => match rec.identity() {
            Some(stored) => {
                if stored.exe == live.exe && stored.created == live.created {
                    RecordState::Confirmed
                } else {
                    RecordState::Reused
                }
            }
            None => {
                if legacy_start_mismatch(rec, &live) {
                    RecordState::Reused
                } else {
                    RecordState::Unverified
                }
            }
        },
    }
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

/// Record that `instance` is running as `pid`, together with the identity of
/// the process. A `None` identity (the OS refused to describe the child) is
/// written as a legacy-shaped record: it is treated as unverified rather than
/// trusted.
///
/// The record is replaced atomically: the JSON is written to a temporary file
/// in the same directory, flushed to disk and then moved over the record with
/// a single rename. On Windows that rename is
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, so there is no window in which the
/// record is missing, and a failed replacement leaves the previous record
/// untouched. The caller must not treat the launch as registered when this
/// returns an error.
pub fn write_pid_file(
    data_dir: &Path,
    instance: &str,
    pid: u32,
    identity: Option<&ProcessIdentity>,
) -> Result<(), String> {
    let dir = running_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;

    let rec = RunningGame {
        instance: instance.to_string(),
        pid,
        started_at: chrono::Local::now().to_rfc3339(),
        exe: identity.map(|i| i.exe.clone()),
        created: identity.map(|i| i.created.to_string()),
    };
    let json =
        serde_json::to_vec(&rec).map_err(|e| format!("cannot encode the running state: {}", e))?;

    // Same directory as the destination: the rename below never crosses a
    // volume. The `.tmp` extension keeps the file out of `scan_games`.
    let tmp = dir.join(format!(
        ".{}.{}-{}.tmp",
        crate::game_logs::sanitize_instance_name(instance),
        std::process::id(),
        unique_suffix()
    ));
    if let Err(e) = write_and_sync(&tmp, &json) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {}", tmp.display(), e));
    }

    let dst = pid_file(data_dir, instance);
    if let Err(e) = std::fs::rename(&tmp, &dst) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot replace {}: {}", dst.display(), e));
    }
    Ok(())
}

/// Write the complete file and flush it to the storage device before it is
/// moved into place. A crash between the two leaves only the (ignored) `.tmp`
/// file, never a truncated record.
fn write_and_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Distinct suffix for concurrent launches of different instances (and for
/// repeated attempts on the same one).
fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Best-effort: drop the pid file for `instance` (called on game exit / stop).
pub fn remove_pid_file(data_dir: &Path, instance: &str) {
    let _ = std::fs::remove_file(pid_file(data_dir, instance));
}

/// Records found on disk, split by whether their process identity could be
/// confirmed.
#[derive(Debug, Default, Clone)]
pub struct LiveScan {
    /// Games whose live process matches the recorded identity exactly.
    pub confirmed: Vec<RunningGame>,
    /// Live-but-unverifiable records (legacy files, unreadable identity).
    /// Kept on disk, never killed, never reported as running.
    pub unverified: Vec<RunningGame>,
}

/// Reconcile every persisted record with the live process table.
///
/// Confirmed records are returned as running games. Records whose pid is free
/// or belongs to a different process are deleted (the game is provably gone);
/// unverifiable records are kept and returned separately.
pub fn scan_games(data_dir: &Path) -> LiveScan {
    scan_games_with(data_dir, &SystemProbe)
}

fn scan_games_with(data_dir: &Path, probe: &dyn ProcessProbe) -> LiveScan {
    let mut scan = LiveScan::default();
    let Ok(entries) = std::fs::read_dir(running_dir(data_dir)) else {
        return scan;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let parsed: Option<RunningGame> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        let Some(rec) = parsed else {
            // Corrupt or unreadable — no usable record.
            let _ = std::fs::remove_file(&path);
            continue;
        };
        match check_record(&rec, probe) {
            RecordState::Confirmed => scan.confirmed.push(rec),
            RecordState::Unverified => scan.unverified.push(rec),
            RecordState::Gone => {
                let _ = std::fs::remove_file(&path);
            }
            RecordState::Reused => {
                tracing::info!(
                    target: "launch",
                    "Dropping stale pid record for '{}': pid {} now belongs to another process",
                    rec.instance,
                    rec.pid
                );
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    scan.confirmed.sort_by(|a, b| a.instance.cmp(&b.instance));
    scan.unverified.sort_by(|a, b| a.instance.cmp(&b.instance));
    scan
}

/// Names of instances whose game process is confirmed to be running.
pub fn scan_live_games(data_dir: &Path) -> Vec<RunningGame> {
    scan_games(data_dir).confirmed
}

/// Read the identity of the process currently holding `pid`.
pub fn read_identity(pid: u32) -> IdentityProbe {
    #[cfg(windows)]
    {
        match win::open(pid) {
            Ok(handle) => match identity_from_handle(handle.raw(), pid) {
                // A terminated process object can linger while another handle
                // keeps it alive; it is not a running game any more.
                Some(_) if win::is_running(handle.raw()) == Some(false) => IdentityProbe::Gone,
                Some(identity) => IdentityProbe::Found(identity),
                None => IdentityProbe::Unavailable,
            },
            Err(win::OpenError::NotFound) => IdentityProbe::Gone,
            Err(win::OpenError::Denied) => IdentityProbe::Unavailable,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        IdentityProbe::Unavailable
    }
}

/// Identity of a freshly spawned child. Read through the child handle, so it
/// describes exactly the process the launcher started.
pub fn identity_of_child(child: &std::process::Child) -> Option<ProcessIdentity> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        identity_from_handle(child.as_raw_handle() as _, child.id())
    }
    #[cfg(not(windows))]
    {
        let _ = child;
        None
    }
}

/// Path normalization used for identity comparison: full paths are compared
/// case-insensitively on Windows.
pub fn normalize_exe(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

#[cfg(windows)]
fn identity_from_handle(
    handle: windows_sys::Win32::Foundation::HANDLE,
    pid: u32,
) -> Option<ProcessIdentity> {
    let exe = win::image_path(handle)?;
    let created = win::creation_ticks(handle)?;
    Some(ProcessIdentity {
        pid,
        exe: normalize_exe(&exe),
        created,
    })
}

/// Minimal safe wrappers around the Win32 process API.
#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, FILETIME, HANDLE,
        STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum OpenError {
        /// No process holds this pid.
        NotFound,
        /// The process may exist, but it cannot be queried.
        Denied,
    }

    /// Owned process handle: closed on drop.
    pub struct ProcessHandle(HANDLE);

    impl ProcessHandle {
        pub fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Open a process for identity queries only (no terminate right needed).
    pub fn open(pid: u32) -> Result<ProcessHandle, OpenError> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if handle.is_null() {
            let err = unsafe { GetLastError() };
            return Err(if err == ERROR_INVALID_PARAMETER {
                OpenError::NotFound
            } else if err == ERROR_ACCESS_DENIED {
                OpenError::Denied
            } else {
                // Unknown failure: cannot prove anything about this pid.
                OpenError::Denied
            });
        }
        Ok(ProcessHandle(handle))
    }

    /// Creation time of the process behind `handle`, in `FILETIME` ticks.
    pub fn creation_ticks(handle: HANDLE) -> Option<u64> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let ok =
            unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
        if ok == 0 {
            return None;
        }
        Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }

    /// Full Win32 path of the executable behind `handle`.
    pub fn image_path(handle: HANDLE) -> Option<String> {
        // MAX_PATH is not a real limit for this API; 32k covers long paths.
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) };
        if ok == 0 || len == 0 {
            return None;
        }
        buf.truncate(len as usize);
        Some(String::from_utf16_lossy(&buf))
    }

    /// `Some(false)` when the process behind `handle` has exited.
    pub fn is_running(handle: HANDLE) -> Option<bool> {
        let mut code: u32 = 0;
        let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
        if ok == 0 {
            return None;
        }
        Some(code == STILL_ACTIVE as u32)
    }
}

/// Terminate a game process and its children, but only after re-verifying that
/// the process at `identity.pid` is still exactly the process described by
/// `identity`.
///
/// The verification handle is kept open across `taskkill`: while a handle to a
/// process object exists, Windows cannot recycle its pid, so `taskkill` cannot
/// hit a newer process that received the same pid after the game exited. This
/// closes the check-to-kill race (TOCTOU). Any doubt — unreadable identity, a
/// different executable, a different creation time — results in a refusal, not
/// in a kill.
pub fn kill_process_tree(identity: &ProcessIdentity) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let handle = match win::open(identity.pid) {
            Ok(handle) => handle,
            // Already exited: nothing to stop.
            Err(win::OpenError::NotFound) => return Ok(()),
            Err(win::OpenError::Denied) => {
                return Err(format!(
                    "Cannot verify process {} (access denied); refusing to stop it.",
                    identity.pid
                ));
            }
        };
        let live = identity_from_handle(handle.raw(), identity.pid).ok_or_else(|| {
            format!(
                "Cannot verify process {}; refusing to stop it.",
                identity.pid
            )
        })?;
        if &live != identity {
            return Err(format!(
                "Process {} is no longer the game that was started (pid reused); refusing to stop it.",
                identity.pid
            ));
        }

        // `handle` is still open here, so the pid cannot be reassigned.
        let status =
            taskkill_tree(identity.pid).map_err(|e| format!("Failed to run taskkill: {}", e))?;
        if status.success() {
            return Ok(());
        }
        // taskkill can fail because the process exited on its own between the
        // check and the call; that is not an error for our purposes.
        match win::is_running(handle.raw()) {
            Some(false) => Ok(()),
            _ => Err(format!("taskkill failed for pid {}", identity.pid)),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = identity;
        Err(
            "Stopping a game requires a verified process identity, which is not supported on this platform."
                .to_string(),
        )
    }
}

/// `taskkill /T` is used on Windows because the JVM may own helper processes
/// (crash reporter, AWT windows) that `Child::kill()` would leave behind.
#[cfg(target_os = "windows")]
fn taskkill_tree(pid: u32) -> std::io::Result<std::process::ExitStatus> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Process table substitute: no real process is ever inspected or killed.
    struct FakeProbe(HashMap<u32, IdentityProbe>);

    impl FakeProbe {
        fn new(entries: &[(u32, IdentityProbe)]) -> Self {
            Self(entries.iter().cloned().collect())
        }
    }

    impl ProcessProbe for FakeProbe {
        fn probe(&self, pid: u32) -> IdentityProbe {
            self.0.get(&pid).cloned().unwrap_or(IdentityProbe::Gone)
        }
    }

    const CREATED: u64 = 134_512_345_678_901_234;

    fn identity(pid: u32, exe: &str, created: u64) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            exe: normalize_exe(exe),
            created,
        }
    }

    fn record(pid: u32, identity: Option<&ProcessIdentity>) -> RunningGame {
        RunningGame {
            instance: "Pack".to_string(),
            pid,
            started_at: chrono::Local::now().to_rfc3339(),
            exe: identity.map(|i| i.exe.clone()),
            created: identity.map(|i| i.created.to_string()),
        }
    }

    #[test]
    fn confirmed_when_pid_exe_and_creation_time_match() {
        let game = identity(4242, r"C:\Program Files\Java\bin\java.exe", CREATED);
        let probe = FakeProbe::new(&[(4242, IdentityProbe::Found(game.clone()))]);
        assert_eq!(
            check_record(&record(4242, Some(&game)), &probe),
            RecordState::Confirmed
        );
        // Executable paths are compared case-insensitively.
        let rec = record(
            4242,
            Some(&identity(
                4242,
                r"c:\program files\java\bin\JAVA.EXE",
                CREATED,
            )),
        );
        assert_eq!(check_record(&rec, &probe), RecordState::Confirmed);
    }

    #[test]
    fn rejected_when_the_executable_differs() {
        let game = identity(4242, r"C:\Java\bin\java.exe", CREATED);
        let probe = FakeProbe::new(&[(
            4242,
            IdentityProbe::Found(identity(4242, r"C:\Windows\explorer.exe", CREATED)),
        )]);
        assert_eq!(
            check_record(&record(4242, Some(&game)), &probe),
            RecordState::Reused
        );
    }

    #[test]
    fn reused_pid_detected_by_creation_time() {
        let game = identity(4242, r"C:\Java\bin\java.exe", CREATED);
        let probe = FakeProbe::new(&[(
            4242,
            IdentityProbe::Found(identity(4242, r"C:\Java\bin\java.exe", CREATED + 1)),
        )]);
        assert_eq!(
            check_record(&record(4242, Some(&game)), &probe),
            RecordState::Reused,
            "a one-tick creation-time difference already means a different process"
        );
    }

    #[test]
    fn gone_when_nothing_holds_the_pid() {
        let game = identity(4242, r"C:\Java\bin\java.exe", CREATED);
        let probe = FakeProbe::new(&[]);
        assert_eq!(
            check_record(&record(4242, Some(&game)), &probe),
            RecordState::Gone
        );
    }

    #[test]
    fn unreadable_identity_is_unverified_and_never_confirmed() {
        let game = identity(4242, r"C:\Java\bin\java.exe", CREATED);
        let probe = FakeProbe::new(&[(4242, IdentityProbe::Unavailable)]);
        assert_eq!(
            check_record(&record(4242, Some(&game)), &probe),
            RecordState::Unverified
        );
    }

    #[test]
    fn legacy_record_without_identity_deserializes() {
        let json = r#"{"instance":"Old Pack","pid":4242,"started_at":"2026-01-02T03:04:05+03:00"}"#;
        let rec: RunningGame = serde_json::from_str(json).expect("legacy record must parse");
        assert_eq!(rec.instance, "Old Pack");
        assert_eq!(rec.pid, 4242);
        assert_eq!(rec.started_at, "2026-01-02T03:04:05+03:00");
        assert_eq!(rec.exe, None);
        assert_eq!(rec.created, None);
        assert!(rec.identity().is_none());
    }

    #[test]
    fn legacy_record_is_never_confirmed_even_when_the_process_looks_familiar() {
        let mut rec = record(4242, None);
        rec.started_at = "2026-01-02T03:04:05+03:00".to_string();
        // A process created within the tolerance window could still be the
        // game, but there is no identity to prove it: unverified, not trusted.
        let created_unix = chrono::DateTime::parse_from_rfc3339(&rec.started_at)
            .unwrap()
            .timestamp() as u64
            + 11_644_473_600;
        let probe = FakeProbe::new(&[(
            4242,
            IdentityProbe::Found(identity(
                4242,
                r"C:\Java\bin\java.exe",
                created_unix * 10_000_000,
            )),
        )]);
        assert_eq!(check_record(&rec, &probe), RecordState::Unverified);
    }

    #[test]
    fn legacy_record_whose_pid_was_reused_is_rejected() {
        let mut rec = record(4242, None);
        rec.started_at = "2026-01-02T03:04:05+03:00".to_string();
        // The live process was created two hours after the game started: the
        // pid was handed out again after our game exited.
        let created_unix = chrono::DateTime::parse_from_rfc3339(&rec.started_at)
            .unwrap()
            .timestamp() as u64
            + 11_644_473_600
            + 2 * 60 * 60;
        let probe = FakeProbe::new(&[(
            4242,
            IdentityProbe::Found(identity(
                4242,
                r"C:\Java\bin\java.exe",
                created_unix * 10_000_000,
            )),
        )]);
        assert_eq!(check_record(&rec, &probe), RecordState::Reused);
    }

    #[test]
    fn a_new_record_round_trips_the_process_identity() {
        let dir = temp_dir("identity_roundtrip");
        let own = std::process::id();
        let identity = read_identity(own);
        let IdentityProbe::Found(identity) = identity else {
            // Some sandboxes forbid querying even our own process; the fake
            // probe tests still cover the logic.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        assert!(identity.created > 0);

        write_pid_file(&dir, "Pack", own, Some(&identity)).expect("record written");
        let raw = std::fs::read_to_string(pid_file(&dir, "Pack")).expect("record written");
        let rec: RunningGame = serde_json::from_str(&raw).expect("record parses");
        assert_eq!(rec.pid, own);
        assert!(!rec.started_at.is_empty());
        assert_eq!(rec.identity(), Some(identity.clone()));

        let scan = scan_games_with(&dir, &SystemProbe);
        assert_eq!(scan.confirmed.len(), 1);
        assert_eq!(scan.confirmed[0].identity(), Some(identity));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Files that must never survive a write attempt: the atomic replacement
    /// writes into a `.tmp` file first.
    fn temp_leftovers(dir: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(running_dir(dir))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().and_then(|s| s.to_str()) != Some("json"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn write_pid_file_replaces_the_record_atomically() {
        let dir = temp_dir("atomic_write");
        let first = identity(111, r"C:\Java\bin\java.exe", 1_000);
        let second = identity(222, r"C:\Java\bin\java.exe", 2_000);

        write_pid_file(&dir, "Pack", 111, Some(&first)).expect("first write");
        let dst = pid_file(&dir, "Pack");
        assert!(dst.exists(), "the record must exist after the write");

        write_pid_file(&dir, "Pack", 222, Some(&second)).expect("replace an existing record");
        let rec: RunningGame =
            serde_json::from_str(&std::fs::read_to_string(&dst).expect("record readable"))
                .expect("record parses");
        assert_eq!(rec.pid, 222);
        assert_eq!(
            rec.identity(),
            Some(second),
            "the new identity replaced the old one"
        );
        assert!(!rec.started_at.is_empty(), "started_at is still written");
        assert!(
            temp_leftovers(&dir).is_empty(),
            "no temporary files may remain after a successful write: {:?}",
            temp_leftovers(&dir)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(windows)]
    // Clearing the read-only attribute is meaningful on Windows
    // (`MoveFileExW` refuses to replace a read-only file); the call is the
    // documented way to undo what the test set up.
    #[allow(clippy::permissions_set_readonly_false)]
    fn a_failed_replacement_keeps_the_previous_good_record() {
        let dir = temp_dir("atomic_keep");
        let first = identity(111, r"C:\Java\bin\java.exe", 1_000);
        write_pid_file(&dir, "Pack", 111, Some(&first)).expect("first write");
        let dst = pid_file(&dir, "Pack");

        // A read-only destination cannot be replaced; Windows fails the move.
        let mut perms = std::fs::metadata(&dst).expect("stat").permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&dst, perms).expect("make read-only");

        let second = identity(222, r"C:\Java\bin\java.exe", 2_000);
        let err = write_pid_file(&dir, "Pack", 222, Some(&second))
            .expect_err("replacing a read-only record must fail");
        assert!(err.contains("cannot replace"), "got: {err}");

        // The previous record is intact and still describes the old process.
        let rec: RunningGame =
            serde_json::from_str(&std::fs::read_to_string(&dst).expect("old record still there"))
                .expect("old record still parses");
        assert_eq!(rec.pid, 111);
        assert_eq!(rec.identity(), Some(first));

        // Whatever went wrong, the partial file must not be left behind.
        assert!(
            temp_leftovers(&dir).is_empty(),
            "temporary files must be cleaned up: {:?}",
            temp_leftovers(&dir)
        );

        let mut perms = std::fs::metadata(&dst).expect("stat").permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&dst, perms).expect("clear read-only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_reports_an_error_when_the_state_directory_is_unusable() {
        let dir = temp_dir("write_error");
        std::fs::create_dir_all(&dir).expect("temp dir");
        // `running` exists as a regular file: the record cannot be placed.
        std::fs::write(running_dir(&dir), b"not a directory").expect("blocking file");

        let err = write_pid_file(&dir, "Pack", 111, None).expect_err("must fail");
        assert!(err.contains("cannot create"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_move_cleans_up_the_temporary_file() {
        let dir = temp_dir("atomic_rename_fail");
        std::fs::create_dir_all(running_dir(&dir).join("Pack.json")).expect("blocking directory");

        let err = write_pid_file(&dir, "Pack", 111, None)
            .expect_err("a directory cannot be replaced by a file");
        assert!(err.contains("cannot replace"), "got: {err}");
        assert!(
            temp_leftovers(&dir).is_empty(),
            "temporary files must be cleaned up: {:?}",
            temp_leftovers(&dir)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_ignores_an_interrupted_temporary_file() {
        let dir = temp_dir("atomic_scan");
        let rdir = running_dir(&dir);
        std::fs::create_dir_all(&rdir).expect("temp dir");
        // A crash between create and rename leaves this behind; it must never
        // be mistaken for a record (or deleted as a corrupt one).
        let tmp = rdir.join(".Pack.1234-5678.tmp");
        std::fs::write(&tmp, b"{\"instance\":\"Pack\"").expect("partial temp file");

        let scan = scan_games_with(&dir, &FakeProbe::new(&[]));
        assert!(scan.confirmed.is_empty());
        assert!(scan.unverified.is_empty());
        assert!(
            tmp.exists(),
            "a temporary file is not a record and is left alone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_splits_confirmed_and_unverified_and_cleans_stale_records() {
        let dir = temp_dir("scan_split");
        let rdir = running_dir(&dir);
        std::fs::create_dir_all(&rdir).unwrap();

        let live = identity(100, r"C:\Java\bin\java.exe", CREATED);
        let confirmed = record(100, Some(&live));
        let reused = RunningGame {
            pid: 200,
            instance: "Reused".to_string(),
            ..record(200, Some(&identity(200, r"C:\Java\bin\java.exe", CREATED)))
        };
        let gone = RunningGame {
            pid: 300,
            instance: "Gone".to_string(),
            ..record(300, Some(&identity(300, r"C:\Java\bin\java.exe", CREATED)))
        };
        let legacy = RunningGame {
            pid: 400,
            instance: "Legacy".to_string(),
            started_at: "2026-01-02T03:04:05+03:00".to_string(),
            exe: None,
            created: None,
        };
        let created_400 = (chrono::DateTime::parse_from_rfc3339(&legacy.started_at)
            .unwrap()
            .timestamp() as u64
            + 11_644_473_600)
            * 10_000_000;

        for rec in [&confirmed, &reused, &gone, &legacy] {
            std::fs::write(
                rdir.join(format!("{}.json", rec.instance)),
                serde_json::to_string(rec).unwrap(),
            )
            .unwrap();
        }

        let probe = FakeProbe::new(&[
            (100, IdentityProbe::Found(live.clone())),
            (
                200,
                IdentityProbe::Found(identity(200, r"C:\Windows\explorer.exe", CREATED)),
            ),
            (
                400,
                IdentityProbe::Found(identity(400, r"C:\Java\bin\java.exe", created_400)),
            ),
        ]);

        let scan = scan_games_with(&dir, &probe);
        assert_eq!(scan.confirmed.len(), 1);
        assert_eq!(scan.confirmed[0].instance, "Pack");
        assert_eq!(scan.unverified.len(), 1);
        assert_eq!(scan.unverified[0].instance, "Legacy");

        assert!(
            !rdir.join("Reused.json").exists(),
            "reused pid record is dropped"
        );
        assert!(!rdir.join("Gone.json").exists(), "dead record is dropped");
        assert!(
            rdir.join("Legacy.json").exists(),
            "an unverifiable record is kept, not silently deleted"
        );
        assert!(rdir.join("Pack.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_live_games_excludes_unverified_records() {
        let dir = temp_dir("scan_unverified");
        let rdir = running_dir(&dir);
        std::fs::create_dir_all(&rdir).unwrap();
        let legacy = RunningGame {
            instance: "Legacy".to_string(),
            pid: 400,
            started_at: "2026-01-02T03:04:05+03:00".to_string(),
            exe: None,
            created: None,
        };
        std::fs::write(
            rdir.join("Legacy.json"),
            serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();
        // The process was created at the moment the record was written, so it
        // could be the game — but without an identity it is still unverified.
        let created = (chrono::DateTime::parse_from_rfc3339(&legacy.started_at)
            .unwrap()
            .timestamp() as u64
            + 11_644_473_600)
            * 10_000_000;
        let probe = FakeProbe::new(&[(
            400,
            IdentityProbe::Found(identity(400, r"C:\Java\bin\java.exe", created)),
        )]);
        let scan = scan_games_with(&dir, &probe);
        assert!(
            scan.confirmed.is_empty(),
            "legacy records are not running games"
        );
        assert_eq!(scan.unverified.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_pid_file_is_removed_without_panic() {
        let dir = temp_dir("corrupt");
        let rdir = running_dir(&dir);
        std::fs::create_dir_all(&rdir).unwrap();
        std::fs::write(rdir.join("broken.json"), "{not json").unwrap();

        let scan = scan_games_with(&dir, &FakeProbe::new(&[]));
        assert!(scan.confirmed.is_empty());
        assert!(scan.unverified.is_empty());
        assert!(!rdir.join("broken.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_record_never_grants_a_kill() {
        // A legacy record carries no identity, so there is nothing the Stop
        // path could verify — and `kill_process_tree` only ever accepts a
        // verified identity.
        let rec = record(4242, None);
        assert!(rec.identity().is_none());
    }

    #[test]
    fn normalize_exe_is_case_and_separator_insensitive() {
        assert_eq!(
            normalize_exe(r"C:/Program Files/Java/bin/JAVA.EXE"),
            normalize_exe(r"c:\program files\java\bin\java.exe")
        );
    }

    #[test]
    #[cfg(windows)]
    fn kill_process_tree_refuses_when_the_identity_does_not_match() {
        let mut child = spawn_test_process();
        let identity = identity_of_child(&child).expect("identity of our own child");
        let mut forged = identity.clone();
        forged.created = forged.created.wrapping_add(1);

        let err =
            kill_process_tree(&forged).expect_err("a mismatched identity must never be killed");
        assert!(err.contains("refusing"), "got: {err}");
        assert!(
            matches!(read_identity(child.id()), IdentityProbe::Found(_)),
            "the process must still be alive after a refused stop"
        );

        // The verified identity can stop it.
        kill_process_tree(&identity).expect("verified stop succeeds");
        let _ = child.wait();
    }

    #[test]
    #[cfg(windows)]
    fn kill_process_tree_never_touches_a_foreign_process_after_pid_reuse() {
        // A process that has already exited leaves a stale identity behind.
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .expect("spawn helper");
        let stale = identity_of_child(&child).expect("identity");
        let _ = child.wait();

        // Either the pid is free (no-op) or it was reused — but a reused pid
        // can never be killed through the old identity.
        match kill_process_tree(&stale) {
            Ok(()) => {}
            Err(err) => assert!(err.contains("refusing"), "unexpected error: {err}"),
        }
    }

    #[cfg(windows)]
    fn spawn_test_process() -> std::process::Child {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .args(["/C", "ping", "-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawn a disposable test process")
    }

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let unique = format!(
            "vl_running_{prefix}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before unix epoch")
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }
}

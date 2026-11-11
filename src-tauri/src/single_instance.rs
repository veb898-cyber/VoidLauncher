//! One launcher process per Windows session.
//!
//! `LaunchRegistry`, `EditGuard` and the per-path download locks are all
//! process-local. They cannot coordinate two independent `voidlauncher.exe`
//! processes that share the same data directory. A named kernel mutex is
//! acquired before the logger, config or any instance file is touched; the
//! second process exits without doing any work.
//!
//! The kernel owns the mutex object, so an abnormal exit (panic, task
//! manager, crash) closes the handle automatically. There is no stale lock
//! file to clean up. Multiple Tauri windows inside the first process are not
//! affected: the guard is per process, not per window.

use std::fmt;

/// Per-session namespace: two Windows users get their own data directories
/// and must not block each other. Two elevations in one session still share
/// the name, which is what we want for the same data directory.
pub const APP_MUTEX_NAME: &str = r"Local\VoidLauncher.SingleInstance";

pub struct SingleInstanceGuard {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[derive(Debug)]
pub enum AcquireError {
    AlreadyRunning,
    #[cfg(windows)]
    Windows(std::io::Error),
}

impl fmt::Display for AcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "VoidLauncher is already running"),
            #[cfg(windows)]
            Self::Windows(error) => write!(f, "{error}"),
        }
    }
}

/// Acquire the single-instance mutex. On success the guard must stay alive for
/// the whole process; dropping it releases the instance slot.
#[cfg(windows)]
pub fn acquire(name: &str) -> Result<SingleInstanceGuard, AcquireError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let wide: Vec<u16> = std::ffi::OsStr::new(name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // No security attributes: the default descriptor is limited to the
    // current user/session, and the handle is not inheritable by Minecraft.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(AcquireError::Windows(std::io::Error::last_os_error()));
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return Err(AcquireError::AlreadyRunning);
    }
    Ok(SingleInstanceGuard { handle })
}

#[cfg(not(windows))]
pub fn acquire(_name: &str) -> Result<SingleInstanceGuard, AcquireError> {
    Ok(SingleInstanceGuard {})
}

/// Startup variant with a short hand-off window. Tauri's updater relaunch
/// spawns the replacement process before the old process calls `exit(0)`;
/// the replacement must be allowed to wait for the old handle to close instead
/// of showing "already running" and leaving the user with no launcher.
/// A normal second launch waits the same short window and then exits.
pub fn acquire_with_handoff(name: &str) -> Result<SingleInstanceGuard, AcquireError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match acquire(name) {
            Err(AcquireError::AlreadyRunning) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

#[cfg(windows)]
pub fn show_already_running() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    let text: Vec<u16> = "VoidLauncher уже запущен.\nVoidLauncher is already running.\0"
        .encode_utf16()
        .collect();
    let title: Vec<u16> = "VoidLauncher\0".encode_utf16().collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(not(windows))]
pub fn show_already_running() {}

#[cfg(windows)]
pub fn show_startup_error(detail: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let text: Vec<u16> = format!(
        "VoidLauncher не смог проверить, запущен ли другой экземпляр.\n\
         VoidLauncher could not verify the single-instance state.\n\n{detail}\0"
    )
    .encode_utf16()
    .collect();
    let title: Vec<u16> = "VoidLauncher\0".encode_utf16().collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
pub fn show_startup_error(_detail: &str) {}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn unique_name() -> String {
        format!(
            r"Local\VoidLauncher.Test.{}.{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        )
    }

    /// Runs only in the child process spawned by the test below. Without the
    /// environment variable it is a no-op in the normal test pass.
    #[test]
    fn child_holds_instance() {
        let Ok(name) = std::env::var("VL_SINGLE_INSTANCE_TEST_NAME") else {
            return;
        };
        let _guard = acquire(&name).expect("child must acquire the mutex");
        if let Ok(ready) = std::env::var("VL_SINGLE_INSTANCE_TEST_READY") {
            std::fs::write(ready, b"ready").expect("child must signal readiness");
        }
        std::thread::sleep(Duration::from_secs(30));
    }

    struct ChildGuard(std::process::Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn spawn_holder(name: &str, ready: &std::path::Path) -> ChildGuard {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        ChildGuard(
            std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("single_instance::tests::child_holds_instance")
                .env("VL_SINGLE_INSTANCE_TEST_NAME", name)
                .env("VL_SINGLE_INSTANCE_TEST_READY", ready)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .expect("spawn the child test process"),
        )
    }

    fn wait_until_ready(ready: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready.exists() {
            if Instant::now() > deadline {
                panic!("the child process did not acquire the mutex");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_second_process_is_blocked_and_a_crash_releases_the_lock() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_single_instance_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ready = dir.join("ready");
        let name = unique_name();

        let mut child = spawn_holder(&name, &ready);
        wait_until_ready(&ready);

        match acquire(&name) {
            Err(AcquireError::AlreadyRunning) => {}
            Ok(_) => panic!("a second process must not acquire the same mutex"),
            Err(AcquireError::Windows(error)) => {
                panic!("unexpected mutex error: {error}")
            }
        }

        // Abnormal exit: TerminateProcess closes the kernel handle without
        // running Rust destructors.
        child.0.kill().expect("kill the child");
        child.0.wait().expect("reap the child");

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match acquire(&name) {
                Ok(guard) => {
                    drop(guard);
                    break;
                }
                Err(AcquireError::AlreadyRunning) => {
                    if Instant::now() > deadline {
                        panic!("the kernel did not release the mutex after a crash");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(AcquireError::Windows(error)) => {
                    panic!("unexpected mutex error: {error}")
                }
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_relaunched_process_waits_for_the_old_owner() {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_single_handoff_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ready = dir.join("ready");
        let name = unique_name();

        let mut child = spawn_holder(&name, &ready);
        wait_until_ready(&ready);

        let waiter_name = name.clone();
        let waiter = std::thread::spawn(move || match acquire_with_handoff(&waiter_name) {
            Ok(guard) => drop(guard),
            Err(error) => panic!("the replacement did not take over: {error}"),
        });
        // Give the waiter time to enter the retry loop, then terminate the
        // owner the same way Tauri's updater relaunch does (old process exits
        // while the replacement is already running).
        std::thread::sleep(Duration::from_millis(150));
        child.0.kill().expect("kill the child");
        child.0.wait().expect("reap the child");

        waiter.join().expect("waiter thread");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

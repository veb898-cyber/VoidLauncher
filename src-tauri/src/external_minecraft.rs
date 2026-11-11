//! Best-effort detection of Minecraft processes that were not launched by
//! VoidLauncher.
//!
//! The launcher's own games are tracked exactly (PID + executable + creation
//! time) in `running_state`. A game started by another launcher has no such
//! record, so this module inspects the live process list for Java processes
//! whose command line looks like Minecraft and whose `--gameDir` (or, when the
//! argument is absent, current working directory) is the instance's game
//! directory.
//!
//! This is deliberately conservative:
//! - only Java executables are considered;
//! - at least two launch flags are required, including one Minecraft-specific
//!   flag (`--gameDir`, `--assetIndex`, `--accessToken`, ...);
//! - the candidate directory must normalize to the exact instance directory;
//! - the process must still have a verifiable identity at the moment of the
//!   match.
//!
//! If the command line or working directory cannot be read (protected process,
//! permissions), the process is ignored rather than guessed. The caller keeps
//! the existing launcher-only protection and the OCC check.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::running_state::{read_identity, IdentityProbe, ProcessIdentity};

/// External Minecraft process that appears to use the requested game dir.
#[derive(Debug, Clone)]
pub struct ExternalGame {
    pub game_dir: PathBuf,
    pub identity: ProcessIdentity,
}

/// Injectable probe so the server commands can exercise the block rule without
/// depending on whatever Java processes happen to run on the build machine.
pub trait ExternalGameProbe {
    fn find_for_game_dir(&self, game_dir: &Path) -> Option<ExternalGame>;
}

/// Reads the real process table.
pub struct SystemExternalGameProbe;

/// True when this instance is in use either by the launcher's own tracked
/// process or by a game started outside it. Repair calls this again between
/// phases: the first check alone cannot cover a game that starts while a
/// large download is already running.
pub fn instance_is_running(
    config: &crate::config::AppConfig,
    instance: &crate::instances::Instance,
) -> bool {
    instance_is_running_with(&SystemExternalGameProbe, config, instance)
}

fn instance_is_running_with(
    probe: &dyn ExternalGameProbe,
    config: &crate::config::AppConfig,
    instance: &crate::instances::Instance,
) -> bool {
    crate::running_state::scan_live_games(&config.data_dir)
        .iter()
        .any(|game| game.instance == instance.name)
        || probe
            .find_for_game_dir(&instance.minecraft_dir(&config.instances_dir()))
            .is_some()
}

impl ExternalGameProbe for SystemExternalGameProbe {
    fn find_for_game_dir(&self, game_dir: &Path) -> Option<ExternalGame> {
        let wanted = normalize_game_dir(game_dir);
        for (pid, process) in refresh_processes().processes() {
            if !looks_like_java(process.exe()) || !looks_like_minecraft(process.cmd()) {
                continue;
            }

            let candidate = match game_dir_from_args(process.cmd())
                .or_else(|| process.cwd().map(Path::to_path_buf))
            {
                Some(candidate) => candidate,
                None => continue,
            };
            // `--gameDir` is normally absolute. A relative value is relative to
            // the game's own working directory, not to the launcher's.
            let candidate = if candidate.is_relative() {
                match process.cwd() {
                    Some(cwd) => cwd.join(candidate),
                    None => continue,
                }
            } else {
                candidate
            };
            if normalize_game_dir(&candidate) != wanted {
                continue;
            }

            // A PID is not an identity. Only report a match when the exact
            // process identity can still be read for that PID.
            let IdentityProbe::Found(identity) = read_identity(pid.as_u32()) else {
                continue;
            };
            if process
                .exe()
                .and_then(|exe| exe.to_str())
                .map(crate::running_state::normalize_exe)
                .is_some_and(|exe| exe != identity.exe)
            {
                // The pid was reused between enumeration and the identity
                // read. Never treat that as the game we just inspected.
                continue;
            }
            // The executable is identical for every Java process, so also
            // compare the creation second. This catches a pid reused by a
            // different java.exe between enumeration and the identity read.
            let enumerated_created = process.start_time();
            if enumerated_created != 0 && enumerated_created != creation_seconds(identity.created) {
                continue;
            }

            return Some(ExternalGame {
                game_dir: candidate,
                identity,
            });
        }
        None
    }
}

fn refresh_processes() -> System {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet),
    );
    system
}

fn looks_like_java(exe: Option<&Path>) -> bool {
    exe.and_then(|path| path.file_name())
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            let lower = name.to_ascii_lowercase();
            lower == "java.exe" || lower == "javaw.exe"
        })
}

/// Minecraft's main class and launch arguments. Requiring two flags avoids
/// treating an unrelated Java application that happens to use one generic
/// option as the game. At least one flag must be Minecraft-specific:
/// `--version` and `--uuid` alone are common in unrelated Java tooling.
const MINECRAFT_FLAGS: &[&str] = &[
    "--gameDir",
    "--assetIndex",
    "--version",
    "--versionType",
    "--uuid",
    "--accessToken",
    "--userProperties",
    "--userType",
];

const MINECRAFT_SPECIFIC_FLAGS: &[&str] = &[
    "--gameDir",
    "--assetIndex",
    "--versionType",
    "--accessToken",
    "--userProperties",
    "--userType",
];

fn arg_is_flag(arg: &OsStr, flag: &str) -> bool {
    let value = arg.to_string_lossy();
    value == flag || value.starts_with(&format!("{flag}="))
}

fn looks_like_minecraft(args: &[std::ffi::OsString]) -> bool {
    let count = args
        .iter()
        .filter(|arg| MINECRAFT_FLAGS.iter().any(|flag| arg_is_flag(arg, flag)))
        .count();
    count >= 2
        && args.iter().any(|arg| {
            MINECRAFT_SPECIFIC_FLAGS
                .iter()
                .any(|flag| arg_is_flag(arg, flag))
        })
}

/// `--gameDir <path>` or `--gameDir=<path>`.
fn game_dir_from_args(args: &[std::ffi::OsString]) -> Option<PathBuf> {
    for (index, arg) in args.iter().enumerate() {
        if arg == OsStr::new("--gameDir") {
            return args.get(index + 1).map(PathBuf::from);
        }
        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--gameDir="))
        {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

/// Convert Windows `FILETIME` ticks (100 ns since 1601-01-01) to Unix seconds,
/// the unit used by `sysinfo`'s `Process::start_time`.
fn creation_seconds(ticks: u64) -> u64 {
    (ticks / 10_000_000).saturating_sub(11_644_473_600)
}

/// Comparison form for Windows paths: canonical when possible, slash-normalized,
/// case-insensitive and without a trailing separator.
pub fn normalize_game_dir(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut value = canonical.to_string_lossy().replace('/', "\\");
    if let Some(stripped) = value.strip_prefix(r"\\?\") {
        value = stripped.to_string();
    }
    let trimmed = value.trim_end_matches('\\').to_ascii_lowercase();
    if trimmed.is_empty() {
        value.to_ascii_lowercase()
    } else {
        trimmed
    }
}

#[cfg(test)]
pub(crate) fn inspect_process_for_test(pid: u32) -> Option<(String, Vec<String>, Option<PathBuf>)> {
    let system = refresh_processes();
    let process = system.process(sysinfo::Pid::from_u32(pid))?;
    Some((
        process.exe()?.to_string_lossy().to_string(),
        process
            .cmd()
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect(),
        process.cwd().map(Path::to_path_buf),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn recognizes_minecraft_arguments() {
        assert!(looks_like_minecraft(&args(&[
            "--gameDir",
            r"C:\games\pack",
            "--assetIndex",
            "26.3",
            "--version",
            "26.3",
            "--uuid",
            "abc",
        ])));
        assert!(looks_like_minecraft(&args(&[
            "--accessToken",
            "token",
            "--uuid",
            "abc",
        ])));
        assert!(!looks_like_minecraft(&args(&["-jar", "server.jar"])));
        assert!(!looks_like_minecraft(&args(&["--version"])));
        // Two generic flags are common outside Minecraft; they must not be
        // enough without a Minecraft-specific argument.
        assert!(!looks_like_minecraft(&args(&[
            "--version",
            "1.0",
            "--uuid",
            "abc",
        ])));
    }

    #[test]
    fn an_external_game_appearing_after_the_first_check_is_seen_by_the_second() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct ToggleProbe(AtomicBool);

        impl ExternalGameProbe for ToggleProbe {
            fn find_for_game_dir(&self, game_dir: &Path) -> Option<ExternalGame> {
                self.0.load(Ordering::SeqCst).then(|| ExternalGame {
                    game_dir: game_dir.to_path_buf(),
                    identity: ProcessIdentity {
                        pid: 4242,
                        exe: r"c:\java\bin\javaw.exe".to_string(),
                        created: 1,
                    },
                })
            }
        }

        let dir = std::env::temp_dir().join(format!(
            "vl-external-toggle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::AppConfig {
            data_dir: dir.clone(),
            ..Default::default()
        };
        let instance = crate::instances::Instance::new(
            "Toggle",
            "1.20.1",
            crate::instances::LoaderType::Vanilla,
            None,
        );
        let probe = ToggleProbe(AtomicBool::new(false));
        assert!(!instance_is_running_with(&probe, &config, &instance));
        probe.0.store(true, Ordering::SeqCst);
        assert!(
            instance_is_running_with(&probe, &config, &instance),
            "the second phase check must see a game that appeared after the first"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_instance_is_busy_when_its_pid_file_is_live() {
        let dir = std::env::temp_dir().join(format!(
            "vl-external-busy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::AppConfig {
            data_dir: dir.clone(),
            ..Default::default()
        };
        let instance = crate::instances::Instance::new(
            "Busy",
            "1.20.1",
            crate::instances::LoaderType::Vanilla,
            None,
        );
        assert!(!instance_is_running(&config, &instance));
        if let crate::running_state::IdentityProbe::Found(identity) =
            crate::running_state::read_identity(std::process::id())
        {
            crate::running_state::write_pid_file(
                &config.data_dir,
                &instance.name,
                std::process::id(),
                Some(&identity),
            )
            .unwrap();
            assert!(instance_is_running(&config, &instance));
            crate::running_state::remove_pid_file(&config.data_dir, &instance.name);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extracts_game_dir_in_both_forms() {
        assert_eq!(
            game_dir_from_args(&args(&["--gameDir", r"C:\games\pack", "--version", "26.3"])),
            Some(PathBuf::from(r"C:\games\pack"))
        );
        assert_eq!(
            game_dir_from_args(&args(&[r"--gameDir=C:\games\pack", "--version", "26.3"])),
            Some(PathBuf::from(r"C:\games\pack"))
        );
        assert_eq!(game_dir_from_args(&args(&["--version", "26.3"])), None);
    }

    #[test]
    fn converts_windows_creation_ticks_to_unix_seconds() {
        assert_eq!(creation_seconds(11_644_473_600 * 10_000_000), 0);
        assert_eq!(
            creation_seconds((11_644_473_600 + 1_700_000_000) * 10_000_000 + 9_999_999),
            1_700_000_000
        );
    }

    #[test]
    fn normalizes_case_separators_and_trailing_separator() {
        let base = std::env::temp_dir().join(format!(
            "vl-external-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let with_slash = PathBuf::from(format!("{}\\", base.display()));
        assert_eq!(normalize_game_dir(&base), normalize_game_dir(&with_slash));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[test]
    fn reads_real_process_command_line_and_cwd() {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let dir = std::env::temp_dir().join(format!(
            "vl-external-proc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawn test process");
        let pid = child.id();
        let snapshot = inspect_process_for_test(pid);
        let _ = child.kill();
        let _ = child.wait();
        let (exe, cmd, cwd) = snapshot.expect("test process must be enumerable");
        assert!(exe.to_ascii_lowercase().ends_with("ping.exe"), "exe: {exe}");
        assert!(
            cmd.iter().any(|arg| arg.eq_ignore_ascii_case("-n")),
            "cmd: {cmd:?}"
        );
        assert_eq!(
            cwd.map(|value| normalize_game_dir(&value)),
            Some(normalize_game_dir(&dir))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Recovery planning and the small set of crash fixes that are safe to apply
//! from the launcher itself.
//!
//! The planner only offers actions that follow from the evidence:
//! - a jar named by a fatal mod/mixin/compatibility signal may be disabled;
//! - a driver crash may open the vendor page;
//! - an unknown failure may run the existing file verifier as a fallback.
//!
//! Installation, version pinning and loader updates deliberately stay on
//! their own screens; a crash dialog must not silently rewrite the instance.

use crate::config::AppConfig;
use crate::instances::Instance;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CrashAction {
    /// Stable kind for the frontend: `disable_mod`, `open_url`, `repair_files`.
    pub kind: String,
    /// Only parameter of the action. Re-validated by the backend before use.
    pub arg: String,
    /// `danger` | `secondary`.
    pub tone: String,
}

fn action(kind: &str, arg: impl Into<String>, tone: &str) -> CrashAction {
    CrashAction {
        kind: kind.to_string(),
        arg: arg.into(),
        tone: tone.to_string(),
    }
}

/// Plan the actions shown under a crash report. `file` is a jar already
/// resolved against this instance's mods folder.
pub fn plan(kind: &str, file: Option<&str>, text: &str) -> Vec<CrashAction> {
    let mut actions = Vec::new();
    if let Some(file) = file {
        if matches!(
            kind,
            "mod_crash" | "mod_conflict" | "wrong_loader" | "wrong_game"
        ) {
            actions.push(action("disable_mod", file, "danger"));
        }
    }
    if kind == "graphics_driver" {
        if let Some(url) = crate::crash_signals::driver_url(text) {
            actions.push(action("open_url", url, "secondary"));
        }
    }
    // Repair is a fallback for a verdict that did not identify a mod. It is
    // deliberately absent for dependency/loader conflicts: re-downloading the
    // client cannot fix those.
    if actions.is_empty() && matches!(kind, "unknown" | "missing_class") {
        actions.push(action("repair_files", "", "secondary"));
    }
    actions
}

/// The only URLs a crash action may open. Check the parsed host, not a
/// substring: `https://www.nvidia.com.evil.example` and
/// `https://evil.example/?nvidia.com` must both fail. HTTPS is mandatory.
pub fn validate_official_url(url: &str) -> Result<(), String> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .ok_or_else(|| "Only HTTPS driver pages are allowed".to_string())?;
    if rest.contains('@') {
        return Err("User info in a driver URL is not allowed".to_string());
    }
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match host.as_str() {
        "www.nvidia.com" | "nvidia.com" | "www.amd.com" | "amd.com" | "www.intel.com"
        | "intel.com" => Ok(()),
        _ => Err("Driver URL host is not allowed".to_string()),
    }
}

/// A jar name is the only thing the webview may send back. It must be a bare
/// file name inside this instance's `mods` folder; separators, absolute paths
/// and `.disabled` targets are rejected before anything is touched.
pub fn validate_mod_filename(filename: &str) -> Result<(), String> {
    if filename.is_empty() || filename.len() > 255 {
        return Err("Invalid mod file name".to_string());
    }
    if filename.contains(['/', '\\', ':', '\0']) || filename == "." || filename == ".." {
        return Err("Invalid mod file name".to_string());
    }
    if !filename.to_lowercase().ends_with(".jar") || filename.ends_with(".disabled") {
        return Err("Only an enabled .jar can be disabled".to_string());
    }
    crate::import::check_safe_file_name(filename).map_err(|_| "Invalid mod file name".to_string())
}

/// Move a mod to its `.disabled` name without ever replacing an existing
/// destination. `std::fs::rename` uses `MOVEFILE_REPLACE_EXISTING` on Windows,
/// which would silently overwrite a `.disabled` file created after the
/// advisory existence check.
fn rename_no_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
        let from: Vec<u16> = source
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let to: Vec<u16> = target
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Deliberately no MOVEFILE_REPLACE_EXISTING.
        let moved = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) };
        if moved == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::hard_link(source, target)?;
        std::fs::remove_file(source)?;
        Ok(())
    }
}

/// Disable exactly one mod by renaming it to `<name>.disabled`. The game and
/// the content scanner both honour that suffix. The instance must not be
/// running: changing its mod set under a live process is not a safe fix.
pub fn disable_mod(config: &AppConfig, instance: &Instance, filename: &str) -> Result<(), String> {
    validate_mod_filename(filename)?;
    if crate::external_minecraft::instance_is_running(config, instance) {
        return Err("Instance is running; close the game before changing its mods".to_string());
    }
    let mods = instance.mods_dir(&config.instances_dir());
    let source = mods.join(filename);
    let target = mods.join(format!("{filename}.disabled"));
    let instance_root = std::fs::canonicalize(instance.minecraft_dir(&config.instances_dir()))
        .map_err(|_| "Instance directory is unavailable".to_string())?;
    let mods_canon =
        std::fs::canonicalize(&mods).map_err(|_| "Mods directory is unavailable".to_string())?;
    if !mods_canon.starts_with(&instance_root) {
        return Err("Mods directory is outside the instance".to_string());
    }
    crate::import::ensure_no_reparse_points(&mods, &source)
        .map_err(|_| "Refusing to modify a linked mod file".to_string())?;
    crate::import::ensure_no_reparse_points(&mods, &target)
        .map_err(|_| "Refusing to write through a linked path".to_string())?;
    let metadata =
        std::fs::symlink_metadata(&source).map_err(|_| "Mod file not found".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Mod file not found".to_string());
    }
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err("A disabled file with this name already exists".to_string());
    }
    // The mods directory itself is the operation boundary. Re-resolve it
    // right before the rename so swapping it for a junction after the first
    // canonical check cannot redirect the move.
    if std::fs::canonicalize(&mods).ok().as_deref() != Some(mods_canon.as_path()) {
        return Err("Mods directory changed during the operation".to_string());
    }
    // Re-check immediately before the single rename: a locally started game
    // cannot be reserved, so the first process-table snapshot is only a
    // pre-filter. `rename_no_replace` closes the separate destination race.
    if crate::external_minecraft::instance_is_running(config, instance) {
        return Err("Instance is running; close the game before changing its mods".to_string());
    }
    rename_no_replace(&source, &target).map_err(|e| format!("Could not disable the mod: {e}"))
}

/// Execute one action after re-validating it. Only `disable_mod` has a
/// backend effect; `open_url` and `repair_files` are frontend actions.
pub fn execute(
    config: &AppConfig,
    instance: &Instance,
    action_kind: &str,
    arg: &str,
) -> Result<(), String> {
    match action_kind {
        "disable_mod" => disable_mod(config, instance, arg),
        "open_url" => validate_official_url(arg),
        "repair_files" => Ok(()),
        other => Err(format!("Unsupported recovery action: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::LoaderType;
    use std::path::Path;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_recovery_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config_at(dir: &Path) -> AppConfig {
        AppConfig {
            data_dir: dir.to_path_buf(),
            ..Default::default()
        }
    }

    fn instance() -> Instance {
        Instance::new("Test", "1.20.1", LoaderType::Vanilla, None)
    }

    #[test]
    fn fatal_mod_evidence_offers_disable() {
        let actions = plan("mod_crash", Some("sodium.jar"), "");
        assert_eq!(
            actions,
            vec![CrashAction {
                kind: "disable_mod".into(),
                arg: "sodium.jar".into(),
                tone: "danger".into()
            }]
        );
    }

    #[test]
    fn a_missing_class_does_not_blame_a_jar() {
        let actions = plan("missing_class", Some("sodium.jar"), "");
        assert_eq!(
            actions,
            vec![CrashAction {
                kind: "repair_files".into(),
                arg: String::new(),
                tone: "secondary".into()
            }],
            "a missing class can be a dependency problem; never auto-disable a jar for it"
        );
    }

    #[test]
    fn a_driver_crash_opens_the_vendor_page() {
        let actions = plan("graphics_driver", None, "C [nvoglv64.dll+0x1]");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, "open_url");
        assert!(actions[0].arg.contains("nvidia.com"));
    }

    #[test]
    fn an_unknown_failure_offers_a_file_repair_fallback() {
        let actions = plan("unknown", None, "");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, "repair_files");
    }

    #[test]
    fn only_exact_official_https_driver_hosts_are_opened() {
        for good in [
            "https://www.nvidia.com/Download/index.aspx",
            "https://www.amd.com/en/support",
            "https://www.intel.com/content/www/us/en/download-center/home.html",
        ] {
            assert!(validate_official_url(good).is_ok(), "{good}");
        }
        for bad in [
            "http://www.nvidia.com/Download/index.aspx",
            "https://www.nvidia.com.evil.example/driver",
            "https://evil.example/?next=nvidia.com",
            "https://nvidia.com@evil.example/driver",
            "https://127.0.0.1/nvidia.com",
            "file:///C:/Windows/System32/drivers",
            "https://example.com/driver",
        ] {
            assert!(validate_official_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn path_traversal_and_disabled_targets_are_refused() {
        for bad in [
            "../sodium.jar",
            "sub/sodium.jar",
            "C:\\sodium.jar",
            "sodium.jar.disabled",
            "notes.txt",
            "",
        ] {
            assert!(validate_mod_filename(bad).is_err(), "{bad}");
        }
        assert!(validate_mod_filename("sodium.jar").is_ok());
    }

    #[test]
    fn disabling_renames_only_the_named_jar() {
        let dir = temp_dir("disable");
        let config = config_at(&dir);
        let instance = instance();
        let mods = instance.mods_dir(&config.instances_dir());
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("sodium.jar"), b"jar").unwrap();
        std::fs::write(mods.join("other.jar"), b"jar").unwrap();

        disable_mod(&config, &instance, "sodium.jar").unwrap();
        assert!(!mods.join("sodium.jar").exists());
        assert!(mods.join("sodium.jar.disabled").exists());
        assert!(
            mods.join("other.jar").exists(),
            "other mods must stay untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_running_instance_refuses_to_change_its_mods() {
        let dir = temp_dir("running");
        let config = config_at(&dir);
        let instance = instance();
        let mods = instance.mods_dir(&config.instances_dir());
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("sodium.jar"), b"jar").unwrap();
        let probe = crate::running_state::read_identity(std::process::id());
        if let crate::running_state::IdentityProbe::Found(identity) = probe {
            crate::running_state::write_pid_file(
                &config.data_dir,
                &instance.name,
                std::process::id(),
                Some(&identity),
            )
            .unwrap();
            assert!(disable_mod(&config, &instance, "sodium.jar").is_err());
            crate::running_state::remove_pid_file(&config.data_dir, &instance.name);
            assert!(mods.join("sodium.jar").exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_symlinked_mod_is_not_followed_outside_the_instance() {
        let dir = temp_dir("symlink_mod");
        let config = config_at(&dir);
        let instance = instance();
        let mods = instance.mods_dir(&config.instances_dir());
        std::fs::create_dir_all(&mods).unwrap();
        let outside = dir.join("outside.jar");
        std::fs::write(&outside, b"jar").unwrap();
        let link = mods.join("linked.jar");
        if std::os::windows::fs::symlink_file(&outside, &link).is_err() {
            eprintln!("skipping symlink test: privilege unavailable");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert!(disable_mod(&config, &instance, "linked.jar").is_err());
        assert!(link.exists(), "the link must stay untouched");
        assert!(outside.exists(), "the target must stay untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_never_replaces_an_existing_disabled_file() {
        let dir = temp_dir("no_replace");
        let source = dir.join("sodium.jar");
        let target = dir.join("sodium.jar.disabled");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&target, b"keep").unwrap();
        assert!(rename_no_replace(&source, &target).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"new");
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_already_disabled_jar_is_not_silently_replaced() {
        let dir = temp_dir("conflict");
        let config = config_at(&dir);
        let instance = instance();
        let mods = instance.mods_dir(&config.instances_dir());
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("sodium.jar"), b"jar").unwrap();
        std::fs::write(mods.join("sodium.jar.disabled"), b"old").unwrap();
        assert!(disable_mod(&config, &instance, "sodium.jar").is_err());
        assert!(disable_mod(&config, &instance, "missing.jar").is_err());
        assert!(mods.join("sodium.jar").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

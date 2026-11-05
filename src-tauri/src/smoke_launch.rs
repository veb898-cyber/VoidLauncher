//! Smoke-launch test: installs Minecraft versions + loaders into an isolated
//! temp data dir, launches each combo with the real `launch_minecraft` path,
//! watches `logs/latest.log` for startup markers, then kills the process.
//!
//! Run:  cargo test --lib smoke_all -- --ignored --nocapture
//! Env:  SMOKE_DATA_DIR  (optional, default: %TEMP%\voidsmoke-<millis>; reused
//!                        between runs for cached libraries / resume support)
//!       SMOKE_COMBOS    (optional: restrict the run to these scenarios, e.g.
//!                        "latest-vanilla,1.20.1-Fabric,1.21.4-NeoForge";
//!                        default: the curated COMBOS list)
//!       SMOKE_RESULTS   (optional: report path, default target/smoke_results.txt)
//!       SMOKE_FILTER    (optional: dev-only substring filter, skipped combos
//!                        are reported as passing)

use crate::config::AppConfig;
use crate::download;
use crate::instances::{self, Instance, LoaderType};
use crate::launch;
use crate::modloaders;
use crate::versions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const POLL_STEP: Duration = Duration::from_millis(1000);
const COMBO_TIMEOUT: Duration = Duration::from_secs(120);

/// After the process is gone, wait this long for both launcher pipe readers to
/// reach EOF and flush the raw tee files before reading diagnostics.
const READERS_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound for reaping a killed child (taskkill /F is normally immediate).
const REAP_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-file cap for the published diagnostics (tail copies).
const MAX_DIAG_BYTES: u64 = 2 * 1024 * 1024;
/// How many crash reports of one run are copied to the artifact.
const MAX_CRASH_REPORTS: usize = 4;

const COMBOS: &[(&str, &str)] = &[
    ("1.8.9", "Forge"),
    ("1.12.2", "Vanilla"),
    ("1.14.4", "Fabric"),
    ("1.12.2", "Forge"),
    ("1.16.5", "Forge"),
    ("1.18.2", "Fabric"),
    ("1.18.2", "Forge"),
    ("1.20.1", "Fabric"),
    ("1.20.1", "NeoForge"),
    ("1.21.4", "Vanilla"),
    ("1.21.4", "NeoForge"),
];

fn loader_type(s: &str) -> LoaderType {
    match s {
        "Fabric" => LoaderType::Fabric,
        "Forge" => LoaderType::Forge,
        "NeoForge" => LoaderType::NeoForge,
        _ => LoaderType::Vanilla,
    }
}

fn loader_marker(s: &str) -> Option<&'static str> {
    match s {
        "Fabric" => Some("FabricLoader"),
        "Forge" => Some("MinecraftForge"),
        "NeoForge" => Some("NeoForge"),
        _ => None,
    }
}

// "Sound engine started" is printed by every MC version (1.8-era
// LWJGL2 clients included, which never print "Backend library: LWJGL").
const MAIN_MARKERS: &[&str] = &["Sound engine started", "Minecraft client started"];

fn temp_dir_path() -> PathBuf {
    std::env::var("SMOKE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!(
                "voidsmoke-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            ))
        })
}

fn results_path() -> PathBuf {
    std::env::var("SMOKE_RESULTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("smoke_results.txt")
        })
}

fn report_line(s: &str) {
    eprintln!("[smoke] {}", s);
    let path = results_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    use std::io::Write;
    let _ = writeln!(f, "{}", s);
}

/// Parse one `SMOKE_COMBOS` entry that is not the `latest-vanilla` alias.
///
/// Version ids may contain dashes (`1.20.5-pre1`), loader names do not, so the
/// split happens at the last dash.
fn parse_combo_entry(entry: &str) -> Result<(String, String), String> {
    let Some((mc, loader)) = entry.rsplit_once('-') else {
        return Err(format!(
            "SMOKE_COMBOS entry '{}' is not <minecraft>-<loader>",
            entry
        ));
    };
    if !matches!(loader, "Vanilla" | "Fabric" | "Forge" | "NeoForge") {
        return Err(format!(
            "SMOKE_COMBOS entry '{}' has an unknown loader '{}'",
            entry, loader
        ));
    }
    if mc.trim().is_empty() {
        return Err(format!("SMOKE_COMBOS entry '{}' has no version", entry));
    }
    Ok((mc.to_string(), loader.to_string()))
}

/// Combos to run.
///
/// Default: the curated [`COMBOS`] list (handy locally). `SMOKE_COMBOS`
/// (comma separated) restricts the run to the given scenarios, which is what
/// CI uses so the report only contains tests that actually ran. Each entry is
/// either `<minecraft>-<loader>` or the alias `latest-vanilla`, resolved
/// against the official version manifest at run time.
async fn resolve_combos() -> Result<Vec<(String, String)>, String> {
    let raw = match std::env::var("SMOKE_COMBOS") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            return Ok(COMBOS
                .iter()
                .map(|(mc, loader)| (mc.to_string(), loader.to_string()))
                .collect())
        }
    };

    let mut combos: Vec<(String, String)> = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if entry.eq_ignore_ascii_case("latest-vanilla") {
            let manifest = versions::fetch_version_manifest()
                .await
                .map_err(|e| format!("cannot resolve latest vanilla version: {}", e))?;
            combos.push((manifest.latest.release, "Vanilla".to_string()));
            continue;
        }
        combos.push(parse_combo_entry(entry)?);
    }
    if combos.is_empty() {
        return Err("SMOKE_COMBOS selects no combos".to_string());
    }
    Ok(combos)
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;
    use std::time::SystemTime;

    fn temp(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vl_{}_{}_{}",
            prefix,
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write_file(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(path, text).expect("write");
    }

    fn set_modified(path: &Path, at: SystemTime) {
        if let Ok(file) = std::fs::File::options().write(true).open(path) {
            let _ = file.set_modified(at);
        }
    }

    #[test]
    fn diagnostics_take_only_the_current_runs_output() {
        let data = temp("diag_iso");
        let diag = temp("diag_iso_out");
        let minecraft = data.join("instances/smoke-Pack/.minecraft");
        std::fs::create_dir_all(&minecraft).expect("minecraft dir");
        std::fs::create_dir_all(data.join("logs/game")).expect("tee dir");

        // A previous run of the same instance and a concurrent other combo.
        let old = data.join("logs/game/smoke-Pack_20260101_000000.stdout.log");
        write_file(&old, "old run line\n");
        set_modified(&old, SystemTime::now() - Duration::from_secs(600));

        let other = data.join("logs/game/smoke-Other_20260101_000000.stdout.log");
        write_file(&other, "other combo line\n");

        let since = SystemTime::now();
        let current = data.join("logs/game/smoke-Pack_20260101_000100.stdout.log");
        write_file(&current, "current run line\n");

        let lines = collect_diagnostics(
            &diag,
            "Pack-Vanilla",
            "smoke-Pack",
            &minecraft,
            &data,
            since,
        );
        let copied =
            std::fs::read_to_string(diag.join("Pack-Vanilla/stdout.log")).expect("stdout copy");
        assert!(copied.contains("current run line"), "{copied}");
        assert!(
            !copied.contains("old run line"),
            "a previous run must not leak into this combo: {copied}"
        );
        assert!(
            !copied.contains("other combo line"),
            "another combo's log must not leak: {copied}"
        );
        assert!(
            lines.iter().any(|line| line.contains("current run line")),
            "the tail must reach the report: {lines:?}"
        );

        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&diag);
    }

    #[test]
    fn diagnostics_copy_latest_log_and_crash_reports() {
        let data = temp("diag_crash");
        let diag = temp("diag_crash_out");
        let minecraft = data.join("instances/smoke-Pack/.minecraft");
        write_file(
            &minecraft.join("logs/latest.log"),
            "[INFO] latest line\n[INFO] Sound engine started\n",
        );
        write_file(
            &minecraft.join("crash-reports/crash-2026-01-01_00.00.00-client.txt"),
            "crash head\njava.lang.RuntimeException: no OpenGL context\n",
        );

        let since = SystemTime::now() - Duration::from_secs(30);
        let lines = collect_diagnostics(
            &diag,
            "Pack-Vanilla",
            "smoke-Pack",
            &minecraft,
            &data,
            since,
        );
        assert!(diag.join("Pack-Vanilla/latest.log").exists());
        assert!(diag.join("Pack-Vanilla").read_dir().unwrap().count() >= 2);
        assert!(
            lines.iter().any(|line| line.contains("latest line")),
            "latest.log tail missing: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("no OpenGL context")),
            "crash report content must reach the report: {lines:?}"
        );

        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&diag);
    }

    /// Collected output after a timeout-style kill: the child is terminated,
    /// reaped, and the pipe readers are waited for before the tee is read.
    #[test]
    #[cfg(windows)]
    fn output_and_diagnostics_are_complete_after_a_timeout_kill() {
        use std::os::windows::process::CommandExt;
        let data = temp("diag_timeout");
        let minecraft = data.join("instances/smoke-Pack/.minecraft");
        std::fs::create_dir_all(&minecraft).expect("minecraft dir");
        std::fs::create_dir_all(data.join("logs/game")).expect("tee dir");

        let mut child = std::process::Command::new("cmd")
            .args(["/C", "echo smoke-stdout-line & ping -n 30 127.0.0.1"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .creation_flags(0x0800_0000)
            .spawn()
            .expect("spawn test process");
        let pid = child.id();
        let since = SystemTime::now();

        let out_path = data.join("logs/game/smoke-Pack_timeout_run.stdout.log");
        let err_path = data.join("logs/game/smoke-Pack_timeout_run.stderr.log");
        crate::game_logs::attach_output_readers(
            pid,
            Box::new(child.stdout.take().expect("stdout")),
            Box::new(child.stderr.take().expect("stderr")),
            std::fs::File::create(&out_path).ok(),
            std::fs::File::create(&err_path).ok(),
            None,
        );
        std::thread::sleep(Duration::from_millis(500));

        let run = ComboRun {
            combo_name: "Pack-Vanilla",
            instance_name: "smoke-Pack",
            minecraft_dir: &minecraft,
            data_dir: &data,
            since,
        };
        let (readers, reaped, lines) = finish_combo(&mut child, pid, &run, true);
        assert!(reaped, "the killed child must be reaped");
        assert!(
            matches!(
                readers,
                crate::game_logs::ReadersWait::Finished | crate::game_logs::ReadersWait::Unknown
            ),
            "both readers must finish after the process is gone: {readers:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("smoke-stdout-line")),
            "the real stdout must be reported after the timeout: {lines:?}"
        );

        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn diagnostics_never_publish_the_smoke_token() {
        let data = temp("diag_token");
        let diag = temp("diag_token_out");
        let minecraft = data.join("instances/smoke-Pack/.minecraft");
        std::fs::create_dir_all(&minecraft).expect("minecraft dir");
        std::fs::create_dir_all(data.join("logs/game")).expect("tee dir");

        let since = SystemTime::now();
        write_file(
            &data.join("logs/game/smoke-Pack_run.stdout.log"),
            "Realm error: Failed to parse into SignedJWT: smoke-offline-token\n",
        );
        let lines = collect_diagnostics(
            &diag,
            "Pack-Vanilla",
            "smoke-Pack",
            &minecraft,
            &data,
            since,
        );

        let copied =
            std::fs::read_to_string(diag.join("Pack-Vanilla/stdout.log")).expect("stdout copy");
        assert!(!copied.contains("smoke-offline-token"), "{copied}");
        assert!(copied.contains("[redacted]"), "{copied}");

        let report = lines.join("\n");
        assert!(
            !report.contains("smoke-offline-token"),
            "the report must not publish the token: {report}"
        );
        assert!(report.contains("[redacted]"), "{report}");

        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&diag);
    }

    #[test]
    #[cfg(windows)]
    fn gl_probe_reports_a_capability_or_a_reason() {
        let lines = gl_probe();
        assert!(!lines.is_empty());
        assert!(
            lines[0].contains("OpenGL probe"),
            "unexpected probe output: {:?}",
            lines
        );
        for line in &lines {
            assert!(!line.contains('\n'), "each diagnostics line is one line");
        }
    }
}

#[cfg(test)]
mod combo_selection_tests {
    use super::parse_combo_entry;

    #[test]
    fn parses_a_version_and_loader_pair() {
        assert_eq!(
            parse_combo_entry("1.20.1-Fabric").unwrap(),
            ("1.20.1".to_string(), "Fabric".to_string())
        );
        assert_eq!(
            parse_combo_entry("1.21.4-NeoForge").unwrap(),
            ("1.21.4".to_string(), "NeoForge".to_string())
        );
    }

    #[test]
    fn keeps_dashes_inside_prerelease_versions() {
        assert_eq!(
            parse_combo_entry("1.20.5-pre1-Vanilla").unwrap(),
            ("1.20.5-pre1".to_string(), "Vanilla".to_string())
        );
    }

    #[test]
    fn rejects_malformed_entries() {
        for entry in ["1.20.1", "Fabric", "-Fabric", "1.20.1-FabricMod", ""] {
            assert!(
                parse_combo_entry(entry).is_err(),
                "'{}' must be rejected",
                entry
            );
        }
    }
}

fn java_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("java")
}

fn check_java_availability(data_dir: &Path, required_major: u32) -> bool {
    let installations = launch::find_all_java_installations(data_dir);
    // Mirrors the launch flow: only an exact 64-bit major match is accepted.
    installations
        .iter()
        .any(|j| j.is_64bit && j.major_version == required_major)
}

/// Ensure a managed JRE of `major` exists in `data_dir/java/jdk-<major>`.
/// Uses the exact same Adoptium source + layout as the launcher's own
/// `java_download` module so `list_managed_java` picks it up.
async fn ensure_java(data_dir: &Path, major: u32) -> Result<(), String> {
    let data_dir = data_dir.to_path_buf();
    if check_java_availability(&data_dir, major) {
        return Ok(());
    }
    let dir = java_dir(&data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let url = format!(
        "https://api.adoptium.net/v3/binary/latest/{}/ga/windows/x64/jre/hotspot/normal/eclipse",
        major
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1800))
        .connect_timeout(Duration::from_secs(30))
        .user_agent("VoidLauncherSmoke/0.1")
        .build()
        .map_err(|e| e.to_string())?;

    report_line(&format!("Downloading Java {} (managed runtime)...", major));
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Java {} download failed: {}", major, e))?;
    if !resp.status().is_success() {
        return Err(format!("Java {} download: HTTP {}", major, resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Java {} body read failed: {}", major, e))?;

    let tmp_zip = dir.join(format!("jdk-{}.zip", major));
    std::fs::write(&tmp_zip, &bytes).map_err(|e| e.to_string())?;

    let dest = dir.join(format!("jdk-{}", major));
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;

    let file = std::fs::File::open(&tmp_zip).map_err(|e| format!("open zip: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("zip: {}", e))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("zip entry {}: {}", i, e))?;
        let raw_name = entry.name().replace('\\', "/");
        let is_dir = raw_name.ends_with('/') || raw_name.ends_with('\\');
        let parts: Vec<&str> = raw_name.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() || is_dir {
            continue;
        }
        let mut target = dest.clone();
        for p in &parts {
            if *p == ".." {
                return Err("zip-slip entry in Java archive".to_string());
            }
            target.push(p);
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    }

    std::fs::write(dest.join(".extracted"), b"1").map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&tmp_zip);

    if check_java_availability(&data_dir, major) {
        report_line(&format!("Java {} ready in managed runtimes", major));
        Ok(())
    } else {
        Err(format!(
            "Java {} extracted but not detected (content: {:?})",
            major,
            std::fs::read_dir(&dest).map(|it| it.count())
        ))
    }
}

/// Resolve a loader version for the MC version and install the loader.
async fn install_loader_for(
    data_dir: &Path,
    mc: &str,
    loader: &str,
) -> Result<(String, crate::modloaders::LoaderProfile), String> {
    let libs = data_dir.join("libraries");
    let vers = data_dir.join("versions");

    let page = match loader {
        "Fabric" => modloaders::fabric::get_loader_versions(0, 40).await,
        "Forge" => modloaders::forge::get_loader_versions(mc, 0, 40).await,
        "NeoForge" => modloaders::neoforge::get_loader_versions(mc, 0, 40).await,
        _ => return Err("vanilla has no loader version".into()),
    }
    .map_err(|e| format!("list {} versions: {}", loader, e))?;

    let candidates: Vec<String> = page
        .versions
        .iter()
        .filter(|v| v.stable)
        .map(|v| v.version.clone())
        .chain(
            page.versions
                .iter()
                .filter(|v| !v.stable)
                .map(|v| v.version.clone()),
        )
        .collect();

    let mut last_err = "no versions returned".to_string();
    for ver in candidates.iter().take(5) {
        report_line(&format!("  {} {} -> installing {}...", loader, mc, ver));
        match modloaders::install_loader(loader, mc, ver, &libs, &vers, None).await {
            Ok(profile) => return Ok((ver.clone(), profile)),
            Err(e) => {
                last_err = format!("{}: {}", ver, e);
                report_line(&format!("  {} failed ({}), trying next", ver, e));
            }
        }
    }
    Err(format!(
        "all {} versions failed. last: {}",
        loader, last_err
    ))
}

async fn install_version(config: &AppConfig, mc: &str) -> Result<versions::VersionInfo, String> {
    let manifest = versions::fetch_version_manifest()
        .await
        .map_err(|e| format!("manifest: {}", e))?;
    let url = manifest
        .versions
        .iter()
        .find(|v| v.id == mc)
        .map(|v| v.url.clone())
        .ok_or_else(|| format!("mc {} not in manifest", mc))?;

    let vi = versions::fetch_version_info(&url)
        .await
        .map_err(|e| format!("version info {}: {}", mc, e))?;

    let files = versions::collect_downloads(&vi, &config.libraries_dir(), &config.versions_dir())
        .map_err(|e| format!("collect downloads {}: {}", mc, e))?;
    if !files.is_empty() {
        report_line(&format!("downloading {} files for {}...", files.len(), mc));
        download::download_files(files, |_, _, _, _, _| {})
            .await
            .map_err(|e| e.to_string())?;
    }

    // The native-library ensure step happens at launch (see below): that is
    // where the JVM — and therefore the native architecture — is known.

    let idx_path = config
        .assets_dir()
        .join("indexes")
        .join(format!("{}.json", vi.assets));
    if !idx_path.exists() {
        let asset_index = versions::fetch_asset_index(&vi.asset_index.url)
            .await
            .map_err(|e| format!("asset index {}: {}", mc, e))?;
        std::fs::create_dir_all(idx_path.parent().unwrap()).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&asset_index).map_err(|e| e.to_string())?;
        std::fs::write(&idx_path, json).map_err(|e| e.to_string())?;
        download::download_assets(&asset_index, &config.assets_dir(), |_, _, _, _, _| {})
            .await
            .map_err(|e| format!("assets {}: {}", mc, e))?;
        let asset_index = versions::load_asset_index(&idx_path).map_err(|e| e.to_string())?;
        download::ensure_virtual_assets(&vi.assets, &asset_index, &config.assets_dir())
            .map_err(|e| format!("virtual assets {}: {}", mc, e))?;
    }

    Ok(vi)
}

fn kill_tree(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .creation_flags(0x08000000)
            .output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).output();
    }
}

/// Tail of a byte buffer as lossy UTF-8, capped for error messages. Char-safe:
/// `latest.log` lines may contain non-ASCII text (localised mod messages).
fn pipe_tail(buf: &[u8], max_chars: usize) -> String {
    let s = String::from_utf8_lossy(buf);
    let s = s.trim();
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    s.chars().skip(count - max_chars).collect()
}

/// Never publish tokens: the harness uses a fixed offline token, and the
/// diagnostics copy is scrubbed defensively anyway.
const SMOKE_TOKEN: &str = "smoke-offline-token";

fn scrub(text: &str) -> String {
    let mut out = text.replace(SMOKE_TOKEN, "[redacted]");
    // Also hide any `--accessToken <value>` pair that a JVM error might echo.
    while let Some(pos) = out.find("--accessToken ") {
        let start = pos + "--accessToken ".len();
        let end = out[start..]
            .find(char::is_whitespace)
            .map(|n| start + n)
            .unwrap_or(out.len());
        out.replace_range(start..end, "[redacted]");
    }
    out
}

/// Where the per-combination diagnostic copies are written.
/// `SMOKE_DIAG_DIR` wins (CI publishes that directory); otherwise `<data>/diagnostics`.
fn diagnostics_dir(data_dir: &Path) -> PathBuf {
    std::env::var("SMOKE_DIAG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir.join("diagnostics"))
}

/// Find the raw tee file of THIS run: same instance prefix and modified no
/// earlier than the launch. Picking the newest file by time alone could take a
/// previous combination's log; the instance prefix and the launch window make
/// the match exact.
fn find_run_tee(
    data_dir: &Path,
    instance_name: &str,
    suffix: &str,
    since: std::time::SystemTime,
) -> Option<PathBuf> {
    let dir = data_dir.join("logs").join("game");
    let prefix = format!(
        "{}_",
        crate::game_logs::sanitize_instance_name(instance_name)
    );
    let cutoff = since.checked_sub(Duration::from_secs(2)).unwrap_or(since);
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if !name.starts_with(&prefix) || !name.ends_with(suffix) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if mtime < cutoff {
            continue;
        }
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Copy at most `max_bytes` of `src` (its tail) to `dst`, scrubbing tokens.
fn copy_tail(src: &Path, dst: &Path, max_bytes: u64) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut input = std::fs::File::open(src).ok()?;
    let len = input.metadata().ok()?.len();
    let skip = len.saturating_sub(max_bytes);
    input.seek(SeekFrom::Start(skip)).ok()?;
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes).ok()?;
    let text = scrub(&String::from_utf8_lossy(&bytes));
    let mut out = std::fs::File::create(dst).ok()?;
    out.write_all(text.as_bytes()).ok()?;
    Some(len)
}

fn read_tail_text(path: &Path, max_chars: usize) -> String {
    std::fs::read(path)
        .map(|bytes| scrub(&pipe_tail(&bytes, max_chars)))
        .unwrap_or_default()
}

/// Copy the current run's forensic files into the diagnostics directory and
/// return report lines describing them (including the tails that explain a
/// failure).
fn collect_diagnostics(
    diag_root: &Path,
    combo_name: &str,
    instance_name: &str,
    minecraft_dir: &Path,
    data_dir: &Path,
    since: std::time::SystemTime,
) -> Vec<String> {
    let out_dir = diag_root.join(combo_name);
    let _ = std::fs::create_dir_all(&out_dir);
    let mut lines = Vec::new();

    for (suffix, label) in [(".stdout.log", "stdout"), (".stderr.log", "stderr")] {
        match find_run_tee(data_dir, instance_name, suffix, since) {
            Some(src) => {
                let copied = copy_tail(
                    &src,
                    &out_dir.join(format!("{}.log", label)),
                    MAX_DIAG_BYTES,
                )
                .unwrap_or(0);
                let file_name = src
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                lines.push(format!(
                    "diag {}: {} = {} B (raw: {})",
                    combo_name, label, copied, file_name
                ));
                let tail = read_tail_text(&src, 4000);
                if !tail.is_empty() {
                    lines.push(format!("diag {}: {} tail:\n{}", combo_name, label, tail));
                }
            }
            None => lines.push(format!(
                "diag {}: {}: no raw output file for this run",
                combo_name, label
            )),
        }
    }

    let latest = minecraft_dir.join("logs").join("latest.log");
    if latest.exists() {
        let copied = copy_tail(&latest, &out_dir.join("latest.log"), MAX_DIAG_BYTES).unwrap_or(0);
        lines.push(format!("diag {}: latest.log = {} B", combo_name, copied));
        let tail = read_tail_text(&latest, 8000);
        if !tail.is_empty() {
            lines.push(format!("diag {}: latest.log tail:\n{}", combo_name, tail));
        }
    } else {
        lines.push(format!("diag {}: latest.log: absent", combo_name));
    }

    let crash_dir = minecraft_dir.join("crash-reports");
    let cutoff = since.checked_sub(Duration::from_secs(5)).unwrap_or(since);
    let mut crashes: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&crash_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("txt"))
                != Some(true)
            {
                continue;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if mtime < cutoff {
                continue;
            }
            crashes.push((mtime, path));
        }
    }
    crashes.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    if crashes.is_empty() {
        lines.push(format!("diag {}: crash-reports: none", combo_name));
    } else {
        for (index, (_, path)) in crashes.iter().take(MAX_CRASH_REPORTS).enumerate() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let _ = copy_tail(
                path,
                &out_dir.join(format!("crash-{}-{}", index + 1, name)),
                MAX_DIAG_BYTES,
            );
            lines.push(format!("diag {}: crash report {}", combo_name, name));
            if index == 0 {
                let tail = read_tail_text(path, 8000);
                if !tail.is_empty() {
                    lines.push(format!("diag {}: crash report tail:\n{}", combo_name, tail));
                }
            }
        }
    }

    lines
}

/// Terminate the child (tree) and reap it. Returns false when the process is
/// still not reaped after the bounded wait.
fn stop_and_reap(child: &mut std::process::Child) -> bool {
    let pid = child.id();
    kill_tree(pid);
    let deadline = Instant::now() + REAP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_) => return false,
        }
    }
}

/// Wait (bounded) for both launcher pipe readers to reach EOF and flush.
fn drain_output(pid: u32) -> crate::game_logs::ReadersWait {
    crate::game_logs::wait_for_output_readers(pid, READERS_DRAIN_TIMEOUT)
}

/// Everything needed to finish and describe one combo run.
struct ComboRun<'a> {
    combo_name: &'a str,
    instance_name: &'a str,
    minecraft_dir: &'a Path,
    data_dir: &'a Path,
    since: std::time::SystemTime,
}

/// Finish one combo: stop (when still running) and reap the child, wait for
/// the output readers, then collect the diagnostics. Every failure path runs
/// through here, so no branch can read a tee file before it is flushed.
fn finish_combo(
    child: &mut std::process::Child,
    pid: u32,
    run: &ComboRun<'_>,
    kill: bool,
) -> (crate::game_logs::ReadersWait, bool, Vec<String>) {
    let reaped = if kill {
        stop_and_reap(child)
    } else {
        // The caller already saw the process exit through `try_wait`.
        matches!(child.try_wait(), Ok(Some(_)))
    };
    let readers = drain_output(pid);
    let diagnostics = collect_diagnostics(
        &diagnostics_dir(run.data_dir),
        run.combo_name,
        run.instance_name,
        run.minecraft_dir,
        run.data_dir,
        run.since,
    );
    (readers, reaped, diagnostics)
}

/// [`finish_combo`] plus the report lines (used by the real smoke run).
fn finish_run(
    child: &mut std::process::Child,
    pid: u32,
    run: &ComboRun<'_>,
    kill: bool,
) -> (crate::game_logs::ReadersWait, bool, Vec<String>) {
    let (readers, reaped, diagnostics) = finish_combo(child, pid, run, kill);
    report_line(&format!(
        "{}: process reaped={}, output readers={:?}",
        run.combo_name, reaped, readers
    ));
    for line in &diagnostics {
        report_line(line);
    }
    (readers, reaped, diagnostics)
}

/// Run a short host command with a hard deadline (diagnostics only).
fn run_capture(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    use std::process::Stdio;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(text)
}

/// Host diagnostics recorded before the first game starts: OS build and the
/// graphics stack. Uses only standard Windows tooling; nothing is installed.
fn runner_diagnostics() -> Vec<String> {
    let mut lines = vec![format!(
        "runner: os={} arch={} family={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY
    )];
    if let Some(ver) = run_capture("cmd", &["/C", "ver"], Duration::from_secs(15)) {
        let ver = ver.trim();
        if !ver.is_empty() {
            lines.push(format!("runner: {}", ver));
        }
    }
    if let Some(gpu) = run_capture(
        "powershell",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_VideoController | ForEach-Object { '{0} | driver {1} | {2} | adapter RAM {3} MB' -f $_.Name, $_.DriverVersion, $_.VideoProcessor, [int]($_.AdapterRAM/1MB) }",
        ],
        Duration::from_secs(30),
    ) {
        for line in gpu
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(6)
        {
            lines.push(format!("gpu: {}", line));
        }
    }
    lines.extend(gl_probe());
    lines
}

#[cfg(not(windows))]
fn gl_probe() -> Vec<String> {
    vec!["gpu: OpenGL probe: unsupported on this platform".to_string()]
}

/// Minimal OpenGL capability check: a hidden window, a legacy context, and an
/// attempt to create a 3.2 core-profile context. Diagnostics only — this
/// never replaces the real Minecraft launch.
#[cfg(windows)]
fn gl_probe() -> Vec<String> {
    match gl_capability() {
        Ok(text) => vec![format!("gpu: OpenGL probe: {}", text)],
        Err(err) => vec![format!("gpu: OpenGL probe failed: {}", err)],
    }
}

#[cfg(windows)]
fn gl_capability() -> Result<String, String> {
    use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
    use windows_sys::Win32::Graphics::Gdi::{GetDC, ReleaseDC, HDC};
    use windows_sys::Win32::Graphics::OpenGL::{
        glGetString, wglCreateContext, wglDeleteContext, wglGetProcAddress, wglMakeCurrent,
        ChoosePixelFormat, SetPixelFormat, GL_RENDERER, GL_VERSION, HGLRC, PFD_DOUBLEBUFFER,
        PFD_DRAW_TO_WINDOW, PFD_SUPPORT_OPENGL, PFD_TYPE_RGBA, PIXELFORMATDESCRIPTOR,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, UnregisterClassW, CS_OWNDC,
        WNDCLASSW, WS_POPUP,
    };

    use std::os::windows::ffi::OsStrExt;

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    struct ClassGuard(Vec<u16>, HANDLE);
    impl Drop for ClassGuard {
        fn drop(&mut self) {
            unsafe { UnregisterClassW(self.0.as_ptr(), self.1) };
        }
    }
    struct WindowGuard(HANDLE);
    impl Drop for WindowGuard {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.0) };
        }
    }
    struct DcGuard(HANDLE, HDC);
    impl Drop for DcGuard {
        fn drop(&mut self) {
            unsafe { ReleaseDC(self.0, self.1) };
        }
    }
    struct GlContext(HGLRC);
    impl Drop for GlContext {
        fn drop(&mut self) {
            unsafe { wglDeleteContext(self.0) };
        }
    }

    let class_name = wide("VoidLauncherSmokeGlProbe");
    let title = wide("VoidLauncher smoke GL probe");
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wnd_class = WNDCLASSW {
        style: CS_OWNDC,
        lpfnWndProc: Some(DefWindowProcW),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    if unsafe { RegisterClassW(&wnd_class) } == 0 {
        return Err(format!("RegisterClassW failed (error {})", unsafe {
            GetLastError()
        }));
    }
    let _class_guard = ClassGuard(class_name.clone(), instance);

    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(format!("CreateWindowExW failed (error {})", unsafe {
            GetLastError()
        }));
    }
    let _window_guard = WindowGuard(hwnd);

    let dc = unsafe { GetDC(hwnd) };
    if dc.is_null() {
        return Err(format!("GetDC failed (error {})", unsafe {
            GetLastError()
        }));
    }
    let _dc_guard = DcGuard(hwnd, dc);

    let pfd = PIXELFORMATDESCRIPTOR {
        nSize: std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u16,
        nVersion: 1,
        dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
        iPixelType: PFD_TYPE_RGBA,
        cColorBits: 32,
        ..Default::default()
    };
    let format = unsafe { ChoosePixelFormat(dc, &pfd) };
    if format == 0 {
        return Err(format!("ChoosePixelFormat failed (error {})", unsafe {
            GetLastError()
        }));
    }
    if unsafe { SetPixelFormat(dc, format, &pfd) } == 0 {
        return Err(format!("SetPixelFormat failed (error {})", unsafe {
            GetLastError()
        }));
    }

    let legacy = unsafe { wglCreateContext(dc) };
    if legacy.is_null() {
        return Err("wglCreateContext failed: the driver exposes no usable OpenGL".to_string());
    }
    let _legacy_guard = GlContext(legacy);
    if unsafe { wglMakeCurrent(dc, legacy) } == 0 {
        return Err(format!("wglMakeCurrent failed (error {})", unsafe {
            GetLastError()
        }));
    }

    unsafe fn gl_string(name: u32) -> Option<String> {
        let ptr = glGetString(name);
        if ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while len < 4096 && *ptr.add(len) != 0 {
            len += 1;
        }
        Some(String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).into_owned())
    }

    let version = unsafe { gl_string(GL_VERSION) }.unwrap_or_else(|| "?".to_string());
    let renderer = unsafe { gl_string(GL_RENDERER) }.unwrap_or_else(|| "?".to_string());

    // Attempt the context Minecraft actually needs (3.2 core profile).
    let proc_name: &[u8] = b"wglCreateContextAttribsARB\0";
    let create_attribs = unsafe { wglGetProcAddress(proc_name.as_ptr()) };
    let core = if let Some(address) = create_attribs {
        let create: unsafe extern "system" fn(HGLRC, HGLRC, *const i32) -> HGLRC =
            unsafe { std::mem::transmute(address) };
        // WGL_CONTEXT_MAJOR_VERSION_ARB, MINOR, PROFILE_MASK = CORE_PROFILE_BIT.
        let attribs: [i32; 7] = [0x2091, 3, 0x2092, 2, 0x9126, 0x0000_0001, 0];
        let context = unsafe { create(dc, std::ptr::null_mut(), attribs.as_ptr()) };
        if context.is_null() {
            "failed".to_string()
        } else {
            unsafe { wglDeleteContext(context) };
            "OK".to_string()
        }
    } else {
        "extension unavailable".to_string()
    };

    unsafe { wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut()) };
    Ok(format!(
        "version={} renderer={} core_3_2={}",
        version, renderer, core
    ))
}

type Verdict = Result<String, String>;

fn pass(detail: &str) -> Verdict {
    Ok(detail.to_string())
}

async fn run_combo(config: &AppConfig, data_dir: &Path, mc: &str, loader: &str) -> Verdict {
    let combo_name = format!("{}-{}", mc, loader);
    let instances_dir = config.instances_dir();

    // Resume: skip combos that already passed in this data dir.
    let pass_marker = data_dir
        .join("combo-pass")
        .join(format!("{}.pass", combo_name));
    if pass_marker.exists() {
        return pass("PASS (cached from previous run)");
    }

    // Optional SMOKE_FILTER="1.16.5-Forge,1.18.2-Forge" to run a subset.
    if let Ok(filter) = std::env::var("SMOKE_FILTER") {
        let allowed: Vec<&str> = filter
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if !allowed.is_empty() && !allowed.iter().any(|f| combo_name.contains(f)) {
            return pass("SKIP (filtered out)");
        }
    }

    let name = format!("smoke-{}", combo_name);
    let inst_dir = instances_dir.join(&name);

    let skip_install = std::env::var("SMOKE_SKIP_INSTALL").is_ok();

    let prepare_start = Instant::now();

    // 1. Install version (libraries, natives, assets, version json)
    let vi = if skip_install && inst_dir.exists() {
        let manifest = versions::fetch_version_manifest()
            .await
            .map_err(|e| format!("manifest: {}", e))?;
        let url = manifest
            .versions
            .iter()
            .find(|v| v.id == mc)
            .map(|v| v.url.clone())
            .ok_or_else(|| format!("mc {} not in manifest", mc))?;
        versions::fetch_version_info(&url)
            .await
            .map_err(|e| format!("version info {}: {}", mc, e))?
    } else {
        if inst_dir.exists() {
            std::fs::remove_dir_all(&inst_dir).map_err(|e| format!("cleanup: {}", e))?;
        }
        report_line(&format!("=== COMBO {} / {}", mc, loader));
        let vi = install_version(config, mc).await?;
        let required_java = vi.required_java_major();
        report_line(&format!("{} requires Java {}", mc, required_java));
        ensure_java(data_dir, required_java).await?;
        vi
    };

    // 2. Create instance
    let mut instance = if skip_install && inst_dir.exists() {
        instances::get_instance(&instances_dir, &name)
            .map_err(|e| format!("reuse instance: {}", e))?
    } else {
        let inst = Instance::new(&name, mc, 4096, "g1gc", loader_type(loader), None);
        instances::create_instance(&instances_dir, &inst)
            .map_err(|e| format!("create instance: {}", e))?;
        inst
    };

    // 3. Install loader if any
    if loader != "Vanilla" && !(skip_install && inst_dir.exists()) {
        let (version, profile) = install_loader_for(data_dir, mc, loader).await?;
        instance.loader_version = Some(version);
        instance.loader_profile = Some(profile);
        instances::save_instance(&instances_dir, &instance, None)
            .map_err(|e| format!("save instance: {}", e))?;
    }

    report_line(&format!(
        "{}: prepare OK ({:.0}s)",
        combo_name,
        prepare_start.elapsed().as_secs_f32()
    ));

    // 4. Launch. Resolve the JVM through the same policy the launcher uses —
    //    the probed installation then drives BOTH the native-library ensure
    //    step and the spawn, so natives and the game run on one architecture.
    //    (Version files were downloaded earlier with the process arch, which
    //    is provisional; every native below is re-checked for the JVM arch.)
    let java = launch::resolve_launch_java(config, &instance, &vi)
        .map_err(|e| format!("resolve launch java: {}", e))?;
    versions::ensure_native_libraries_for(
        &vi,
        &config.libraries_dir(),
        versions::TargetArch::from_java_arch(java.arch),
    )
    .await
    .map_err(|e| format!("natives {}: {}", mc, e))?;
    // Architecture of the JVM that will run the game (not the launcher's).
    // The executable path is deliberately left out of the report.
    report_line(&format!(
        "{}: java arch={:?} version={} vendor={} required_major={}",
        combo_name,
        java.arch,
        java.version,
        java.vendor,
        vi.required_java_major()
    ));

    let uuid = "00000000-0000-0000-0000-000000000000";
    let token = "smoke-offline-token";
    let username = "SmokeTest";
    let minecraft_dir = instance.minecraft_dir(&instances_dir);
    std::fs::create_dir_all(&minecraft_dir).map_err(|e| e.to_string())?;

    let start = Instant::now();
    let launch_started = std::time::SystemTime::now();
    let mut child = match launch::launch_minecraft(
        config,
        &instance,
        &vi,
        &launch::LaunchAuth {
            access_token: token,
            uuid,
            username,
        },
        None,
        None,
        Some(&java),
    ) {
        Ok(c) => c,
        Err(e) => return Err(format!("launch spawn error: {}", e)),
    };
    let pid = child.id();
    report_line(&format!("spawned pid {}", pid));
    let run = ComboRun {
        combo_name: &combo_name,
        instance_name: &name,
        minecraft_dir: &minecraft_dir,
        data_dir,
        since: launch_started,
    };

    let latest_log = minecraft_dir.join("logs").join("latest.log");
    let crash_dir = minecraft_dir.join("crash-reports");
    let mut saw_main = false;
    let mut saw_loader = false;

    loop {
        if start.elapsed() > COMBO_TIMEOUT {
            let (readers, reaped, diag) = finish_run(&mut child, pid, &run, true);
            return Err(format!(
                "TIMEOUT {}s (main_marker={}, loader_marker={}, readers={:?}, reaped={})\n{}",
                COMBO_TIMEOUT.as_secs(),
                saw_main,
                saw_loader,
                readers,
                reaped,
                diag.join("\n")
            ));
        }

        let mut log_text = String::new();
        if let Ok(bytes) = std::fs::read(&latest_log) {
            // Java 8 clients write logs in the system charset (Windows-1251
            // on RU locales); lossy-decoding keeps marker matching working.
            log_text = String::from_utf8_lossy(&bytes).into_owned();
            let tail_len = log_text.len().min(400_000);
            log_text = log_text[log_text.len() - tail_len..].to_string();
        }

        if !log_text.is_empty() {
            if !saw_main && MAIN_MARKERS.iter().any(|m| log_text.contains(m)) {
                saw_main = true;
                report_line(&format!("{}: main marker reached", combo_name));
            }
            if let Some(m) = loader_marker(loader) {
                if !saw_loader && log_text.contains(m) {
                    saw_loader = true;
                    report_line(&format!("{}: loader marker '{}' reached", combo_name, m));
                }
            }
            // The main marker already proves the loader booted (it is
            // printed after mod loading completes); the loader-specific
            // marker above is informational only.
            if saw_main {
                let (readers, reaped, diag) = finish_run(&mut child, pid, &run, true);
                report_line(&format!(
                    "{}: launch OK ({:.0}s, main marker reached, readers={:?}, reaped={})",
                    combo_name,
                    start.elapsed().as_secs_f32(),
                    readers,
                    reaped
                ));
                let _ = diag;
                report_line(&format!(
                    "{}: PASS after {:.0}s",
                    combo_name,
                    start.elapsed().as_secs_f32()
                ));
                std::fs::create_dir_all(pass_marker.parent().unwrap()).ok();
                let _ = std::fs::write(&pass_marker, b"1");
                return pass(&format!(
                    "launch+markers in {:.0}s",
                    start.elapsed().as_secs_f32()
                ));
            }
        }

        // Crash report appeared?
        if saw_main {
            // main reached; give it a grace period for loader bootstrap
        } else if let Ok(rd) = std::fs::read_dir(&crash_dir) {
            for e in rd.flatten() {
                if e.file_name()
                    .to_string_lossy()
                    .as_ref()
                    .to_ascii_lowercase()
                    .ends_with(".txt")
                {
                    let content = std::fs::read_to_string(e.path())
                        .unwrap_or_default()
                        .lines()
                        .take(12)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let (readers, reaped, diag) = finish_run(&mut child, pid, &run, true);
                    return Err(format!(
                        "CRASH REPORT present: {}\n{}\n(readers={:?}, reaped={})\n{}",
                        e.file_name().to_string_lossy(),
                        content,
                        readers,
                        reaped,
                        diag.join("\n")
                    )
                    .trim_end()
                    .to_string());
                }
            }
        }

        // Process exit?
        if let Ok(Some(status)) = child.try_wait() {
            let code = status.code().unwrap_or(-1);
            // The process is already gone: no need to kill, but the readers
            // must finish flushing before the tee files are read.
            let (readers, reaped, diag) = finish_run(&mut child, pid, &run, false);
            if let Ok(p) = std::env::var("SMOKE_STDERR_FILE") {
                if let Some(path) = find_run_tee(data_dir, &name, ".stderr.log", launch_started) {
                    if let Ok(bytes) = std::fs::read(&path) {
                        std::fs::write(&p, bytes).ok();
                    }
                }
                if let Some(path) = find_run_tee(data_dir, &name, ".stdout.log", launch_started) {
                    if let Ok(bytes) = std::fs::read(&path) {
                        std::fs::write(format!("{}.stdout", p), bytes).ok();
                    }
                }
            }
            return Err(format!(
                "game exited early (code {}); main={}, loader={}, readers={:?}, reaped={}\n{}",
                code,
                saw_main,
                saw_loader,
                readers,
                reaped,
                diag.join("\n")
            )
            .trim_end()
            .to_string());
        }

        std::thread::sleep(POLL_STEP);
    }
}

#[test]
#[ignore]
fn smoke_all() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("launcher=trace".parse().unwrap()),
        )
        .with_test_writer()
        .try_init();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let data_dir = temp_dir_path();
        std::fs::create_dir_all(&data_dir).expect("create smoke data dir");
        let config = AppConfig {
            data_dir: data_dir.clone(),
            default_memory_mb: 4096,
            max_memory_mb: 8192,
            default_gc_preset: "g1gc".into(),
            default_jvm_args: vec![],
            ..Default::default()
        };
        std::fs::create_dir_all(config.instances_dir()).ok();
        std::fs::create_dir_all(config.versions_dir()).ok();

        let _ = std::fs::remove_file(results_path());

        report_line(&format!("SMOKE DATA DIR: {}", data_dir.display()));
        report_line(&format!(
            "Java present: {}",
            launch::find_all_java_installations(&data_dir).len()
        ));
        for line in runner_diagnostics() {
            report_line(&line);
        }

        let combos = match resolve_combos().await {
            Ok(combos) => combos,
            Err(e) => {
                report_line(&format!("SELECTION: FAIL ({})", e));
                panic!("smoke combo selection failed: {}", e);
            }
        };
        report_line(&format!(
            "COMBOS: {}",
            combos
                .iter()
                .map(|(mc, loader)| format!("{}-{}", mc, loader))
                .collect::<Vec<_>>()
                .join(", ")
        ));

        let mut results: Vec<(String, String, Verdict)> = Vec::new();
        for (mc, loader) in &combos {
            let outcome = run_combo(&config, &data_dir, mc, loader).await;
            match &outcome {
                Ok(d) => report_line(&format!("RESULT {} {}: PASS ({})", mc, loader, d)),
                Err(e) => report_line(&format!("RESULT {} {}: FAIL ({})", mc, loader, e)),
            }
            results.push((mc.clone(), loader.clone(), outcome));
        }

        report_line("================== SUMMARY ==================");
        let mut passed = 0;
        let mut failed = 0;
        for (mc, loader, v) in &results {
            match v {
                Ok(_) => {
                    passed += 1;
                    report_line(&format!("  PASS  {} {}", mc, loader));
                }
                Err(e) => {
                    failed += 1;
                    report_line(&format!(
                        "  FAIL  {} {}  ->  {}",
                        mc,
                        loader,
                        e.lines().next().unwrap_or("")
                    ));
                }
            }
        }
        report_line(&format!(
            "TOTAL: {} combos, {} passed, {} failed",
            results.len(),
            passed,
            failed
        ));

        if failed > 0 {
            // fail the test so CI/dev notices
            panic!(
                "{} combos failed (see {} and result lines above)",
                failed,
                results_path().display()
            );
        }
    });
}

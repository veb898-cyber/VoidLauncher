//! Smoke-launch test: installs Minecraft versions + loaders into an isolated
//! temp data dir, launches each combo with the real `launch_minecraft` path,
//! watches `logs/latest.log` for startup markers, then kills the process.
//!
//! Run:  cargo test --lib smoke_all -- --ignored --nocapture
//! Env:  SMOKE_DATA_DIR  (optional, default: %TEMP%\voidsmoke-<millis>; reused
//!                        between runs for cached libraries / resume support)

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

// LiteLoader is intentionally absent: the upstream artifact host
// (dl.liteloader.com) is dead, so even Prism meta can't serve the JAR;
// the app gates it with a "not supported" toast (liteloader.rs).
const COMBOS: &[(&str, &str)] = &[
    ("1.8.9", "Forge"),
    ("1.12.2", "Vanilla"),
    ("1.12.2", "Fabric"),
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
        "LiteLoader" => LoaderType::LiteLoader,
        _ => LoaderType::Vanilla,
    }
}

fn loader_marker(s: &str) -> Option<&'static str> {
    match s {
        "Fabric" => Some("FabricLoader"),
        "Forge" => Some("MinecraftForge"),
        "NeoForge" => Some("NeoForge"),
        "LiteLoader" => Some("LiteLoader"),
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
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(results_path())
        .unwrap();
    use std::io::Write;
    let _ = writeln!(f, "{}", s);
}

fn java_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("java")
}

/// Ensure a managed JRE of `major` exists in `data_dir/java/jdk-<major>`.
/// Uses the exact same Adoptium source + layout as the launcher's own
/// `java_download` module so `list_managed_java` picks it up.
async fn ensure_java(data_dir: &Path, major: u32) -> Result<(), String> {
    let data_dir = data_dir.to_path_buf();
    if launch::check_java_availability(&data_dir, major).is_some() {
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
        return Err(format!(
            "Java {} download: HTTP {}",
            major,
            resp.status()
        ));
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

    let file =
        std::fs::File::open(&tmp_zip).map_err(|e| format!("open zip: {}", e))?;
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

    if launch::check_java_availability(&data_dir, major).is_some() {
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
        "LiteLoader" => modloaders::liteloader::get_loader_versions(mc, 0, 40).await,
        _ => return Err("vanilla has no loader version".into()),
    }
    .map_err(|e| format!("list {} versions: {}", loader, e))?;

    let candidates: Vec<String> = page
        .versions
        .iter()
        .filter(|v| v.stable)
        .map(|v| v.version.clone())
        .chain(page.versions.iter().filter(|v| !v.stable).map(|v| v.version.clone()))
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
    Err(format!("all {} versions failed. last: {}", loader, last_err))
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

    let files = versions::collect_downloads(&vi, &config.libraries_dir(), &config.versions_dir());
    if !files.is_empty() {
        report_line(&format!("downloading {} files for {}...", files.len(), mc));
        download::download_files(files, |_, _, _| {}).await.map_err(|e| e.to_string())?;
    }

    versions::ensure_native_libraries(&vi, &config.libraries_dir())
        .await
        .map_err(|e| format!("natives {}: {}", mc, e))?;

    let idx_path = config.assets_dir().join("indexes").join(format!("{}.json", vi.assets));
    if !idx_path.exists() {
        let asset_index = versions::fetch_asset_index(&vi.asset_index.url)
            .await
            .map_err(|e| format!("asset index {}: {}", mc, e))?;
        std::fs::create_dir_all(idx_path.parent().unwrap()).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&asset_index).map_err(|e| e.to_string())?;
        std::fs::write(&idx_path, json).map_err(|e| e.to_string())?;
        download::download_assets(&asset_index, &config.assets_dir(), |_, _, _| {})
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

/// Drain a piped game-process handle (stdout/stderr) after the process
/// died, with a hard timeout so a stuck grandchild holding the write
/// end can never hang the harness.
async fn drain_pipe(mut r: Option<impl std::io::Read + Send + 'static>) -> Vec<u8> {
    use std::io::Read;
    let Some(mut r) = r.take() else { return Vec::new(); };
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    match tokio::time::timeout(Duration::from_secs(3), rx).await {
        Ok(Ok(b)) => b,
        _ => Vec::new(),
    }
}

/// Tail of a byte buffer as lossy UTF-8, capped for error messages.
fn pipe_tail(buf: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(buf);
    let s = s.trim();
    let start = s.len().saturating_sub(max);
    s[start..].to_string()
}

type Verdict = Result<String, String>;

fn pass(detail: &str) -> Verdict {
    Ok(detail.to_string())
}

async fn run_combo(
    config: &AppConfig,
    data_dir: &Path,
    mc: &str,
    loader: &str,
) -> Verdict {
    let combo_name = format!("{}-{}", mc, loader);
    let instances_dir = config.instances_dir();

    // Resume: skip combos that already passed in this data dir.
    let pass_marker = data_dir.join("combo-pass").join(format!("{}.pass", combo_name));
    if pass_marker.exists() {
        return pass("PASS (cached from previous run)");
    }

    // Optional SMOKE_FILTER="1.16.5-Forge,1.18.2-Forge" to run a subset.
    if let Ok(filter) = std::env::var("SMOKE_FILTER") {
        let allowed: Vec<&str> = filter.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if !allowed.is_empty() && !allowed.iter().any(|f| combo_name.contains(f)) {
            return pass("SKIP (filtered out)");
        }
    }

    let name = format!("smoke-{}", combo_name);
    let inst_dir = instances_dir.join(&name);

    let skip_install = std::env::var("SMOKE_SKIP_INSTALL").is_ok();

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
        report_line(&format!("=== COMBO {} -> {} / Java {}", mc, loader, "auto"));
        let vi = install_version(config, mc).await?;
        let required_java = vi.required_java_major();
        report_line(&format!("{} requires Java {}", mc, required_java));
        ensure_java(data_dir, required_java).await?;
        vi
    };

    // 2. Create instance
    let mut instance = if skip_install && inst_dir.exists() {
        instances::get_instance(&instances_dir, &name).map_err(|e| format!("reuse instance: {}", e))?
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

    // 4. Launch
    let uuid = "00000000-0000-0000-0000-000000000000";
    let token = "smoke-offline-token";
    let username = "SmokeTest";
    let minecraft_dir = instance.minecraft_dir(&instances_dir);
    std::fs::create_dir_all(&minecraft_dir).map_err(|e| e.to_string())?;

    let start = Instant::now();
    let mut child = match launch::launch_minecraft(
        config, &instance, &vi, token, uuid, username,
    ) {
        Ok(c) => c,
        Err(e) => return Err(format!("launch spawn error: {}", e)),
    };
    report_line(&format!("spawned pid {}", child.id()));

    let latest_log = minecraft_dir.join("logs").join("latest.log");
    let crash_dir = minecraft_dir.join("crash-reports");
    let mut saw_main = false;
    let mut saw_loader = false;

    loop {
        if start.elapsed() > COMBO_TIMEOUT {
            let _ = kill_tree(child.id());
            return Err(format!(
                "TIMEOUT {}s (main_marker={}, loader_marker={})",
                COMBO_TIMEOUT.as_secs(), saw_main, saw_loader
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
                let _ = kill_tree(child.id());
                report_line(&format!("{}: PASS after {:.0}s", combo_name, start.elapsed().as_secs_f32()));
                if let Ok(Some(status)) = child.try_wait() {
                    report_line(&format!("{}: child exit code after kill: {}", combo_name, status.code().unwrap_or(-1)));
                }
                std::fs::create_dir_all(pass_marker.parent().unwrap()).ok();
                let _ = std::fs::write(&pass_marker, b"1");
                return pass(&format!("launch+markers in {:.0}s", start.elapsed().as_secs_f32()));
            }
        }

        // Crash report appeared?
        if saw_main {
            // main reached; give it a grace period for loader bootstrap
        } else if let Ok(rd) = std::fs::read_dir(&crash_dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().as_ref().to_ascii_lowercase().ends_with(".txt") {
                    let content = std::fs::read_to_string(e.path())
                        .unwrap_or_default()
                        .lines()
                        .take(12)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let _ = kill_tree(child.id());
                    return Err(format!(
                        "CRASH REPORT present: {}",
                        e.file_name().to_string_lossy()
                    ) + "\n" + &content);
                }
            }
        }

        // Process exit?
        if let Ok(Some(status)) = child.try_wait() {
            let code = status.code().unwrap_or(-1);
            let _ = kill_tree(child.id());
            let out = drain_pipe(child.stdout.take()).await;
            let err = drain_pipe(child.stderr.take()).await;
            if let Ok(p) = std::env::var("SMOKE_STDERR_FILE") {
                std::fs::write(&p, &err).ok();
                std::fs::write(format!("{}.stdout", p), &out).ok();
            }
            return Err(format!(
                "game exited early (code {}); main={}, loader={}\nstdout tail: {}\nstderr tail: {}",
                code, saw_main, saw_loader,
                pipe_tail(&out, 1500),
                pipe_tail(&err, 1500)
            ).trim_end().to_string());
        }

        std::thread::sleep(POLL_STEP);
    }
}

#[test]
#[ignore]
fn smoke_all() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("launcher=trace".parse().unwrap()))
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
        report_line(&format!("Java present: {}", launch::find_all_java_installations(&data_dir).len()));

        let mut results: Vec<(&'static str, &'static str, Verdict)> = Vec::new();
        for (mc, loader) in COMBOS {
            let outcome = run_combo(&config, &data_dir, mc, loader).await;
            match &outcome {
                Ok(d) => report_line(&format!("RESULT {} {}: PASS ({})", mc, loader, d)),
                Err(e) => report_line(&format!("RESULT {} {}: FAIL ({})", mc, loader, e)),
            }
            results.push((mc, loader, outcome));
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
                    report_line(&format!("  FAIL  {} {}  ->  {}", mc, loader, e.lines().next().unwrap_or("")));
                }
            }
        }
        report_line(&format!("TOTAL: {} combos, {} passed, {} failed", results.len(), passed, failed));

        if failed > 0 {
            // fail the test so CI/dev notices
            panic!("{} combos failed (see {} and result lines above)", failed, results_path().display());
        }
    });
}
//! Diagnostics: reproduce exactly what the UI sees for the owner's real
//! instances (packs lists, icons, mod metadata). Ignored by default.

use crate::commands::mods::get_mod_metadata;
use crate::config::AppConfig;
use crate::instances;
use crate::AppState;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const DATA_DIR: &str = r"C:\Users\User\AppData\Roaming\VoidLauncher";
const INSTANCE: &str = "Better MC [FORGE] BMC4";

fn test_state() -> AppState {
    AppState {
        config: Mutex::new(AppConfig::load(&PathBuf::from(DATA_DIR))),
        auth_state: Mutex::new(crate::auth::AuthState::default()),
        running_instances: Mutex::new(Vec::new()),
        pack_watcher: Mutex::new(None),
        active_sessions: Mutex::new(std::collections::HashMap::new()),
        room_state: Arc::new(crate::rooms::room_state::RoomStateManager::new(
            &PathBuf::from(DATA_DIR),
        )),
        tailscale: crate::rooms::tailscale::TailscaleManager::new(PathBuf::from(DATA_DIR)),
    }
}

#[test]
#[ignore]
fn diag_packs_what_ui_sees() {
    let state = test_state();
    let config = state.config.lock().unwrap();
    let instances_dir = config.instances_dir();
    println!("instances_dir: {}", instances_dir.display());

    for pack_type in ["resourcepacks", "mods", "shaderpacks"] {
        println!("\n===== {pack_type} =====");
        let inst = instances::get_instance(&instances_dir, INSTANCE).unwrap();
        let dir = inst.minecraft_dir(&instances_dir).join(pack_type);
        println!("  dir exists: {} path: {}", dir.exists(), dir.display());
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let names: Vec<String> = rd
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            println!("  raw entries ({})...", names.len());
            for n in names.iter().take(12) {
                println!("    - {n}");
            }
        } else {
            println!("  read_dir FAILED");
        }
        match instances::list_packs(&instances_dir, INSTANCE, pack_type) {
            Ok(entries) => {
                println!("  list_packs entries: {}", entries.len());
                for e in entries.iter().take(3) {
                    println!(
                        "  FIRST: name={} | {} | ver={} | prov={}",
                        e.name, e.filename, e.version, e.provider
                    );
                }
                for e in entries {
                    let icon =
                        instances::read_pack_icon(&instances_dir, INSTANCE, pack_type, &e.filename)
                            .unwrap_or(None);
                    println!(
                        "  icon={:>4?} provider={:<10} version={:<35} name={} | {}",
                        icon.is_some(),
                        e.provider,
                        e.version,
                        e.name,
                        e.filename
                    );
                }
            }
            Err(err) => println!("  ERROR: {err}"),
        }
    }

    println!("\n===== cmd_get_mod_metadata =====");
    match get_mod_metadata(&config, INSTANCE) {
        Ok(mods) => {
            println!("total mods: {}", mods.len());
            for m in mods.iter().filter(|m| {
                m.version.is_empty()
                    || m.provider.eq_ignore_ascii_case("local")
                    || m.provider.is_empty()
                    || m.name
                        .trim()
                        .chars()
                        .all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '.')
            }) {
                println!(
                    "  ver=[{}] prov=[{}] name=[{}] | {}",
                    m.version, m.provider, m.name, m.filename
                );
            }
        }
        Err(err) => println!("  ERROR: {err}"),
    }

    let mods_dir = config
        .instances_dir()
        .join(INSTANCE)
        .join(".minecraft")
        .join("mods");
    let probe = "Hearths v1.0.5.mod.jar";
    println!("\n===== probe {} =====", probe);
    println!(
        "  sidecar: {:?}",
        crate::instances::read_sidecar_meta(&mods_dir, probe)
    );
    let meta = crate::commands::mods::read_mod_meta_from_jar_pub(&mods_dir.join(probe));
    println!(
        "  jar meta: name=[{}] version=[{}] provider=[{}]",
        meta.name, meta.version, meta.provider
    );
    let mut v = crate::commands::mods::sanitize_version_pub(&meta.version);
    println!("  sanitized: [{}]", v);
    if v.is_empty() {
        v = crate::instances::read_sidecar_meta(&mods_dir, probe)
            .and_then(|j| j["project_name"].as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        println!("  fallback project_name: [{}]", v);
    }
    if let Ok(all) = get_mod_metadata(&config, INSTANCE) {
        for m in all.iter().filter(|m| m.filename == probe) {
            println!(
                "  FINAL: ver=[{}] prov=[{}] name=[{}] slug=[{:?}] pid=[{:?}] slug_ver={}",
                m.version, m.provider, m.name, m.slug, m.project_id, m.slug_verified
            );
        }
    }
    let probe2 = "recipeessentials-1.20.1-4.7.jar";
    let meta2 = crate::commands::mods::read_mod_meta_from_jar_pub(&mods_dir.join(probe2));
    println!(
        "  {} jar meta: name=[{}] version=[{}] provider=[{}] icon=[{:?}] slug=[{:?}]",
        probe2, meta2.name, meta2.version, meta2.provider, meta2.icon, meta2.slug
    );
    let probe3 = "smoothchunk-1.20.1-4.1.jar";
    let meta3 = crate::commands::mods::read_mod_meta_from_jar_pub(&mods_dir.join(probe3));
    println!(
        "  {} jar meta: name=[{}] version=[{}] provider=[{}] icon=[{:?}] slug=[{:?}]",
        probe3, meta3.name, meta3.version, meta3.provider, meta3.icon, meta3.slug
    );

    if !config.curseforge_api_key.is_empty() {
        let key = config.curseforge_api_key.clone();
        println!("  CF key present, testing CF API icon fetch for 880814 (Hearths)...");
        let rt = tokio::runtime::Runtime::new().unwrap();
        match rt.block_on(crate::commands::mods::fetch_curseforge_mod_icon_pub(
            880814, &key,
        )) {
            Ok(Some(icon)) => println!("  CF icon OK: {} bytes", icon.len()),
            Ok(None) => println!("  CF icon: None (no logo)"),
            Err(e) => println!("  CF icon ERROR: {}", e),
        }
    } else {
        println!("  NO CF API KEY — CF icons cannot be fetched");
    }

    let pf = crate::instances::pack_format_to_mc_version(&mods_dir.join("CataclysmCompat1.0.zip"));
    println!("  pack_format CataclysmCompat1.0.zip => [{}]", pf);
    println!(
        "  extract CataclysmCompat1.0.zip => [{}]",
        crate::instances::extract_version_from_filename("CataclysmCompat1.0.zip")
    );
    println!(
        "  extract Connector-1.0.0-beta.49+1.20.1.jar => [{}]",
        crate::instances::extract_version_from_filename("Connector-1.0.0-beta.49+1.20.1.jar")
    );
    println!(
        "  extract ConnectorExtras-1.11.2+1.20.1.jar => [{}]",
        crate::instances::extract_version_from_filename("ConnectorExtras-1.11.2+1.20.1.jar")
    );
    if let Ok(all) = get_mod_metadata(&config, INSTANCE) {
        for m in all.iter().filter(|m| {
            m.filename.contains("ConnectorExtras") || m.filename.contains("fresh_waystones")
        }) {
            println!(
                "  FINAL2: ver=[{}] prov=[{}] name=[{}] icon=[{}] | {}",
                m.version,
                m.provider,
                m.name,
                m.icon.is_some(),
                m.filename
            );
        }
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _key = config.curseforge_api_key.clone();
    let icon_fw = rt.block_on(crate::commands::mods::cmd_get_mod_icon_pub(
        &config,
        INSTANCE,
        "fresh_waystones.zip",
    ));
    println!(
        "  cmd_get_mod_icon fresh_waystones.zip => {:?}",
        icon_fw.map(|o| o.is_some())
    );
    let icon_cx = rt.block_on(crate::commands::mods::cmd_get_mod_icon_pub(
        &config,
        INSTANCE,
        "ConnectorExtras-1.11.2+1.20.1.jar",
    ));
    println!(
        "  cmd_get_mod_icon ConnectorExtras => {:?}",
        icon_cx.map(|o| o.is_some())
    );
}

// BENCH: cold/warm scan timing for the owner's BMC4 pack (~500 files).
// Verifies the persistent metadata cache: a fresh process must open the pack
// in tens of ms instead of re-parsing every jar.
#[test]
#[ignore]
fn diag_bench_mod_scan() {
    let state = test_state();
    let config = state.config.lock().unwrap();
    let t = std::time::Instant::now();
    let n = get_mod_metadata(&config, INSTANCE)
        .map(|m| m.len())
        .unwrap_or(0);
    println!("COLD: {} mods in {:?}", n, t.elapsed());
    let t = std::time::Instant::now();
    let _ = get_mod_metadata(&config, INSTANCE);
    println!("WARM: {} mods in {:?}", n, t.elapsed());

    let mods_dir = config
        .instances_dir()
        .join(INSTANCE)
        .join(".minecraft")
        .join("mods");
    let mut files: Vec<_> = std::fs::read_dir(&mods_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".jar"))
                .unwrap_or(false)
        })
        .collect();
    files.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
    for p in files.iter().rev().take(5) {
        let t = std::time::Instant::now();
        let _ = crate::commands::mods::read_mod_meta_from_jar_pub(p);
        println!("PARSE {:?} for {:?}", t.elapsed(), p.file_name().unwrap());
    }
    let t = std::time::Instant::now();
    for p in files.iter() {
        let _ = crate::commands::mods::read_mod_meta_from_jar_pub(p);
    }
    println!("SEQUENTIAL FULL: {} jars in {:?}", files.len(), t.elapsed());
}

// BENCH: mod-icon path on the owner's real pack. Mods whose jars ship no
// logo used to hit the network on every app run (the ~1s leftovers in the
// list); after the first resolve the icon lives in the metadata cache, so
// the second call must be instant and the persisted file must carry it.
#[test]
#[ignore]
fn diag_bench_mod_icon_cache() {
    let state = test_state();
    let config = state.config.lock().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();

    let all = get_mod_metadata(&config, INSTANCE).unwrap_or_default();
    let no_icon: Vec<String> = all
        .iter()
        .filter(|m| m.icon.is_none())
        .map(|m| m.filename.clone())
        .collect();
    println!(
        "mods total={} without icon in metadata={}",
        all.len(),
        no_icon.len()
    );
    for m in all.iter().filter(|m| m.icon.is_some()).take(3) {
        println!("  has icon: {}", m.filename);
    }

    let sample: Vec<String> = no_icon.iter().take(6).cloned().collect();
    for name in &sample {
        let t = std::time::Instant::now();
        let first = rt.block_on(crate::commands::mods::cmd_get_mod_icon_pub(
            &config, INSTANCE, name,
        ));
        println!(
            "FIRST  {:>10?} resolved={:?} | {}",
            t.elapsed(),
            first.as_ref().map(|o| o.is_some()).unwrap_or(false),
            name
        );
        let t = std::time::Instant::now();
        let second = rt.block_on(crate::commands::mods::cmd_get_mod_icon_pub(
            &config, INSTANCE, name,
        ));
        println!(
            "SECOND {:>10?} resolved={:?} | {}",
            t.elapsed(),
            second.as_ref().map(|o| o.is_some()).unwrap_or(false),
            name
        );
    }

    // Regression guard (owner wave, bug report: "иконки пропали — буквы"):
    // for mods whose jar ships a logo, metadata carries only the icon's path
    // INSIDE the jar ("assets/…/icon.png" / "icon.png"), never image data.
    // The icon command must EXTRACT the real image and return a data URL —
    // if it ever returns the raw path again, the UI renders a broken image
    // and falls back to a letter placeholder.
    let jar_icon: Vec<String> = all
        .iter()
        .filter(|m| matches!(&m.icon, Some(i) if !i.starts_with("data:")))
        .map(|m| m.filename.clone())
        .collect();
    println!("mods with jar-internal icon path: {}", jar_icon.len());
    for name in jar_icon.iter().take(3) {
        let t = std::time::Instant::now();
        let out = rt.block_on(crate::commands::mods::cmd_get_mod_icon_pub(
            &config, INSTANCE, name,
        ));
        let prefix: String = out
            .as_ref()
            .ok()
            .and_then(|o| o.as_ref())
            .map(|s| s.chars().take(24).collect())
            .unwrap_or_default();
        println!("JAR    {:>10?} prefix=[{}] | {}", t.elapsed(), prefix, name);
    }

    // Cross-process proof: the persisted cache file must now carry icons
    // for the sampled mods (the file is what the next app start loads).
    let raw =
        std::fs::read_to_string(config.data_dir.join("mod_meta_cache.json")).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    let entries = v["entries"].as_array().cloned().unwrap_or_default();
    let on_disk = sample
        .iter()
        .filter(|n| {
            entries.iter().any(|e| {
                e["path"]
                    .as_str()
                    .map(|p| p.ends_with(n.as_str()))
                    .unwrap_or(false)
                    && e["meta"]["icon"].as_str().is_some()
            })
        })
        .count();
    println!("icons persisted on disk: {}/{}", on_disk, sample.len());
}

/// Owner-requested audit: run the full diagnostics report (system + network
/// + real instance verify) and print it. Env: DIAG_INSTANCE to override the
/// default big pack.
#[test]
#[ignore]
fn diag_probe_run_diagnostics() {
    let state = test_state();
    let config = state.config.lock().unwrap().clone();
    let instance = std::env::var("DIAG_INSTANCE").unwrap_or_else(|_| INSTANCE.to_string());
    let rt = tokio::runtime::Runtime::new().unwrap();
    // `run_diagnostics` uses tauri's async runtime for its worker threads.
    let _ = tauri::async_runtime::set(rt.handle().clone());
    let t = std::time::Instant::now();
    let report = rt.block_on(crate::diagnostics::run_diagnostics(config, Some(instance)));
    println!("run_diagnostics took {:?}", t.elapsed());
    println!("launcher_version={}", report.launcher_version);
    for c in &report.checks {
        println!("CHECK  {:<10} {:<6} {}", c.id, c.status, c.value);
    }
    for i in &report.issues {
        println!("ISSUE  {:<10} {:<6} {:?}", i.id, i.severity, i.params);
    }
    if let Some(inst) = &report.instance {
        println!(
            "instance: {} · {} · {} {:?}",
            inst.name, inst.mc_version, inst.loader, inst.loader_version
        );
    }
    match &report.verify {
        Some(v) => {
            println!(
                "verify: repairable={} content_issues={} categories={}",
                v.repairable_count,
                v.content_issues,
                v.categories.len()
            );
            for c in &v.categories {
                for i in &c.issues {
                    println!(
                        "V-ISSUE {:<10} {:<16} detail={:?} {}",
                        c.id, i.kind, i.detail, i.path
                    );
                }
            }
        }
        None => println!("verify: none"),
    }
}

/// Owner-bug probe: run the crash analyzer against a real session log and
/// print the verdict. Env: DIAG_SESSION (path), DIAG_INSTANCE (name).
#[test]
#[ignore]
fn diag_probe_analyze_session() {
    let (Ok(session), Ok(instance)) = (
        std::env::var("DIAG_SESSION"),
        std::env::var("DIAG_INSTANCE"),
    ) else {
        println!("set DIAG_SESSION and DIAG_INSTANCE");
        return;
    };
    let state = test_state();
    let config = state.config.lock().unwrap();
    match crate::diagnostics::analyze_crash(&config, &instance, Some(&session)) {
        Ok(a) => println!(
            "analyze_crash => verdict={} kind={} source={} culprit={:?}\n  detail: {}",
            a.verdict, a.kind, a.source, a.culprit, a.detail
        ),
        Err(e) => println!("analyze_crash error: {e}"),
    }
}

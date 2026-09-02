//! Diagnostics: reproduce exactly what the UI sees for the owner's real
//! instances (packs lists, icons, mod metadata). Ignored by default.

use crate::commands::mods::get_mod_metadata;
use crate::config::AppConfig;
use crate::instances;
use crate::AppState;
use std::path::PathBuf;
use std::sync::Mutex;

const DATA_DIR: &str = r"C:\Users\User\AppData\Roaming\VoidLauncher";
const INSTANCE: &str = "Better MC [FORGE] BMC4";

fn test_state() -> AppState {
    AppState {
        config: Mutex::new(AppConfig::load(&PathBuf::from(DATA_DIR))),
        auth_state: Mutex::new(crate::auth::AuthState::default()),
        running_instances: Mutex::new(Vec::new()),
        pack_watcher: Mutex::new(None),
        active_sessions: Mutex::new(std::collections::HashMap::new()),
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
                    let icon = instances::read_pack_icon(
                        &instances_dir,
                        INSTANCE,
                        pack_type,
                        &e.filename,
                    )
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
                    || m.name.trim().chars().all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '.')
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
    println!("  sidecar: {:?}", crate::instances::read_sidecar_meta(&mods_dir, probe));
    let meta = crate::commands::mods::read_mod_meta_from_jar_pub(&mods_dir.join(probe));
    println!("  jar meta: name=[{}] version=[{}] provider=[{}]", meta.name, meta.version, meta.provider);
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
    println!("  {} jar meta: name=[{}] version=[{}] provider=[{}] icon=[{:?}] slug=[{:?}]", probe2, meta2.name, meta2.version, meta2.provider, meta2.icon, meta2.slug);
    let probe3 = "smoothchunk-1.20.1-4.1.jar";
    let meta3 = crate::commands::mods::read_mod_meta_from_jar_pub(&mods_dir.join(probe3));
    println!("  {} jar meta: name=[{}] version=[{}] provider=[{}] icon=[{:?}] slug=[{:?}]", probe3, meta3.name, meta3.version, meta3.provider, meta3.icon, meta3.slug);

    if !config.curseforge_api_key.is_empty() {
        let key = config.curseforge_api_key.clone();
        println!("  CF key present, testing CF API icon fetch for 880814 (Hearths)...");
        let rt = tokio::runtime::Runtime::new().unwrap();
        match rt.block_on(crate::commands::mods::fetch_curseforge_mod_icon_pub(880814, &key)) {
            Ok(Some(icon)) => println!("  CF icon OK: {} bytes", icon.len()),
            Ok(None) => println!("  CF icon: None (no logo)"),
            Err(e) => println!("  CF icon ERROR: {}", e),
        }
    } else {
        println!("  NO CF API KEY — CF icons cannot be fetched");
    }

    let pf = crate::instances::pack_format_to_mc_version(&mods_dir.join("CataclysmCompat1.0.zip"));
    println!("  pack_format CataclysmCompat1.0.zip => [{}]", pf);
    println!("  extract CataclysmCompat1.0.zip => [{}]", crate::instances::extract_version_from_filename("CataclysmCompat1.0.zip"));
    println!("  extract Connector-1.0.0-beta.49+1.20.1.jar => [{}]", crate::instances::extract_version_from_filename("Connector-1.0.0-beta.49+1.20.1.jar"));
    println!("  extract ConnectorExtras-1.11.2+1.20.1.jar => [{}]", crate::instances::extract_version_from_filename("ConnectorExtras-1.11.2+1.20.1.jar"));
    if let Ok(all) = get_mod_metadata(&config, INSTANCE) {
        for m in all.iter().filter(|m| m.filename.contains("ConnectorExtras") || m.filename.contains("fresh_waystones")) {
            println!("  FINAL2: ver=[{}] prov=[{}] name=[{}] icon=[{}] | {}", m.version, m.provider, m.name, m.icon.is_some(), m.filename);
        }
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let key = config.curseforge_api_key.clone();
    let icon_fw = rt.block_on(crate::commands::mods::cmd_get_mod_icon_pub(&config, INSTANCE, "fresh_waystones.zip"));
    println!("  cmd_get_mod_icon fresh_waystones.zip => {:?}", icon_fw.map(|o| o.is_some()));
    let icon_cx = rt.block_on(crate::commands::mods::cmd_get_mod_icon_pub(&config, INSTANCE, "ConnectorExtras-1.11.2+1.20.1.jar"));
    println!("  cmd_get_mod_icon ConnectorExtras => {:?}", icon_cx.map(|o| o.is_some()));
}
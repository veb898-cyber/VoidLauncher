mod accounts;
mod atlauncher;
mod auth;
mod commands;
mod config;
mod curseforge;
mod download;
mod entry;
mod error;
mod events;
mod game_logs;
mod i18n;
mod import;
mod instances;
mod java;
mod java_download;
mod jvm;
mod launch;
mod logger;
mod modloaders;
mod modrinth;
mod playtime;
mod versions;

#[cfg(test)]
mod smoke_launch;

#[cfg(test)]
mod diag;

use config::AppConfig;
use std::sync::Mutex;
use std::time::Instant;
use tauri::{Manager, WindowEvent};

/// Global app state shared across all Tauri commands
pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub auth_state: Mutex<auth::AuthState>,
    pub running_instance_id: Mutex<Option<String>>,
    pub pack_watcher: Mutex<Option<PackWatcherHandle>>,
    /// Active playtime-tracking session, if a game is running
    pub active_session: Mutex<Option<playtime::ActiveSession>>,
}

/// Handle to an active file system watcher; dropping stops the watcher
pub struct PackWatcherHandle {
    pub instance_name: String,
    _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
}

/// Returns true if the URL's host is in the allowlist of trusted
/// download mirrors (Modrinth, CurseForge, Mojang). Used by every
/// command that downloads a file from a URL supplied by the frontend
/// or by an upstream API, to prevent SSRF / file:// / mixed-content
/// downgrade attacks.
pub(crate) fn is_allowed_download_host(url: &str) -> bool {
    let url = url.trim();
    if !url.to_ascii_lowercase().starts_with("https://") {
        return false;
    }
    let after_scheme = &url[8..];
    let host_end = after_scheme
        .find(|c: char| c == '/' || c == ':' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let host = after_scheme[..host_end].to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    download::is_host_allowed(&host)
}



// ==================== App Builder ====================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Panic hook: write to a known file so we can debug crashes
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!("{}", info);
        let _ = std::fs::write(
            std::env::temp_dir().join("voidlauncher_panic.txt"),
            &msg,
        );
        if let Some(d) = dirs::data_dir() {
            let _ = std::fs::write(d.join("VoidLauncher").join("panic.log"), &msg);
        }
        orig_hook(info);
    }));

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("VoidLauncher");

    // File logger MUST be initialized first, before any other code path
    // that might emit a tracing event or call eprintln! (e.g. AppConfig::load
    // when the on-disk config is corrupt).
    logger::init(&data_dir);

    let config = AppConfig::load(&data_dir);
    let mut auth_state = auth::load_auth_state(&config.auth_file()).unwrap_or_default();

    // One-time cleanup of the retired persistent icon cache. It grew to
    // ~140 MB and froze the UI on first open; icons now live only in
    // renderer memory for the session (same as Prism Launcher).
    let _ = std::fs::remove_file(data_dir.join("icon_cache.json"));

    download::set_global_proxy(config.proxy_url());
    atlauncher::set_atl_cache_dir(config.data_dir.clone());

    tracing::info!(target: "launcher", "Data dir: {}", data_dir.display());
    tracing::info!(target: "launcher", "Config: data_dir={}, default_memory_mb={}, gc={}",
        config.data_dir.display(), config.default_memory_mb, config.default_gc_preset);

    // Multi-account migration: give the cached Microsoft session (if any) its
    // own per-account vault slot, then make sure the ACTIVE session matches
    // the default account: a different default Microsoft account loads its
    // own stored session; a default offline/Ely.by account hides Microsoft
    // from the UI (its tokens stay safe in the vault for re-activation).
    if let Some(ref profile) = auth_state.profile {
        tracing::info!(target: "launcher", "Restoring cached Microsoft session for {}", profile.name);
        match accounts::upsert_microsoft_account(&data_dir, &profile.name, &profile.id) {
            Ok(entry) => {
                if accounts::load_ms_session(&entry.id).is_none() {
                    // The vault write can transiently fail (Credential Manager
                    // contention right after boot). Retry once; if it still
                    // fails, log loudly — the account would otherwise look
                    // signed-out on the Accounts page while still launching.
                    match accounts::store_ms_session(&entry.id, &auth_state) {
                        Ok(()) => {}
                        Err(e1) => {
                            std::thread::sleep(std::time::Duration::from_millis(300));
                            if let Err(e2) = accounts::store_ms_session(&entry.id, &auth_state) {
                                tracing::error!(
                                    target: "launcher",
                                    "Could not migrate Microsoft session of '{}' to per-account vault slot: {} / {}",
                                    profile.name, e1, e2
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(target: "launcher", "Failed to register Microsoft account {}: {}", profile.name, e);
            }
        }
    }
    {
        let list = accounts::list_accounts(&data_dir);
        let active_uuid = auth_state.profile.as_ref().map(|p| p.id.as_str());
        let default_acc = list.iter().find(|a| a.default).or_else(|| list.first());
        match default_acc {
            Some(acc) if acc.account_type == accounts::AccountType::Microsoft => {
                let matches = acc.uuid.as_deref().zip(active_uuid).map(|(u, a)| u == a);
                if matches != Some(true) {
                    if let Some(session) = accounts::load_ms_session(&acc.id) {
                        tracing::info!(
                            target: "launcher",
                            "Activating stored session of default account '{}'",
                            acc.name
                        );
                        auth_state = session;
                    }
                }
            }
            Some(_) => {
                if auth_state.profile.is_some() {
                    tracing::info!(target: "launcher", "Default account is not a Microsoft one; hiding Microsoft session from UI");
                    auth_state = auth::AuthState::default();
                }
            }
            None => {}
        }
    }

    tauri::Builder::default()
        .setup(|app| {
            events::set_app_handle(app.handle());
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(
            tauri_plugin_updater::Builder::new()
                // GitHub API contents endpoint (fallback to raw.githubusercontent.com)
                // needs this Accept header to return the raw file content
                .header("Accept", "application/vnd.github.raw+json")
                .expect("invalid updater header")
                .build(),
        )
        .manage(AppState {
            config: Mutex::new(config),
            auth_state: Mutex::new(auth_state),
            running_instance_id: Mutex::new(None),
            pack_watcher: Mutex::new(None),
            active_session: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            commands::auth::cmd_start_login,
            commands::auth::cmd_poll_login,
            commands::auth::cmd_get_auth_state,
            commands::auth::cmd_logout,
            commands::auth::cmd_can_launch_offline,
            commands::auth::cmd_get_offline_credentials,
            commands::versions::cmd_get_versions,
            commands::versions::cmd_get_version_info,
            commands::instances::cmd_list_instances,
            commands::instances::cmd_create_instance,
            commands::instances::cmd_delete_instance,
            commands::instances::cmd_get_instance,
            commands::instances::cmd_save_instance,
            commands::versions::cmd_detect_java,
            commands::versions::cmd_list_available_java,
            commands::versions::cmd_download_java,
            commands::versions::cmd_list_managed_java,
            commands::versions::cmd_remove_managed_java,
            commands::launcher::cmd_install_version,
            commands::launcher::cmd_launch_game,
            commands::launcher::cmd_get_fabric_versions,
            commands::launcher::cmd_get_forge_versions,
            commands::launcher::cmd_get_neoforge_versions,
            commands::launcher::cmd_install_fabric,
            commands::launcher::cmd_install_forge,
            commands::launcher::cmd_install_neoforge,
            commands::launcher::cmd_check_instance_loader,
            commands::launcher::cmd_install_instance_loader,
            commands::misc::cmd_get_config,
            commands::misc::cmd_save_config,
            commands::misc::cmd_read_image_file,
            commands::misc::cmd_fetch_page_asset,
            commands::misc::cmd_check_latest_version,
            commands::misc::cmd_get_launch_state,
            commands::instances::cmd_check_instance_installed,
            commands::misc::cmd_detect_system_ram,
            commands::mods::cmd_search_modrinth,
            commands::mods::cmd_search_curseforge,
            commands::mods::cmd_get_modrinth_versions,
            commands::mods::cmd_get_curseforge_files,
            commands::mods::cmd_install_mod,
            commands::mods::cmd_get_modrinth_project,
            commands::mods::cmd_get_modrinth_version_by_id,
            commands::mods::cmd_popular_modrinth,
            commands::mods::cmd_popular_curseforge,
            commands::mods::cmd_get_curseforge_mod_detail,
            commands::mods::cmd_get_modrinth_project_body,
            commands::mods::cmd_check_mod_updates,
            commands::auth::cmd_list_accounts,
            commands::auth::cmd_add_offline_account,
            commands::auth::cmd_add_elyby_account,
            commands::auth::cmd_remove_account,
            commands::auth::cmd_set_default_account,
            commands::auth::cmd_change_skin,
            commands::auth::cmd_get_skin_path,
            commands::instances::cmd_get_instance_dir,
            commands::mods::cmd_list_instance_mods,
            commands::mods::cmd_remove_instance_mod,
            commands::mods::cmd_get_mod_metadata,
            commands::mods::cmd_get_mod_icon,
            commands::mods::cmd_fetch_icon_url,
            commands::launcher::cmd_emit_log,
            commands::launcher::cmd_list_game_logs,
            commands::launcher::cmd_read_game_log,
            commands::launcher::cmd_get_current_game_log,
            commands::launcher::cmd_delete_game_log,
            commands::launcher::cmd_open_instance_logs_folder,
            commands::launcher::cmd_open_game_logs_root,
            commands::misc::cmd_rename_file,
            commands::misc::cmd_delete_file,
            commands::mods::cmd_download_to_folder,
            commands::instances::cmd_duplicate_instance,
            commands::instances::cmd_import_prism_instance,
            commands::instances::cmd_export_instance,
            commands::instances::cmd_probe_modpack,
            commands::instances::cmd_import_modpack,
            commands::instances::cmd_set_instance_icon,
            commands::instances::cmd_set_instance_banner,
            commands::instances::cmd_log_toast,
            commands::instances::cmd_list_saves,
            commands::instances::cmd_rename_world,
            commands::instances::cmd_copy_world,
            commands::instances::cmd_delete_world,
            commands::instances::cmd_list_screenshots,
            commands::instances::cmd_delete_screenshot,
            commands::instances::cmd_read_screenshot,
            commands::instances::cmd_list_packs,
            commands::instances::cmd_get_pack_icon,
            commands::instances::cmd_watch_instance,
            commands::instances::cmd_unwatch_instance,
            commands::instances::cmd_open_instance_folder,
            commands::launcher::cmd_get_playtime,
            commands::launcher::cmd_format_playtime,
            commands::launcher::cmd_flush_playtime,
            commands::misc::cmd_clear_cache,
            commands::misc::cmd_open_folder,
            commands::modpacks::cmd_search_atlauncher,
            commands::modpacks::cmd_pause_modpack_install,
            commands::modpacks::cmd_resume_modpack_install,
            commands::modpacks::cmd_get_atlauncher_pack_versions,
            commands::modpacks::cmd_get_atlauncher_version_detail,
            commands::modpacks::cmd_install_atlauncher_modpack,
            commands::modpacks::cmd_search_curseforge_modpacks,
            commands::modpacks::cmd_get_curseforge_modpack_files,
            commands::modpacks::cmd_install_curseforge_modpack,
            commands::modpacks::cmd_search_modrinth_modpacks,
            commands::modpacks::cmd_get_modrinth_modpack_versions,
            commands::modpacks::cmd_install_modrinth_modpack,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { .. } = event {
                // Flush any active playtime session before the app exits.
                // We do NOT call api.prevent_close() вЂ” we let the window close,
                // but first we write the unpaid minutes to disk synchronously.
                let state: tauri::State<'_, AppState> = window.state();
                let now = Instant::now();
                if let Some((name, delta)) = playtime::take_session(&state.active_session, now) {
                    if delta > 0 {
                        if let Ok(cfg) = state.config.lock() {
                            playtime::add_minutes_and_save(&cfg.data_dir, &name, delta);
                        }
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("Error while running VoidLauncher");
}

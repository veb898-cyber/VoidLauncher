mod accounts;
mod atlauncher;
mod auth;
pub mod authlib_injector;
mod bug_report;
mod commands;
pub mod config;
mod content;
mod content_install;
mod crash_signals;
mod curseforge;
mod default_skin;
mod diagnostics;
mod download;
mod entry;
mod error;
mod events;
mod external_minecraft;
mod game_logs;
mod i18n;
mod import;
mod install_progress;
mod instance_readiness;
pub mod instances;
mod integrity;
mod java;
mod java_download;
pub mod jvm;
pub mod launch;
mod launch_state;
mod logger;
pub mod modloaders;
mod modrinth;
mod packlock;
mod playtime;
mod recovery;
mod rooms;
mod running_state;
mod server_ping;
mod servers;
mod single_instance;
mod verify;
pub mod versions;

#[cfg(test)]
mod smoke_launch;

#[cfg(test)]
mod diag;

use config::AppConfig;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::Manager;

/// Global app state shared across all Tauri commands
pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub auth_state: Mutex<auth::AuthState>,
    /// Per-instance launch phases (`Starting` while the launch is being
    /// prepared, `Running` once the game process exists) and the config-mutation
    /// lock. One synchronized model so Stop, a second Play and instance edits
    /// always observe a consistent phase. Multiple games may run at once.
    pub launches: launch_state::LaunchRegistry,
    pub pack_watcher: Mutex<Option<PackWatcherHandle>>,
    /// Active playtime-tracking sessions, keyed by instance name. One per
    /// running game, allowing concurrent sessions.
    pub active_sessions: Mutex<HashMap<String, playtime::ActiveSession>>,
    /// Room networking state (host/guest), shared with the VoidLink bridge.
    pub room_state: Arc<rooms::room_state::RoomStateManager>,
    /// Tailscale integration handle.
    pub tailscale: rooms::tailscale::TailscaleManager,
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
///
/// The URL must be parsed with the same WHATWG parser reqwest uses. A manual
/// split at `:`/`/` is not safe: `https://github.com:443@evil.example/x`
/// would look like `github.com` to a naive parser while reqwest connects to
/// `evil.example`. Userinfo is therefore rejected outright.
pub(crate) fn is_allowed_download_host(raw: &str) -> bool {
    // Test-only loopback origin for controlled mock HTTP tests. Release
    // builds still use the strict HTTPS allowlist. Parsing (not string
    // prefix matching) keeps userinfo URLs such as
    // `http://127.0.0.1:1@evil.example/` from passing this branch.
    #[cfg(test)]
    if let Ok(url) = reqwest::Url::parse(raw) {
        if url.scheme() == "http" && url.host_str() == Some("127.0.0.1") {
            return true;
        }
    }
    download::validate_download_url(raw).is_ok()
}

// ==================== App Builder ====================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The launcher owns one shared data directory. Acquire the process-wide
    // slot before the logger or config touches anything: a second process must
    // not race repairs, downloads or instance edits. The guard lives until
    // this function returns (i.e. for the whole Tauri event loop).
    let _single_instance =
        match single_instance::acquire_with_handoff(single_instance::APP_MUTEX_NAME) {
            Ok(guard) => guard,
            Err(single_instance::AcquireError::AlreadyRunning) => {
                single_instance::show_already_running();
                return;
            }
            Err(error) => {
                single_instance::show_startup_error(&error.to_string());
                return;
            }
        };

    // Panic hook: write to a known file so we can debug crashes
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!("{}", info);
        let _ = std::fs::write(std::env::temp_dir().join("voidlauncher_panic.txt"), &msg);
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

    // Crash recovery: hidden `.void-setup-*.tmp` folders are never valid
    // instances and are safe to remove. `.void-recovery-*` folders preserve
    // an interrupted import and are reported instead of deleted.
    let instances_dir = config.instances_dir();
    let removed = instance_readiness::cleanup_abandoned_setup_dirs(&instances_dir);
    if removed > 0 {
        tracing::warn!(
            target: "launcher",
            "Removed {} abandoned setup folder(s) from a previous crash",
            removed
        );
    }
    for dir in instance_readiness::recovery_dirs(&instances_dir) {
        tracing::warn!(
            target: "launcher",
            "Preserved data from an interrupted import: {} (kept until you remove it)",
            dir.display()
        );
    }

    download::set_global_proxy_policy(
        config.proxy_enabled,
        config.use_system_proxy(),
        config.proxy_url(),
        config.proxy_only,
    );
    atlauncher::set_atl_cache_dir(config.data_dir.clone());
    modloaders::prism_meta::set_cache_dir(config.data_dir.clone());

    tracing::info!(target: "launcher", "Data dir: {}", data_dir.display());
    tracing::info!(target: "launcher", "Config: data_dir={}, default_memory_mb={}, gc={}",
        config.data_dir.display(), config.default_memory_mb, config.default_gc_preset);

    // Multi-account migration: give the cached Microsoft session (if any) its
    // own per-account vault slot, then make sure the ACTIVE session matches
    // the default account: a different default Microsoft account loads its
    // own stored session; a default offline/Ely.by account hides Microsoft
    // from the UI (its tokens stay safe in the vault for re-activation).
    if let Some(ref profile) = auth_state.profile {
        let accounts_file_exists = accounts::accounts_file_exists(&data_dir);
        if !accounts_file_exists {
            // Legacy single-session install: there is no persistent account
            // list yet, so create it from the cached session.
            tracing::info!(target: "launcher", "Migrating legacy Microsoft session for {}", profile.name);
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
                                if let Err(e2) = accounts::store_ms_session(&entry.id, &auth_state)
                                {
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
        } else {
            // accounts.json is the source of truth. Only attach the cached
            // session to an account that is already listed — never create an
            // entry here, or deleting an account would be undone at startup.
            let existing = accounts::list_accounts(&data_dir).into_iter().find(|a| {
                a.account_type == accounts::AccountType::Microsoft
                    && a.uuid.as_deref() == Some(profile.id.as_str())
            });
            match existing {
                Some(entry) => {
                    if accounts::load_ms_session(&entry.id).is_none() {
                        match accounts::store_ms_session(&entry.id, &auth_state) {
                            Ok(()) => {}
                            Err(e1) => {
                                std::thread::sleep(std::time::Duration::from_millis(300));
                                if let Err(e2) = accounts::store_ms_session(&entry.id, &auth_state)
                                {
                                    tracing::error!(
                                        target: "launcher",
                                        "Could not attach Microsoft session of '{}' to its account entry: {} / {}",
                                        profile.name, e1, e2
                                    );
                                }
                            }
                        }
                    }
                }
                None if accounts::accounts_file_is_parseable(&data_dir) => {
                    tracing::warn!(
                        target: "launcher",
                        "Cached Microsoft session belongs to an account missing from accounts.json; clearing it instead of resurrecting the account"
                    );
                    auth_state = auth::AuthState::default();
                    auth::clear_auth_state(&config.auth_file());
                }
                None => {
                    tracing::warn!(
                        target: "launcher",
                        "accounts.json is present but unreadable; keeping the cached Microsoft session for this run"
                    );
                }
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
            Some(_) if auth_state.profile.is_some() => {
                tracing::info!(target: "launcher", "Default account is not a Microsoft one; hiding Microsoft session from UI");
                auth_state = auth::AuthState::default();
                // Also drop the global blob: leaving it behind made the
                // hidden Microsoft account look active again after a restart.
                auth::clear_auth_state(&config.auth_file());
            }
            _ => {}
        }
    }

    let app = tauri::Builder::default()
        .setup(|app| {
            events::set_app_handle(app.handle());
            // A room persisted while Rooms was enabled must not keep a hidden
            // bridge or a temporary Tailscale hostname after the feature was
            // turned off. Tear it down in the background (Tailscale CLI calls
            // can take seconds and must never delay startup).
            let state = app.state::<AppState>();
            let rooms_disabled = !state
                .config
                .lock()
                .map(|c| c.rooms_enabled)
                .unwrap_or(false);
            if rooms_disabled
                && (state.room_state.role() != rooms::room_state::RoomRole::None
                    || state.room_state.previous_hostname().is_some())
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle.state::<AppState>();
                    if let Err(e) = commands::rooms::teardown_room_session(&state).await {
                        tracing::warn!(target: "rooms", "Startup room teardown failed: {}", e);
                    }
                });
            } else if state.room_state.role() == rooms::room_state::RoomRole::Host
                && state.room_state.host_instance().is_some()
            {
                // Minecraft can outlive the launcher. Resume the host monitor
                // and VoidLink bridge only after the persisted pid record is
                // verified against the live process; otherwise clear the stale
                // state.
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle.state::<AppState>();
                    commands::rooms::resume_host_after_startup(handle.clone(), state).await;
                });
            }
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
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
            launches: launch_state::LaunchRegistry::new(),
            pack_watcher: Mutex::new(None),
            active_sessions: Mutex::new(HashMap::new()),
            room_state: Arc::new(rooms::room_state::RoomStateManager::new(&data_dir)),
            tailscale: rooms::tailscale::TailscaleManager::new(data_dir.clone()),
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
            commands::launcher::cmd_prepare_elyby_skins,
            commands::launcher::cmd_launch_game,
            commands::launcher::cmd_get_fabric_versions,
            commands::launcher::cmd_get_forge_versions,
            commands::launcher::cmd_get_neoforge_versions,
            commands::launcher::cmd_install_fabric,
            commands::launcher::cmd_install_forge,
            commands::launcher::cmd_install_neoforge,
            commands::launcher::cmd_install_instance_loader,
            commands::launcher::cmd_update_instance_loader,
            commands::misc::cmd_get_config,
            commands::misc::cmd_save_config,
            commands::misc::cmd_read_image_file,
            commands::misc::cmd_fetch_page_asset,
            commands::misc::cmd_check_latest_version,
            commands::misc::cmd_apply_proxy_policy,
            commands::misc::cmd_updater_proxy_policy,
            commands::misc::cmd_updater_check,
            commands::misc::cmd_get_launch_state,
            commands::instances::cmd_check_instance_installed,
            commands::misc::cmd_detect_system_ram,
            commands::misc::cmd_get_installer_language,
            commands::heads::cmd_get_player_head,
            commands::heads::cmd_invalidate_player_head,
            commands::heads::cmd_get_player_capes,
            commands::heads::cmd_set_player_cape,
            commands::mods::cmd_search_modrinth,
            commands::mods::cmd_search_curseforge,
            commands::mods::cmd_get_modrinth_versions,
            commands::mods::cmd_get_curseforge_files,
            commands::mods::cmd_install_local_content,
            commands::mods::cmd_get_modrinth_project,
            commands::mods::cmd_get_modrinth_version_by_id,
            commands::mods::cmd_popular_modrinth,
            commands::mods::cmd_popular_curseforge,
            commands::mods::cmd_get_curseforge_mod_detail,
            commands::mods::cmd_get_modrinth_project_body,
            commands::mods::cmd_check_mod_updates,
            commands::content::cmd_resolve_dependencies,
            commands::content::cmd_check_content_compatibilities,
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
            commands::mods::cmd_resolve_mod_project_names,
            commands::mods::cmd_get_mod_icon,
            commands::mods::cmd_fetch_icon_url,
            commands::launcher::cmd_emit_log,
            commands::launcher::cmd_list_game_logs,
            commands::launcher::cmd_read_game_log,
            commands::launcher::cmd_read_game_log_incremental,
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
            commands::export::cmd_save_export_file,
            commands::instances::cmd_probe_modpack,
            commands::instances::cmd_import_modpack,
            commands::instances::cmd_finish_instance_setup,
            commands::instances::cmd_set_instance_icon,
            commands::instances::cmd_set_instance_banner,
            commands::instances::cmd_log_toast,
            commands::instances::cmd_list_saves,
            commands::instances::cmd_rename_world,
            commands::instances::cmd_copy_world,
            commands::instances::cmd_delete_world,
            commands::instances::cmd_backup_world,
            commands::instances::cmd_list_world_backups,
            commands::instances::cmd_restore_world,
            commands::instances::cmd_delete_world_backup,
            commands::instances::cmd_backup_instance,
            commands::instances::cmd_list_instance_backups,
            commands::instances::cmd_restore_instance,
            commands::instances::cmd_delete_instance_backup,
            commands::instances::cmd_list_screenshots,
            commands::instances::cmd_delete_screenshot,
            commands::instances::cmd_read_screenshot,
            commands::instances::cmd_list_packs,
            commands::instances::cmd_set_resource_pack_enabled,
            commands::instances::cmd_set_resource_packs_order,
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
            commands::modpacks::cmd_retry_curseforge_import_files,
            commands::packlock::cmd_get_pack_lock,
            commands::packlock::cmd_check_modpack_update,
            commands::packlock::cmd_update_modpack,
            commands::packlock::cmd_repair_modpack,
            commands::modpacks::cmd_search_modrinth_modpacks,
            commands::modpacks::cmd_get_modrinth_modpack_versions,
            commands::modpacks::cmd_install_modrinth_modpack,
            commands::rooms::cmd_room_status,
            commands::rooms::cmd_room_check_link,
            commands::rooms::cmd_room_install_tailscale,
            commands::rooms::cmd_room_login,
            commands::rooms::cmd_room_session_active,
            commands::rooms::cmd_room_invitation,
            commands::rooms::cmd_room_set_host_instance,
            commands::rooms::cmd_room_create_host,
            commands::rooms::cmd_room_join_guest,
            commands::rooms::cmd_room_leave,
            commands::rooms::cmd_room_refresh,
            commands::rooms::cmd_room_report_minecraft_port,
            commands::rooms::cmd_room_detect_minecraft_port,
            commands::rooms::cmd_room_join_args,
            commands::rooms::cmd_room_quick_play_join_status,
            commands::rooms::cmd_room_open_admin_console,
            commands::verify::cmd_verify_instance,
            commands::verify::cmd_repair_instance,
            commands::diagnostics::cmd_run_diagnostics,
            commands::diagnostics::cmd_analyze_crash,
            commands::diagnostics::cmd_apply_crash_action,
            commands::diagnostics::cmd_open_official_url,
            bug_report::cmd_prepare_bug_report,
            bug_report::cmd_get_bug_report_log,
            bug_report::cmd_get_bug_report_crash,
            commands::servers::cmd_list_servers,
            commands::servers::cmd_add_server,
            commands::servers::cmd_update_server,
            commands::servers::cmd_delete_server,
            commands::servers::cmd_ping_server,
            commands::launcher::cmd_stop_game,
        ])
        .build(tauri::generate_context!())
        .expect("Error while building VoidLauncher");

    // Playtime must be finalized only when the application really exits.
    // `CloseRequested` is not final: the JS close guard may prevent the close
    // (unsaved changes) or the user may cancel the confirmation. Flushing
    // there would drain the live sessions and stop counting the running game.
    app.run(|app_handle, event| {
        if should_flush_playtime_on_run_event(&event) {
            flush_playtime_on_exit(app_handle);
        }
    });
}

/// The final application-exit event is the only point where active playtime
/// sessions may be drained.
fn should_flush_playtime_on_run_event(event: &tauri::RunEvent) -> bool {
    matches!(event, tauri::RunEvent::Exit)
}

/// Commit every still-active playtime session exactly once at app exit.
fn flush_playtime_on_exit(app_handle: &tauri::AppHandle) {
    let state: tauri::State<'_, AppState> = app_handle.state();
    let data_dir = match state.config.lock() {
        Ok(config) => config.data_dir.clone(),
        Err(_) => return,
    };
    let flushed = playtime::flush_all_sessions(&state.active_sessions, &data_dir, Instant::now());
    if flushed > 0 {
        tracing::info!(
            target: "launcher",
            "Flushed {} playtime session(s) on exit",
            flushed
        );
    }
}

#[cfg(test)]
mod window_lifecycle_tests {
    use super::should_flush_playtime_on_run_event;

    #[test]
    fn playtime_is_flushed_only_on_application_exit() {
        assert!(should_flush_playtime_on_run_event(&tauri::RunEvent::Exit));
        assert!(!should_flush_playtime_on_run_event(&tauri::RunEvent::Ready));
        assert!(!should_flush_playtime_on_run_event(
            &tauri::RunEvent::MainEventsCleared
        ));
    }
}

#[cfg(test)]
mod download_host_tests {
    use super::is_allowed_download_host;

    #[test]
    fn accepts_allowlisted_https_hosts() {
        assert!(is_allowed_download_host(
            "https://cdn.modrinth.com/data/project/version/file.jar"
        ));
        assert!(is_allowed_download_host(
            "https://raw.githubusercontent.com/owner/repo/main/file.jar"
        ));
        assert!(is_allowed_download_host(
            "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
        ));
    }

    #[test]
    fn rejects_userinfo_host_confusion() {
        // `github.com:443` is userinfo; reqwest would connect to evil.example.
        assert!(!is_allowed_download_host(
            "https://github.com:443@evil.example/payload"
        ));
        assert!(!is_allowed_download_host(
            "https://cdn.modrinth.com@evil.example/payload"
        ));
    }

    #[test]
    fn rejects_backslash_and_suffix_lookalikes() {
        assert!(!is_allowed_download_host(
            "https://evil.example\\.github.com/payload"
        ));
        assert!(!is_allowed_download_host(
            "https://cdn.modrinth.com.evil.example/payload"
        ));
        assert!(!is_allowed_download_host(
            "https://evilcdn.modrinth.com/payload"
        ));
    }

    #[test]
    fn rejects_non_https_and_malformed_urls() {
        assert!(!is_allowed_download_host(
            "http://cdn.modrinth.com/file.jar"
        ));
        assert!(!is_allowed_download_host("file:///C:/Windows/System32"));
        assert!(!is_allowed_download_host("not a url"));
    }
}

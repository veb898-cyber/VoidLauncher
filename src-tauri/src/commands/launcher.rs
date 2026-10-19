// ==================== Launcher Commands ====================
// Version installation, game launch, mod loader installation,
// playtime tracking, and game-log commands.

use crate::commands::instances::validate_instance_name;
use crate::download;
use crate::events::{self, InstallProgressPayload, ProgressSender};
use crate::game_logs;
use crate::instances;
use crate::is_allowed_download_host;
use crate::java_download;
use crate::launch;
use crate::modloaders;
use crate::playtime;
use crate::versions;
use crate::AppState;
use crate::{accounts, i18n};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

/// All `cmd_get_*_versions` commands return a `LoaderVersionPage`
/// (a slice of the full sorted version list plus `total`). The wizard
/// uses infinite scroll with `PAGE_SIZE` items per request.
const PAGE_SIZE: usize = 20;

#[tauri::command]
pub async fn cmd_install_version(
    app: AppHandle,
    state: State<'_, AppState>,
    version_url: String,
    instance_id: String,
) -> Result<String, String> {
    let config = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.clone()
    };

    // Create progress channel and bridge
    let (progress_tx, rx) = ProgressSender::new();
    events::spawn_event_bridge(app.clone(), rx, instance_id.clone());

    let send_progress = |percent: f64, stage: &str, message: &str| {
        progress_tx.send(InstallProgressPayload {
            instance_id: instance_id.clone(),
            percent,
            downloaded_bytes: 0,
            total_bytes: 0,
            stage: stage.to_string(),
            message: message.to_string(),
        });
    };

    events::emit_log(&app, "info", "install", "Fetching version info...");
    send_progress(0.0, "manifest", "Fetching version info...");

    // Verify the URL is from an allowed host (SSRF protection)
    if !is_allowed_download_host(&version_url) {
        return Err("Access denied: download host not allowed".to_string());
    }

    // Fetch version info
    let version_info = versions::fetch_version_info(&version_url)
        .await
        .map_err(|e| e.to_string())?;

    events::emit_log(
        &app,
        "info",
        "install",
        &format!("Version {} fetched", version_info.id),
    );

    // The id and asset-index name are remote metadata used as directory and
    // file names — they must be plain components, never a path.
    crate::import::check_safe_file_name(&version_info.id)
        .map_err(|_| "Invalid version id in remote metadata".to_string())?;
    crate::import::check_safe_file_name(&version_info.assets)
        .map_err(|_| "Invalid asset index name in remote metadata".to_string())?;

    send_progress(5.0, "manifest", "Collecting files to download...");

    // Collect files to download
    let files = versions::collect_downloads(
        &version_info,
        &config.libraries_dir(),
        &config.versions_dir(),
    );

    let progress_tx_clone = progress_tx.clone();
    let instance_id_clone = instance_id.clone();

    // Download all files with real progress
    events::emit_log(
        &app,
        "info",
        "install",
        &format!("Downloading {} libraries...", files.len()),
    );
    send_progress(10.0, "libraries", "Downloading libraries...");
    download::download_files(
        files,
        move |completed, total, bytes_done, bytes_total, _msg| {
            let frac = if bytes_total > 0 {
                bytes_done as f64 / bytes_total as f64
            } else if total > 0 {
                completed as f64 / total as f64
            } else {
                1.0
            };
            let pct = 10.0 + frac * 60.0;
            progress_tx_clone.send(InstallProgressPayload {
                instance_id: instance_id_clone.clone(),
                percent: pct,
                downloaded_bytes: bytes_done,
                total_bytes: bytes_total,
                stage: "libraries".to_string(),
                message: format!("Downloading libraries ({}/{})", completed, total),
            });
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    send_progress(70.0, "libraries", "Saving version metadata...");

    // Save version JSON
    let version_dir = config.versions_dir().join(&version_info.id);
    std::fs::create_dir_all(&version_dir).map_err(|e| e.to_string())?;
    let version_json_path = version_dir.join(format!("{}.json", version_info.id));
    let json = serde_json::to_string_pretty(&version_info).map_err(|e| e.to_string())?;
    std::fs::write(version_json_path, json).map_err(|e| e.to_string())?;

    send_progress(75.0, "assets", "Downloading asset index...");

    // Download and save asset index
    if !is_allowed_download_host(&version_info.asset_index.url) {
        return Err("Access denied: asset index host not allowed".to_string());
    }
    let asset_index = versions::fetch_asset_index(&version_info.asset_index.url)
        .await
        .map_err(|e| e.to_string())?;

    let indexes_dir = config.assets_dir().join("indexes");
    std::fs::create_dir_all(&indexes_dir).map_err(|e| e.to_string())?;
    let index_path = indexes_dir.join(format!("{}.json", version_info.assets));
    let index_json = serde_json::to_string_pretty(&asset_index).map_err(|e| e.to_string())?;
    std::fs::write(index_path, index_json).map_err(|e| e.to_string())?;

    events::emit_log(
        &app,
        "info",
        "install",
        &format!("Downloading assets for {}...", version_info.assets),
    );
    send_progress(78.0, "assets", "Downloading assets...");

    // Download assets with progress
    let progress_tx_assets = progress_tx.clone();
    let instance_id_assets = instance_id.clone();
    download::download_assets(
        &asset_index,
        &config.assets_dir(),
        move |completed, total, bytes_done, bytes_total, _msg| {
            let pct = 78.0 + (completed as f64 / total as f64) * 20.0;
            progress_tx_assets.send(InstallProgressPayload {
                instance_id: instance_id_assets.clone(),
                percent: pct,
                downloaded_bytes: bytes_done,
                total_bytes: bytes_total,
                stage: "assets".to_string(),
                message: format!("Downloading assets ({}/{})", completed, total),
            });
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    // Mirror objects into virtual/legacy for old versions (1.6.x and older
    // read flat files from --assetsDir; 1.7.x/1.8.x use it as legacy layout).
    download::ensure_virtual_assets(&version_info.assets, &asset_index, &config.assets_dir())
        .map_err(|e| e.to_string())?;

    events::emit_log(&app, "info", "install", "Installation complete!");
    send_progress(100.0, "done", "Installation complete!");

    Ok(version_info.id)
}

/// Ensure suitable Java is available. If missing, download it automatically.
async fn ensure_java_for_launch(
    app: &AppHandle,
    data_dir: &Path,
    version_info: &versions::VersionInfo,
    instance: &instances::Instance,
    config: &crate::config::AppConfig,
) -> Result<(), String> {
    let required_major = version_info.required_java_major();

    // An explicitly configured Java (instance or launcher settings) takes
    // priority in `launch::get_java_path`. If it is present, this function
    // has nothing to download: a matching runtime needs no help, and a
    // mismatching/broken one is reported by `get_java_path` at spawn time
    // with a precise error (downloading a managed runtime would be ignored
    // anyway). Previously the launcher downloaded a whole managed JRE even
    // though the user had already pointed it at a valid one.
    if let Some(path) = instance
        .java_path
        .as_ref()
        .filter(|p| p.exists())
        .or_else(|| config.java_path.as_ref().filter(|p| p.exists()))
    {
        if let Some(inst) = crate::java::probe_java_by_path(path) {
            if inst.major_version == required_major {
                tracing::debug!(
                    target: "launcher",
                    "Configured Java {} already matches required Java {}",
                    inst.version, required_major
                );
            }
        }
        return Ok(());
    }

    // Only use the exact required version AND a 64-bit build. Newer JDKs
    // (21+/23+) scan classpath JARs for automatic module names during boot
    // layer initialisation, but the obfuscated client.jar contains classes in
    // the unnamed package, which causes java.lang.module.FindException. A
    // 32-bit install cannot run modern clients (and is filtered out of the
    // auto-detection too), so it must not satisfy this check.
    let installations = launch::find_all_java_installations(data_dir);
    if installations
        .iter()
        .any(|j| j.is_64bit && j.major_version == required_major)
    {
        return Ok(());
    }

    events::emit_log(
        app,
        "info",
        "launch",
        &format!(
            "Java {} required but not found locally. Downloading...",
            required_major
        ),
    );

    java_download::download_java_runtime(required_major, data_dir, app)
        .await
        .map_err(|e| {
            let msg = format!("Failed to download Java {}: {}", required_major, e);
            events::emit_log(app, "error", "launch", &msg);
            msg
        })?;

    events::emit_log(
        app,
        "info",
        "launch",
        &format!("Java {} downloaded successfully", required_major),
    );
    Ok(())
}

/// RAII slot in `running_instances` that reserves an instance name for the
/// whole launch-preparation phase. If the command returns early (error
/// before the game starts) the slot is released automatically; on success
/// `disarm()` keeps the entry until the exit handler removes it.
struct LaunchReservation<'a> {
    state: &'a AppState,
    instance_name: String,
    armed: bool,
}

impl<'a> LaunchReservation<'a> {
    fn acquire(state: &'a AppState, instance_name: &str) -> Result<Self, String> {
        let mut running = state.running_instances.lock().map_err(|e| e.to_string())?;
        if running.iter().any(|n| n == instance_name) {
            return Err(format!(
                "Instance '{}' is already running. Launch it again after it exits.",
                instance_name
            ));
        }
        running.push(instance_name.to_string());
        Ok(Self {
            state,
            instance_name: instance_name.to_string(),
            armed: true,
        })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for LaunchReservation<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut running) = self.state.running_instances.lock() {
            running.retain(|n| n != &self.instance_name);
        }
    }
}

#[tauri::command]
pub async fn cmd_launch_game(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
    server_address: Option<String>,
    server_port: Option<u16>,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;

    // Guard against launching the same instance twice concurrently — a
    // second click on a running instance must be a no-op, not a second copy.
    // The slot is reserved NOW (before the long download/prepare phase) and
    // released automatically if this launch fails before the game starts;
    // otherwise it stays until the process exits (see the exit handler).
    let (config, data_dir) = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        (c.clone(), c.data_dir.clone())
    };

    // Reconcile persisted pid files with the live process table BEFORE
    // reserving the name. A game orphaned by a launcher restart or an
    // auto-update relaunch must both reappear in the running list (badge,
    // Stop button) and block a second launch of the same instance.
    for game in crate::running_state::scan_live_games(&data_dir) {
        if let Ok(mut running) = state.running_instances.lock() {
            if !running.iter().any(|n| n == &game.instance) {
                running.push(game.instance.clone());
            }
        }
    }

    // Guard against launching the same instance twice concurrently — a
    // second click on a running instance must be a no-op, not a second copy.
    // The slot is reserved NOW (before the long download/prepare phase) and
    // released automatically if this launch fails before the game starts;
    // otherwise it stays until the process exits (see the exit handler).
    let mut reservation = match LaunchReservation::acquire(state.inner(), &instance_name) {
        Ok(r) => r,
        Err(msg) => {
            events::emit_log(&app, "warn", "launch", &msg);
            return Err(msg);
        }
    };

    // Create game log file early so all launch messages are captured in it.
    // The path is threaded through this launch explicitly (quick play log,
    // output readers) so concurrent games never write into each other's logs.
    let session_log = game_logs::create_game_log_file(&data_dir, &instance_name).ok();

    events::emit_log(
        &app,
        "info",
        "launch",
        &format!("Preparing to launch: {}", instance_name),
    );

    // Launch strictly as the active account (the one marked "active" on the
    // Accounts page). Each account type resolves its own credentials;
    // Microsoft accounts use their per-account stored session.
    let accounts_list = accounts::list_accounts(&data_dir);
    let default_account = accounts_list
        .iter()
        .find(|a| a.default)
        .or_else(|| accounts_list.first())
        .cloned();

    let Some(account) = default_account else {
        let msg =
            "No account available to launch the game. Please add an account first.".to_string();
        events::emit_log(&app, "error", "launch", &msg);
        return Err(msg);
    };

    let (access_token, uuid, username) = match account.account_type {
        accounts::AccountType::Offline => {
            events::emit_log(
                &app,
                "info",
                "launch",
                &format!("Launching offline as '{}'", account.name),
            );
            let uuid_val = account
                .uuid
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            (String::new(), uuid_val, account.name.clone())
        }
        accounts::AccountType::ElyBy => {
            events::emit_log(
                &app,
                "info",
                "launch",
                &format!("Launching via Ely.by as '{}'", account.name),
            );
            // Online-mode servers validate the join token against the session
            // server (authserver.ely.by for Ely.by-aware servers). An empty
            // token makes multiplayer fail with "Invalid session"; the token
            // stored at login is required.
            let token = accounts::get_elyby_token(&account.id)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| {
                    events::emit_log(
                        &app,
                        "warn",
                        "launch",
                        "No stored Ely.by token — multiplayer session may be rejected.",
                    );
                    String::new()
                });
            (
                token,
                account.uuid.clone().unwrap_or_default(),
                account.name.clone(),
            )
        }
        accounts::AccountType::Microsoft => {
            match super::auth::ensure_ms_session(state.inner(), &account).await {
                Ok(session) => {
                    if let (Some(mc_token), Some(profile)) =
                        (&session.minecraft_token, &session.profile)
                    {
                        events::emit_log(
                            &app,
                            "info",
                            "launch",
                            &format!("Online mode: launching as '{}' (Microsoft)", profile.name),
                        );
                        (
                            mc_token.access_token.clone(),
                            profile.id.clone(),
                            profile.name.clone(),
                        )
                    } else {
                        let msg = format!(
                            "Stored Microsoft session for '{}' is incomplete. Please sign in again.",
                            account.name
                        );
                        events::emit_log(&app, "error", "launch", &msg);
                        return Err(msg);
                    }
                }
                Err(e) => {
                    events::emit_log(&app, "error", "launch", &e);
                    return Err(e);
                }
            }
        }
    };

    let mut instance = match instances::get_instance(&config.instances_dir(), &instance_name) {
        Ok(i) => i,
        Err(e) => {
            let msg = format!("Failed to load instance '{}': {}", instance_name, e);
            events::emit_log(&app, "error", "launch", &msg);
            return Err(e.to_string());
        }
    };

    // Auto-install the loader if it's set but not installed yet
    if instance.loader != instances::LoaderType::Vanilla && instance.loader_profile.is_none() {
        let loader_str = match instance.loader {
            instances::LoaderType::Fabric => "Fabric",
            instances::LoaderType::Forge => "Forge",
            instances::LoaderType::NeoForge => "NeoForge",
            _ => "Vanilla",
        };
        let loader_version = match instance.loader_version.clone() {
            Some(v) if !v.is_empty() => v,
            _ => {
                let msg = format!(
                    "{} loader is set but has no version. Please reinstall it.",
                    loader_str
                );
                events::emit_log(&app, "error", "launch", &msg);
                return Err(msg);
            }
        };
        events::emit_log(
            &app,
            "info",
            "launch",
            &format!(
                "{} loader is not installed, installing automatically ({} for MC {})...",
                loader_str, loader_version, instance.mc_version
            ),
        );
        let profile = match modloaders::install_loader(
            loader_str,
            &instance.mc_version,
            &loader_version,
            &config.libraries_dir(),
            &config.versions_dir(),
            Some(&app),
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("Failed to auto-install {} loader: {}", loader_str, e);
                events::emit_log(&app, "error", "launch", &msg);
                return Err(msg);
            }
        };
        instance.loader_profile = Some(profile);
        if let Err(e) = instances::save_instance(&config.instances_dir(), &instance, None) {
            let msg = format!("Failed to save installed {} loader: {}", loader_str, e);
            events::emit_log(&app, "error", "launch", &msg);
            return Err(msg);
        }
        events::emit_log(
            &app,
            "info",
            "launch",
            &format!(
                "{} loader installed automatically (version {})",
                loader_str, loader_version
            ),
        );
    }

    events::emit_log(&app, "info", "launch", "Fetching version manifest...");
    let manifest = match versions::fetch_version_manifest().await {
        Ok(m) => m,
        Err(e) => {
            let msg = format!("Failed to fetch version manifest: {}", e);
            events::emit_log(&app, "error", "launch", &msg);
            return Err(msg);
        }
    };

    let version_url = manifest
        .versions
        .iter()
        .find(|v| v.id == instance.mc_version)
        .ok_or_else(|| {
            let msg = format!("Version {} not found in manifest", instance.mc_version);
            events::emit_log(&app, "error", "launch", &msg);
            msg
        })?
        .url
        .clone();

    events::emit_log(
        &app,
        "info",
        "launch",
        &format!("Fetching version info for {}...", instance.mc_version),
    );
    let version_info = match versions::fetch_version_info(&version_url).await {
        Ok(v) => v,
        Err(e) => {
            let msg = format!(
                "Failed to fetch version info for {}: {}",
                instance.mc_version, e
            );
            events::emit_log(&app, "error", "launch", &msg);
            return Err(msg);
        }
    };

    // Self-heal: ensure the version's files (client.jar, libraries) are on
    // disk. They are normally downloaded when the instance is created, but a
    // failed creation or a manually removed file must not break the launch.
    events::emit_log(
        &app,
        "info",
        "launch",
        "Ensuring version files are present...",
    );
    let files = versions::collect_downloads(
        &version_info,
        &config.libraries_dir(),
        &config.versions_dir(),
    );
    let missing = files
        .into_iter()
        .filter(|(_, path, _, _)| !path.exists())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        events::emit_log(
            &app,
            "info",
            "launch",
            &format!("Downloading {} missing version file(s)...", missing.len()),
        );
        download::download_files(missing, |completed, total, bytes_done, bytes_total, _msg| {
            tracing::info!(target: "launcher", "Version file download {}/{} ({} / {} bytes)", completed, total, bytes_done, bytes_total);
        })
        .await
        .map_err(|e| {
            let msg = format!("Failed to download version files: {}", e);
            events::emit_log(&app, "error", "launch", &msg);
            msg
        })?;
    }

    // Self-heal: ensure the asset index and assets exist (e.g. when the
    // instance creation was interrupted before the asset step).
    let index_path = config
        .assets_dir()
        .join("indexes")
        .join(format!("{}.json", version_info.assets));
    if !index_path.exists() {
        events::emit_log(
            &app,
            "info",
            "launch",
            &format!("Downloading asset index {}...", version_info.assets),
        );
        if !crate::is_allowed_download_host(&version_info.asset_index.url) {
            let msg = format!(
                "Access denied: asset index host not allowed ({})",
                version_info.asset_index.url
            );
            events::emit_log(&app, "error", "launch", &msg);
            return Err(msg);
        }
        let asset_index = versions::fetch_asset_index(&version_info.asset_index.url)
            .await
            .map_err(|e| {
                let msg = format!("Failed to fetch asset index: {}", e);
                events::emit_log(&app, "error", "launch", &msg);
                msg
            })?;
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let index_json = serde_json::to_string_pretty(&asset_index).map_err(|e| e.to_string())?;
        std::fs::write(&index_path, index_json).map_err(|e| e.to_string())?;
        events::emit_log(
            &app,
            "info",
            "launch",
            &format!("Downloading assets for {}...", version_info.assets),
        );
        download::download_assets(&asset_index, &config.assets_dir(), |completed, total, bytes_done, bytes_total, _msg| {
            tracing::info!(target: "launcher", "Asset download {}/{} ({} / {} bytes)", completed, total, bytes_done, bytes_total);
        })
        .await
        .map_err(|e| {
            let msg = format!("Failed to download assets: {}", e);
            events::emit_log(&app, "error", "launch", &msg);
            msg
        })?;
    }

    // Ensure suitable Java is available (download if missing)
    ensure_java_for_launch(&app, &data_dir, &version_info, &instance, &config).await?;

    // Ensure loader processor-generated JARs exist (client-*-srg.jar,
    // forge-*-client.jar, NeoForge's patched client, etc.). Do NOT refresh
    // the stored launch profile here: it is persisted in instance.json, and
    // re-fetching it on every launch re-downloaded the 9–14 MB installer.
    match instance.loader {
        instances::LoaderType::Forge => {
            if let Some(ref loader_ver) = instance.loader_version {
                if let Err(e) = modloaders::forge::ensure_processor_jars(
                    &instance.mc_version,
                    loader_ver,
                    &config.libraries_dir(),
                    &config.versions_dir(),
                )
                .await
                {
                    tracing::warn!(target: "launcher", "Processor JAR check failed (continuing anyway): {}", e);
                }
            }
        }
        instances::LoaderType::NeoForge => {
            if let Some(ref loader_ver) = instance.loader_version {
                if let Err(e) = modloaders::neoforge::ensure_processor_jars(
                    &instance.mc_version,
                    loader_ver,
                    &config.libraries_dir(),
                    &config.versions_dir(),
                )
                .await
                {
                    tracing::warn!(target: "launcher", "NeoForge processor JAR check failed (continuing anyway): {}", e);
                }
            }
        }
        _ => {}
    }

    // Self-heal loader libraries: if one was deleted or quarantined after
    // installation, download it now instead of silently dropping it from the
    // classpath (which surfaces later as NoClassDefFoundError).
    if let Some(profile) = &instance.loader_profile {
        for lib in &profile.libraries {
            if lib.url.is_empty() || crate::import::check_safe_remote_path(&lib.path).is_err() {
                continue;
            }
            let lib_path = config.libraries_dir().join(&lib.path);
            let is_jar = lib.path.to_ascii_lowercase().ends_with(".jar");
            let needs_download =
                !lib_path.is_file() || (is_jar && !modloaders::forge::is_usable_jar(&lib_path));
            if !needs_download {
                continue;
            }
            events::emit_log(
                &app,
                "info",
                "launch",
                &format!("Repairing loader library {}...", lib.name),
            );
            if let Err(e) =
                download::download_file(&lib.url, &lib_path, lib.sha1.as_deref().unwrap_or(""))
                    .await
            {
                let msg = format!("Failed to repair loader library {}: {}", lib.name, e);
                events::emit_log(&app, "error", "launch", &msg);
                return Err(msg);
            }
        }
    }

    // Ensure LWJGL native JARs are present (self-heal for installs that
    // predate native-classifier downloads)
    if let Err(e) = versions::ensure_native_libraries(&version_info, &config.libraries_dir()).await
    {
        tracing::warn!(target: "launcher", "Native library check failed (continuing anyway): {}", e);
    }

    // Ensure virtual/legacy assets exist for old versions (1.6.x and
    // older need them as --assetsDir; 1.7.x/1.8.x use them as fallback).
    let index_path = config
        .assets_dir()
        .join("indexes")
        .join(format!("{}.json", version_info.assets));
    if index_path.exists() {
        if let Ok(asset_index) = versions::load_asset_index(&index_path) {
            if let Err(e) = download::ensure_virtual_assets(
                &version_info.assets,
                &asset_index,
                &config.assets_dir(),
            ) {
                tracing::warn!(target: "launcher", "Virtual assets check failed (continuing anyway): {}", e);
            }
        }
    }

    events::emit_log(
        &app,
        "info",
        "launch",
        "Building classpath and launching Java...",
    );
    // Launch preparation (Java probing, classpath assembly, argument
    // composition) is blocking I/O and subprocess work; run it on the
    // blocking pool so it cannot stall the async runtime / UI.
    let child = {
        let launch_config = config.clone();
        let launch_instance = instance.clone();
        let launch_version_info = version_info.clone();
        let launch_token = access_token.clone();
        let launch_uuid = uuid.clone();
        let launch_username = username.clone();
        let launch_server = server_address.clone();
        let launch_session_log = session_log.clone();
        let result = tokio::task::spawn_blocking(move || {
            launch::launch_minecraft(
                &launch_config,
                &launch_instance,
                &launch_version_info,
                &launch::LaunchAuth {
                    access_token: &launch_token,
                    uuid: &launch_uuid,
                    username: &launch_username,
                },
                launch_server.as_deref().map(|h| (h, server_port)),
                launch_session_log.as_deref(),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("Launch task failed: {}", e))?;
        result.map_err(|e| {
            events::emit_log(&app, "error", "launch", &format!("Launch failed: {}", e));
            e
        })?
    };

    let pid = child.id();
    let _ = instances::update_last_played(&config.instances_dir(), &instance_name);
    // Persist the pid BEFORE anything else can fail: if the launcher is
    // closed or updated while the game runs, the next start recovers the
    // still-running game from this file (badge + Stop + relaunch guard).
    crate::running_state::write_pid_file(&data_dir, &instance_name, pid);
    let child_handle: Arc<std::sync::Mutex<Option<std::process::Child>>> =
        Arc::new(std::sync::Mutex::new(Some(child)));

    // Start the playtime session BEFORE releasing the reservation and before
    // the "close on launch" window close: the CloseRequested handler flushes
    // active sessions, and the exit watcher finalizes this one by pid. If the
    // sessions lock is poisoned we stop the freshly started game instead of
    // leaving it marked "running" without tracking — which would block every
    // future relaunch until an app restart.
    {
        let now = Instant::now();
        let mut sessions = match state.active_sessions.lock() {
            Ok(g) => g,
            Err(e) => {
                if let Ok(mut guard) = child_handle.lock() {
                    if let Some(mut c) = guard.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                }
                crate::running_state::remove_pid_file(&data_dir, &instance_name);
                if let Ok(mut running) = state.running_instances.lock() {
                    running.retain(|n| n != &instance_name);
                }
                if let Some(p) = session_log.as_deref() {
                    game_logs::clear_current_log_path_if(p);
                }
                return Err(format!("Failed to start playtime session: {}", e));
            }
        };
        sessions.insert(
            instance_name.clone(),
            playtime::ActiveSession {
                instance_name: instance_name.clone(),
                pid,
                started_at: now,
                last_flush: now,
                child: child_handle.clone(),
            },
        );
    }
    events::emit_log(
        &app,
        "info",
        "launch",
        &format!("Playtime session started for: {}", instance_name),
    );

    // Notify the frontend that the game process is actually running. The
    // launcher window is minimized from the `game_started` handler, not on
    // the Play click — a failed launch or a crash leaves the window visible.
    let _ = app.emit(
        "game_started",
        events::LaunchEventPayload {
            instance_id: instance_name.clone(),
            status: "running".into(),
            pid: Some(pid),
            exit_code: None,
        },
    );

    // "Close on launch": the window close triggers the playtime flush
    // (WindowEvent::CloseRequested), then the app exits, leaving only the game.
    if config.close_on_launch {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.close();
        }
    }

    // Mark this instance as running (the frontend shows a "Running" badge).
    // Multiple games may run at once, so the reservation slot from the start
    // of this command becomes permanent until the process exits.
    reservation.disarm();

    // Background timer: while the game runs, flush whole minutes to disk every
    // minute. The exit watcher owns the final teardown; the pid-guarded
    // helpers keep a stale timer (map keys are instance names, reused across
    // relaunches) from ever touching a newer run's session.
    let app_for_timer = app.clone();
    let data_dir_for_timer = data_dir.clone();
    let instance_for_timer = instance_name.clone();
    let child_for_timer = child_handle.clone();
    let pid_for_timer = pid;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            // If the game process has exited, stop the timer — the exit
            // watcher commits the final tail within ~500 ms.
            let exited = {
                if let Ok(mut guard) = child_for_timer.lock() {
                    guard
                        .as_mut()
                        .map(|c| c.try_wait().ok().flatten().is_some())
                        .unwrap_or(true)
                } else {
                    true
                }
            };
            if exited {
                break;
            }
            // Commit the actual unpaid minutes since the last flush. The timer
            // tick is just a cadence hint — a slow tick (GC pause, system
            // suspend) credits 2+ minutes, a fast tick credits 0.
            let now = Instant::now();
            let app_state = app_for_timer.state::<AppState>();
            let delta = playtime::flush_keyed_session_for_pid(
                &app_state.active_sessions,
                &instance_for_timer,
                pid_for_timer,
                now,
            );
            if delta > 0 {
                playtime::add_minutes_and_save(&data_dir_for_timer, &instance_for_timer, delta);
            }
        }
    });

    // Background task: wait for the process to exit and emit launch_complete
    let app_clone = app.clone();
    let instance_clone = instance_name.clone();
    let child_for_wait = child_handle.clone();
    let pid_for_exit = pid;
    let data_dir_for_exit = data_dir.clone();
    let session_log_for_exit = session_log.clone();
    tokio::spawn(async move {
        // Poll try_wait in a loop; multiple try_wait callers are safe.
        let exit_code: i32 = loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let status_opt = {
                if let Ok(mut guard) = child_for_wait.lock() {
                    if let Some(child) = guard.as_mut() {
                        child.try_wait().ok().flatten()
                    } else {
                        // Child was removed (shouldn't happen); bail out
                        break -1;
                    }
                } else {
                    break -1;
                }
            };
            if let Some(s) = status_opt {
                break s.code().unwrap_or(-1);
            }
        };

        // Hand the exit code to the pipe-reader threads: whichever finishes
        // LAST appends the final chronological line to the session log, so
        // no game output can land after it.
        crate::game_logs::mark_game_exit(pid_for_exit, exit_code);
        // The game is gone: drop its persisted pid record so the next app
        // start does not try to reconcile a dead process.
        crate::running_state::remove_pid_file(&data_dir_for_exit, &instance_clone);
        // Final playtime commit. This watcher reacts within ~500 ms — long
        // before the 60-second timer would — and the pid guard plus the
        // atomic cursor advance make both paths safe to race. A relaunch
        // that already replaced the session is left untouched.
        {
            let now = Instant::now();
            let app_state = app_clone.state::<AppState>();
            if let Some((name, delta)) = playtime::take_keyed_session_for_pid(
                &app_state.active_sessions,
                &instance_clone,
                pid_for_exit,
                now,
            ) {
                if delta > 0 {
                    playtime::add_minutes_and_save(&data_dir_for_exit, &name, delta);
                }
                events::emit_log(
                    &app_clone,
                    "info",
                    "launch",
                    &format!("Playtime session ended for: {}", name),
                );
            }
        }
        // Clear only THIS instance from the running list. Other games may
        // still be running, so never wipe the whole list.
        if let Ok(mut running) = app_clone.state::<AppState>().running_instances.lock() {
            running.retain(|n| n != &instance_clone);
        }
        events::emit_launch_event(
            &app_clone,
            "info",
            &format!("Game exited with code {}", exit_code),
        );
        // Drop the global "current log" pointer only if this run still owns
        // it — another instance may have started meanwhile and re-pointed it.
        if let Some(p) = session_log_for_exit.as_deref() {
            game_logs::clear_current_log_path_if(p);
        }
        let _ = app_clone.emit(
            "launch_complete",
            events::LaunchEventPayload {
                instance_id: instance_clone,
                status: "exited".into(),
                pid: None,
                exit_code: Some(exit_code),
            },
        );
    });

    events::emit_log(
        &app,
        "info",
        "launch",
        &format!("Game launched: {} (PID: {})", instance_name, pid),
    );

    Ok(())
}

#[tauri::command]
pub async fn cmd_get_fabric_versions(
    mc_version: String,
    offset: usize,
    limit: usize,
) -> Result<modloaders::LoaderVersionPage, String> {
    let limit = if limit == 0 { PAGE_SIZE } else { limit };
    modloaders::fabric::get_loader_versions_for(&mc_version, offset, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_get_forge_versions(
    mc_version: String,
    offset: usize,
    limit: usize,
) -> Result<modloaders::LoaderVersionPage, String> {
    let limit = if limit == 0 { PAGE_SIZE } else { limit };
    modloaders::forge::get_loader_versions(&mc_version, offset, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_get_neoforge_versions(
    mc_version: String,
    offset: usize,
    limit: usize,
) -> Result<modloaders::LoaderVersionPage, String> {
    let limit = if limit == 0 { PAGE_SIZE } else { limit };
    modloaders::neoforge::get_loader_versions(&mc_version, offset, limit)
        .await
        .map_err(|e| e.to_string())
}

/// Reject installing a loader built for a different Minecraft version than
/// the instance uses. Prevents a loader profile for e.g. 26.2 from being
/// applied to an instance running 1.12.2 (can happen when an instance name
/// is reused).
fn ensure_loader_mc_match(
    lang: &str,
    loader_name: &str,
    instance: &instances::Instance,
    mc_version: &str,
) -> Result<(), String> {
    if instance.mc_version != mc_version {
        return Err(i18n::tr(
            lang,
            "loader_version_mismatch",
            &[
                ("loader", loader_name),
                ("instance_name", &instance.name),
                ("instance_version", &instance.mc_version),
                ("loader_version", mc_version),
            ],
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn cmd_install_fabric(
    state: State<'_, AppState>,
    mc_version: String,
    loader_version: String,
    instance_name: String,
    lang: Option<String>,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let lang = lang.as_deref().unwrap_or("en");
    let libraries_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        ensure_loader_mc_match(lang, "Fabric", &instance, &mc_version)?;
        config.libraries_dir()
    };
    let profile = modloaders::fabric::install(&mc_version, &loader_version, &libraries_dir)
        .await
        .map_err(|e| {
            i18n::tr(
                lang,
                "loader_install_failed",
                &[("loader", "Fabric"), ("error", &e.to_string())],
            )
        })?;

    // Save loader profile to instance
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    instance.loader = instances::LoaderType::Fabric;
    instance.loader_version = Some(loader_version.clone());
    instance.loader_profile = Some(profile);
    instances::save_instance(&config.instances_dir(), &instance, None)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn cmd_install_forge(
    state: State<'_, AppState>,
    mc_version: String,
    loader_version: String,
    instance_name: String,
    lang: Option<String>,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let lang = lang.as_deref().unwrap_or("en");
    let (libraries_dir, versions_dir) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        ensure_loader_mc_match(lang, "Forge", &instance, &mc_version)?;
        (config.libraries_dir(), config.versions_dir())
    };
    let profile =
        modloaders::forge::install(&mc_version, &loader_version, &libraries_dir, &versions_dir)
            .await
            .map_err(|e| {
                i18n::tr(
                    lang,
                    "loader_install_failed",
                    &[("loader", "Forge"), ("error", &e.to_string())],
                )
            })?;

    // Save loader profile to instance
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    instance.loader = instances::LoaderType::Forge;
    instance.loader_version = Some(loader_version.clone());
    instance.loader_profile = Some(profile);
    instances::save_instance(&config.instances_dir(), &instance, None)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn cmd_install_neoforge(
    state: State<'_, AppState>,
    mc_version: String,
    loader_version: String,
    instance_name: String,
    lang: Option<String>,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let lang = lang.as_deref().unwrap_or("en");
    let (libraries_dir, versions_dir) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;
        ensure_loader_mc_match(lang, "NeoForge", &instance, &mc_version)?;
        (config.libraries_dir(), config.versions_dir())
    };
    let profile = modloaders::neoforge::install(
        &mc_version,
        &loader_version,
        &libraries_dir,
        &versions_dir,
        None,
    )
    .await
    .map_err(|e| {
        i18n::tr(
            lang,
            "loader_install_failed",
            &[("loader", "NeoForge"), ("error", &e.to_string())],
        )
    })?;

    // Save loader profile to instance
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    instance.loader = instances::LoaderType::NeoForge;
    instance.loader_version = Some(loader_version.clone());
    instance.loader_profile = Some(profile);
    instances::save_instance(&config.instances_dir(), &instance, None)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Install the mod loader for an instance, emitting progress events.
#[tauri::command]
pub async fn cmd_install_instance_loader(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
    lang: Option<String>,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let lang = lang.as_deref().unwrap_or("en");

    let (libraries_dir, versions_dir, loader_type, loader_version, mc_version) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;

        let loader_type = match instance.loader {
            instances::LoaderType::Fabric => "Fabric",
            instances::LoaderType::Forge => "Forge",
            instances::LoaderType::NeoForge => "NeoForge",
            instances::LoaderType::Vanilla => {
                return Err(i18n::tr(lang, "instance_no_loader", &[]));
            }
        };
        let loader_version = instance
            .loader_version
            .clone()
            .ok_or_else(|| i18n::tr(lang, "instance_no_loader_version", &[]))?;
        (
            config.libraries_dir(),
            config.versions_dir(),
            loader_type.to_string(),
            loader_version,
            instance.mc_version.clone(),
        )
    };

    let installing_msg = i18n::tr(
        lang,
        "installing_loader",
        &[
            ("loader", &loader_type),
            ("version", &loader_version),
            ("mc", &mc_version),
        ],
    );
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({ "stage": "start", "message": installing_msg }),
    );
    let downloading_msg = i18n::tr(lang, "downloading_loader_libs", &[("loader", &loader_type)]);
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({ "stage": "download", "message": downloading_msg }),
    );

    let profile = modloaders::install_loader(
        &loader_type,
        &mc_version,
        &loader_version,
        &libraries_dir,
        &versions_dir,
        Some(&app),
    )
    .await
    .map_err(|e| {
        let msg = i18n::tr(
            lang,
            "loader_install_failed",
            &[("loader", &loader_type), ("error", &e.to_string())],
        );
        let _ = app.emit(
            "loader-install-progress",
            serde_json::json!({
                "stage": "error",
                "message": &msg,
            }),
        );
        msg
    })?;

    // Save profile to instance
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    instance.loader_profile = Some(profile);
    instances::save_instance(&config.instances_dir(), &instance, None)
        .map_err(|e| e.to_string())?;

    let msg_done = i18n::tr(lang, "loader_install_success", &[("loader", &loader_type)]);
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({
            "stage": "done",
            "message": msg_done,
        }),
    );

    // Launcher logs stay English regardless of the UI language.
    let msg_saved = format!(
        "{} {} installed for {}",
        loader_type, loader_version, instance_name
    );
    events::emit_log(&app, "info", "loader", &msg_saved);
    Ok(())
}

/// Changes the mod loader of an existing instance to an explicitly chosen
/// version. Unlike `cmd_install_instance_loader` (which installs whatever
/// version the instance already records), the new version is installed
/// first and only then written to the instance together with the profile —
/// a failed install leaves the instance on its previous version.
#[tauri::command]
pub async fn cmd_update_instance_loader(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
    loader_version: String,
    lang: Option<String>,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let lang = lang.as_deref().unwrap_or("en");
    let loader_version = loader_version.trim().to_string();
    if loader_version.is_empty() {
        return Err(i18n::tr(lang, "instance_no_loader_version", &[]));
    }

    let (libraries_dir, versions_dir, loader_type, mc_version, old_version) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let instance = instances::get_instance(&config.instances_dir(), &instance_name)
            .map_err(|e| e.to_string())?;

        let loader_type = match instance.loader {
            instances::LoaderType::Fabric => "Fabric",
            instances::LoaderType::Forge => "Forge",
            instances::LoaderType::NeoForge => "NeoForge",
            instances::LoaderType::Vanilla => {
                return Err(i18n::tr(lang, "instance_no_loader", &[]));
            }
        };
        (
            config.libraries_dir(),
            config.versions_dir(),
            loader_type.to_string(),
            instance.mc_version.clone(),
            instance.loader_version.clone().unwrap_or_default(),
        )
    };

    let installing_msg = i18n::tr(
        lang,
        "installing_loader",
        &[
            ("loader", &loader_type),
            ("version", &loader_version),
            ("mc", &mc_version),
        ],
    );
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({ "stage": "start", "message": installing_msg }),
    );
    let downloading_msg = i18n::tr(lang, "downloading_loader_libs", &[("loader", &loader_type)]);
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({ "stage": "download", "message": downloading_msg }),
    );

    let profile = modloaders::install_loader(
        &loader_type,
        &mc_version,
        &loader_version,
        &libraries_dir,
        &versions_dir,
        Some(&app),
    )
    .await
    .map_err(|e| {
        let msg = i18n::tr(
            lang,
            "loader_install_failed",
            &[("loader", &loader_type), ("error", &e.to_string())],
        );
        let _ = app.emit(
            "loader-install-progress",
            serde_json::json!({
                "stage": "error",
                "message": &msg,
            }),
        );
        msg
    })?;

    // Install succeeded: record the new version and profile together.
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    instance.loader_version = Some(loader_version.clone());
    instance.loader_profile = Some(profile);
    instances::save_instance(&config.instances_dir(), &instance, None)
        .map_err(|e| e.to_string())?;

    let msg_done = i18n::tr(lang, "loader_install_success", &[("loader", &loader_type)]);
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({
            "stage": "done",
            "message": msg_done,
        }),
    );

    // Launcher logs stay English regardless of the UI language.
    let msg_saved = if old_version.is_empty() {
        format!(
            "{} {} installed for {}",
            loader_type, loader_version, instance_name
        )
    } else {
        format!(
            "{} {} updated for {} (was {})",
            loader_type, loader_version, instance_name, old_version
        )
    };
    events::emit_log(&app, "info", "loader", &msg_saved);
    Ok(())
}

#[tauri::command]
pub fn cmd_emit_log(
    app: AppHandle,
    level: String,
    source: String,
    message: String,
) -> Result<(), String> {
    // Whitelist the level enum to prevent arbitrary strings from polluting logs.
    let level_normalized = match level.to_lowercase().as_str() {
        "info" | "warn" | "warning" | "error" | "debug" => level.to_lowercase(),
        _ => return Err("Invalid log level".to_string()),
    };
    // Bound the source length to keep log files readable.
    let safe_source: String = source
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(32)
        .collect();
    if safe_source.is_empty() {
        return Err("Source must contain at least one alphanumeric character".to_string());
    }
    events::emit_log(&app, &level_normalized, &safe_source, &message);
    Ok(())
}
#[tauri::command]
pub async fn cmd_list_game_logs(
    state: State<'_, AppState>,
    instance_name: Option<String>,
) -> Result<Vec<game_logs::GameLogSession>, String> {
    let data_dir = {
        let c = state.config.lock().map_err(|e| e.to_string());
        match c {
            Ok(cfg) => cfg.data_dir.clone(),
            Err(_) => return Ok(Vec::new()),
        }
    };
    tauri::async_runtime::spawn_blocking(move || {
        let mut sessions = game_logs::list_game_log_sessions(&data_dir);
        // Optional per-instance filter (frontend passes the raw instance name;
        // matching uses the same filename-sanitization scheme as creation).
        if let Some(name) = instance_name {
            let wanted = game_logs::sanitize_instance_name(&name);
            sessions.retain(|s| s.instance_name == wanted);
        }
        sessions
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cmd_read_game_log(
    state: State<'_, AppState>,
    path: String,
    max_lines: Option<usize>,
) -> Result<String, String> {
    let data_dir = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.data_dir.clone()
    };
    tauri::async_runtime::spawn_blocking(move || {
        let safe_path = game_logs::validate_log_path(&data_dir, &path)?;
        game_logs::read_game_log(&safe_path, max_lines)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cmd_get_current_game_log() -> Option<String> {
    game_logs::get_current_log_path()
}

#[tauri::command]
pub async fn cmd_delete_game_log(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let data_dir = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.data_dir.clone()
    };
    tauri::async_runtime::spawn_blocking(move || game_logs::delete_game_log(&data_dir, &path))
        .await
        .map_err(|e| e.to_string())?
}

/// Open the `.minecraft/logs` folder of an instance in the file manager.
#[tauri::command]
pub fn cmd_open_instance_logs_folder(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<(), String> {
    crate::commands::instances::validate_instance_name(&instance_name)?;
    let logs_dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config
            .instances_dir()
            .join(&instance_name)
            .join(".minecraft")
            .join("logs")
    };
    crate::commands::misc::cmd_open_folder(state, app, logs_dir.to_string_lossy().to_string())
}
/// Open the root game-logs folder (`%DATA_DIR%/logs/game`) in the file
/// manager. Used by the Game Logs page, which is no longer tied to a
/// specific instance.
#[tauri::command]
pub fn cmd_open_game_logs_root(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let dir = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.data_dir.join("logs").join("game")
    };
    let _ = std::fs::create_dir_all(&dir);
    crate::commands::misc::cmd_open_folder(state, app, dir.to_string_lossy().to_string())
}

#[tauri::command]
pub fn cmd_get_playtime(state: State<'_, AppState>, instance_name: String) -> Result<u64, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    // Return seconds for consistency with play_time_seconds
    Ok(playtime::get_playtime(&config.data_dir, &instance_name) * 60)
}

#[tauri::command]
pub fn cmd_format_playtime(minutes: u64, language: Option<String>) -> String {
    let lang = match language.as_deref() {
        Some("en") => playtime::PlaytimeLang::En,
        // Default to English on unknown / null / "ru" — historically this
        // command always returned Russian, but the launcher's UI now ships
        // in English by default and the playtime label should match.
        _ => playtime::PlaytimeLang::En,
    };
    playtime::format_playtime_in(minutes, lang)
}

/// Flush the active playtime session for a given instance (commits whole
/// minutes and clears that session). Called manually from the frontend
/// (e.g., when the user closes a running game).
#[tauri::command]
pub fn cmd_flush_playtime(state: State<'_, AppState>, instance_name: String) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let now = Instant::now();
    if let Some((name, delta)) =
        playtime::take_keyed_session(&state.active_sessions, &instance_name, now)
    {
        if delta > 0 {
            playtime::add_minutes_and_save(&config.data_dir, &name, delta);
        }
    }
    Ok(())
}

/// Force-stop a running game. Used by the "Running" badge's Stop button and
/// as the recovery path for a game orphaned by a launcher restart/update
/// (there is no in-memory child handle for those — the pid file is the
/// source of truth).
#[tauri::command]
pub fn cmd_stop_game(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<(), String> {
    validate_instance_name(&instance_name)?;
    let data_dir = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.data_dir.clone()
    };

    // Prefer the live session pid (launcher-owned process); fall back to the
    // persisted pid file (orphan from a previous app run).
    let session_pid = state
        .active_sessions
        .lock()
        .ok()
        .and_then(|g| g.get(&instance_name).map(|s| s.pid));
    let pid = session_pid.or_else(|| {
        crate::running_state::scan_live_games(&data_dir)
            .into_iter()
            .find(|g| g.instance == instance_name)
            .map(|g| g.pid)
    });

    let Some(pid) = pid else {
        // Not running anymore — heal the local state so the badge cannot
        // stick forever (e.g. the progress died while the launcher slept).
        crate::running_state::remove_pid_file(&data_dir, &instance_name);
        if let Ok(mut running) = state.running_instances.lock() {
            running.retain(|n| n != &instance_name);
        }
        return Err(format!("Instance '{}' is not running.", instance_name));
    };

    crate::running_state::kill_process_tree(pid)?;
    events::emit_log(
        &app,
        "info",
        "launch",
        &format!("Stopping game '{}' (PID {})...", instance_name, pid),
    );

    let owned = state
        .active_sessions
        .lock()
        .map(|g| g.contains_key(&instance_name))
        .unwrap_or(false);
    if !owned {
        // Orphaned game: no exit watcher exists to clean up after it, so do
        // the bookkeeping here. (The playtime session from the previous app
        // run was already flushed when that app closed.)
        crate::running_state::remove_pid_file(&data_dir, &instance_name);
        if let Ok(mut running) = state.running_instances.lock() {
            running.retain(|n| n != &instance_name);
        }
        let _ = app.emit(
            "launch_complete",
            events::LaunchEventPayload {
                instance_id: instance_name,
                status: "exited".into(),
                pid: None,
                exit_code: None,
            },
        );
    }
    Ok(())
}

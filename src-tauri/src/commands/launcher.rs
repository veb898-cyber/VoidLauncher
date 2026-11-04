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
    )
    .map_err(|e| format!("Invalid version metadata: {}", e))?;

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

/// Ensure a usable Java executable exists and return it. The unified
/// selection policy (`java::resolve_java`) decides; when nothing suitable is
/// installed, the existing managed download path runs and the fresh runtime
/// is re-verified with the M3.1 probe (via the fresh detection list, whose
/// managed entries are re-probed) before the launch proceeds.
///
/// The returned installation (with its probed architecture) is handed to
/// `launch::launch_minecraft` verbatim, so prepare and spawn always run the
/// very same JVM and the native selection follows its probed architecture.
/// An explicitly configured but broken/incompatible Java is reported instead
/// of being replaced by another installation or a download.
async fn ensure_java_for_launch(
    app: &AppHandle,
    data_dir: &Path,
    version_info: &versions::VersionInfo,
    instance: &instances::Instance,
    config: &crate::config::AppConfig,
) -> Result<crate::java::JavaInstallation, String> {
    let req = crate::java::JavaRequirement {
        major: version_info.required_java_major(),
        arch: None,
        require_64bit: true,
        allow_newer: false,
    };

    let installations = launch::find_all_java_installations(data_dir);
    let choice = crate::java::resolve_java(
        &req,
        instance.java_path.as_deref(),
        config.java_path.as_deref(),
        &installations,
    );
    tracing::debug!(
        target: "launcher",
        "Java requirement {} resolved: {:?}",
        version_info.required_java_major(),
        choice
    );

    match crate::java::preparation_from_choice(&choice) {
        Ok(crate::java::JavaPreparation::Ready { path, install }) => {
            tracing::info!(
                target: "launcher",
                "Java is ready: {} v{} at {:?} (arch {:?})",
                install.vendor,
                install.version,
                path,
                install.arch
            );
            Ok(install)
        }
        Ok(crate::java::JavaPreparation::InstallManaged { major }) => {
            events::emit_log(
                app,
                "info",
                "launch",
                &format!(
                    "Java {} required but not found locally. Downloading...",
                    major
                ),
            );

            java_download::download_java_runtime(major, data_dir, app)
                .await
                .map_err(|e| {
                    let msg = format!("Failed to download Java {}: {}", major, e);
                    events::emit_log(app, "error", "launch", &msg);
                    msg
                })?;

            events::emit_log(
                app,
                "info",
                "launch",
                &format!("Java {} downloaded successfully", major),
            );

            // Never trust the downloader's manifest: re-run the detection
            // (managed entries are probed there with the M3.1 probe, so a
            // broken or mis-labelled archive never reaches the game).
            let installations = launch::find_all_java_installations(data_dir);
            let choice = crate::java::resolve_java(
                &req,
                instance.java_path.as_deref(),
                config.java_path.as_deref(),
                &installations,
            );
            match crate::java::preparation_from_choice(&choice) {
                Ok(crate::java::JavaPreparation::Ready { path, install }) => {
                    tracing::info!(
                        target: "launcher",
                        "Downloaded Java verified: {} v{} at {:?} (arch {:?})",
                        install.vendor,
                        install.version,
                        path,
                        install.arch
                    );
                    Ok(install)
                }
                // A second `InstallManaged` after a successful download means
                // the fresh runtime did not survive verification — abort
                // instead of looping or shipping an unverified JVM.
                _ => {
                    let msg = format!(
                        "Java {} was installed, but no usable runtime of that version was found on this system. Install Java {} manually or pick it in the settings.",
                        major, major
                    );
                    events::emit_log(app, "error", "launch", &msg);
                    Err(msg)
                }
            }
        }
        Err(msg) => Err(msg),
    }
}

/// RAII reservation of an instance for the whole launch-preparation phase.
///
/// Acquiring it puts the instance into `Starting` (see
/// [`crate::launch_state`]), which blocks a second launch, a config mutation
/// and an instance deletion while the launcher downloads files and builds the
/// profile. If the command returns early (error before the game starts) the
/// reservation is released automatically; once the process exists
/// [`LaunchReservation::mark_running`] publishes it as `Running` and the exit
/// handler owns the entry from then on.
struct LaunchReservation<'a> {
    state: &'a AppState,
    instance_name: String,
    token: crate::launch_state::LaunchToken,
    /// Still holds a `Starting` entry; becomes false after `mark_running`.
    holds_start: bool,
}

impl<'a> LaunchReservation<'a> {
    fn acquire(state: &'a AppState, instance_name: &str) -> Result<Self, String> {
        let token = state
            .launches
            .begin_launch(instance_name)
            .map_err(|busy| match busy {
                crate::launch_state::LaunchBusy::Starting => format!(
                    "Instance '{}' is already starting. Wait for the game to launch.",
                    instance_name
                ),
                _ => format!(
                    "Instance '{}' is already running. Launch it again after it exits.",
                    instance_name
                ),
            })?;
        Ok(Self {
            state,
            instance_name: instance_name.to_string(),
            token,
            holds_start: true,
        })
    }

    /// The game process exists: `Starting` becomes `Running`, and the identity
    /// of the spawned process is recorded in the same synchronized step so a
    /// later Stop can only ever act on that exact process. The entry is kept
    /// (the exit handler removes it), so the slot is never freed while the game
    /// is up.
    fn mark_running(&mut self, identity: Option<crate::running_state::ProcessIdentity>) {
        if !self.holds_start {
            return;
        }
        self.state
            .launches
            .mark_running(&self.instance_name, self.token, identity);
        self.holds_start = false;
    }

    /// Token identifying this reservation, handed to the exit handler so it
    /// can release exactly this run's entry.
    fn token(&self) -> crate::launch_state::LaunchToken {
        self.token
    }
}

impl Drop for LaunchReservation<'_> {
    fn drop(&mut self) {
        if !self.holds_start {
            return;
        }
        self.state.launches.release(&self.instance_name, self.token);
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
    //
    // Only a record whose live process matches the recorded identity exactly
    // (executable + exact creation time) is adopted: a pid alone proves
    // nothing, because Windows reuses pids.
    let scan = crate::running_state::scan_games(&data_dir);
    for game in &scan.confirmed {
        state
            .launches
            .adopt_running(&game.instance, game.identity());
    }
    if let Some(stale) = scan
        .unverified
        .iter()
        .find(|game| game.instance == instance_name)
    {
        // Legacy record: the pid is occupied, but nothing proves it is this
        // instance's game. Never trust it (no badge, never killed) — and never
        // start a second copy on the same instance either.
        let msg = format!(
            "Instance '{}' may still be running (process {}), but the launcher cannot verify it was started by it: the record was written by an older version. Close the game manually, then try again.",
            instance_name, stale.pid
        );
        events::emit_log(&app, "warn", "launch", &msg);
        return Err(msg);
    }

    // Guard against launching the same instance twice concurrently — a
    // second click on a running instance must be a no-op, not a second copy.
    // The instance enters `Starting` NOW (before the long download/prepare
    // phase) and is released automatically if this launch fails before the
    // game starts; on success it becomes `Running` until the process exits
    // (see the exit handler).
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

    // Version strings become directory/file names at every stage below.
    // A tampered instance.json must fail with a clear reason, not be joined
    // onto `versions/` or `libraries/`.
    if crate::import::check_safe_file_name(&instance.mc_version).is_err()
        || instance
            .loader_version
            .as_deref()
            .filter(|v| !v.is_empty())
            .is_some_and(|v| crate::import::check_safe_file_name(v).is_err())
    {
        let msg = "Instance settings contain an invalid Minecraft or loader version.".to_string();
        events::emit_log(&app, "error", "launch", &msg);
        return Err(msg);
    }

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

    // The id and asset-index name become directory/file names below.
    crate::import::check_safe_file_name(&version_info.id).map_err(|_| {
        let msg = "Invalid version id in remote metadata".to_string();
        events::emit_log(&app, "error", "launch", &msg);
        msg
    })?;
    crate::import::check_safe_file_name(&version_info.assets).map_err(|_| {
        let msg = "Invalid asset index name in remote metadata".to_string();
        events::emit_log(&app, "error", "launch", &msg);
        msg
    })?;

    // Ensure a usable Java installation exists (download + verify through the
    // unified policy if missing). The returned installation — probed by M3.1,
    // so its architecture is known — is the ONE JVM for this run: the spawn
    // step below receives it verbatim and must not pick its own. The probe's
    // architecture also decides the native selection everywhere below, which
    // is why this runs BEFORE any version-file download.
    let prepared_java =
        ensure_java_for_launch(&app, &data_dir, &version_info, &instance, &config).await?;
    let launch_target = crate::versions::TargetArch::from_java_arch(prepared_java.arch);

    // Self-heal: ensure the version's files (client.jar, libraries, and the
    // native classifier JARs of the chosen JVM's architecture) are on disk.
    // They are normally downloaded when the instance is created, but a failed
    // creation or a manually removed file must not break the launch.
    events::emit_log(
        &app,
        "info",
        "launch",
        "Ensuring version files are present...",
    );
    let files = versions::collect_downloads_for(
        &version_info,
        &config.libraries_dir(),
        &config.versions_dir(),
        launch_target,
    )
    .map_err(|e| {
        let msg = format!("Invalid version metadata: {}", e);
        events::emit_log(&app, "error", "launch", &msg);
        msg
    })?;
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
                    let msg = format!("Failed to prepare Forge launch files: {}", e);
                    events::emit_log(&app, "error", "launch", &msg);
                    return Err(msg);
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
                    let msg = format!("Failed to prepare NeoForge launch files: {}", e);
                    events::emit_log(&app, "error", "launch", &msg);
                    return Err(msg);
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
    // predate native-classifier downloads). Missing natives are a required
    // part of the game runtime — continuing would only crash Java later with
    // an unrelated-looking error. Same architecture as the downloads above.
    if let Err(e) =
        versions::ensure_native_libraries_for(&version_info, &config.libraries_dir(), launch_target)
            .await
    {
        let msg = format!("Failed to prepare native libraries: {}", e);
        events::emit_log(&app, "error", "launch", &msg);
        return Err(msg);
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
    let mut child = {
        let launch_config = config.clone();
        let launch_instance = instance.clone();
        let launch_version_info = version_info.clone();
        let launch_token = access_token.clone();
        let launch_uuid = uuid.clone();
        let launch_username = username.clone();
        let launch_server = server_address.clone();
        let launch_session_log = session_log.clone();
        let launch_java = prepared_java.clone();
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
                Some(&launch_java),
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
    // Read the exact identity of the process we just spawned (through its own
    // handle, so it cannot describe any other process). It is persisted below
    // and published through the registry, and every later Stop re-verifies it.
    let process_identity = crate::running_state::identity_of_child(&child);
    let _ = instances::update_last_played(&config.instances_dir(), &instance_name);
    // Persist the pid BEFORE anything else can fail: if the launcher is
    // closed or updated while the game runs, the next start recovers the
    // still-running game from this file (badge + Stop + relaunch guard).
    if let Err(cause) = crate::running_state::write_pid_file(
        &data_dir,
        &instance_name,
        pid,
        process_identity.as_ref(),
    ) {
        // The game exists, but nothing on disk would let the launcher find it
        // again after a crash or an update. Do not report a registered launch:
        // stop the process we just started (re-verifying its identity), or at
        // least say loudly that it may still be running.
        let outcome = stop_unpersisted_child(&mut child, process_identity.as_ref());
        let msg = unpersisted_launch_error(&instance_name, pid, &cause, &outcome);
        events::emit_log(&app, "error", "launch", &msg);
        if let Some(p) = session_log.as_deref() {
            game_logs::clear_current_log_path_if(p);
        }
        // The reservation is still `Starting`; returning drops it.
        return Err(msg);
    }
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
                // The reservation is still armed: returning drops it and
                // releases the `Starting` entry automatically.
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

    // The process exists: publish this instance as `Running` (it stops being
    // `Starting`) together with the verified process identity, and keep it
    // reserved until the exit handler releases the entry. Multiple games may
    // run at once.
    reservation.mark_running(process_identity);
    let launch_token = reservation.token();

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
        // Release exactly this run's entry: a newer launch of the same
        // instance (if one ever follows) is never freed by a stale watcher.
        app_clone
            .state::<AppState>()
            .launches
            .release(&instance_clone, launch_token);
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

/// What `cmd_stop_game` can do when no process pid was found for the instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoPidStop {
    /// The launch is still being prepared: the reservation is alive and no
    /// process exists yet. Stop must not pretend the launch is over.
    StillStarting,
    /// A config/resource-pack mutation holds the instance right now.
    BeingModified,
    /// No live reservation: heal a stale `Running` record, if any.
    Heal,
}

fn no_pid_stop(phase: Option<crate::launch_state::LaunchPhase>) -> NoPidStop {
    match phase {
        Some(crate::launch_state::LaunchPhase::Starting) => NoPidStop::StillStarting,
        Some(crate::launch_state::LaunchPhase::Editing) => NoPidStop::BeingModified,
        _ => NoPidStop::Heal,
    }
}

/// The process the Stop button may act on, decided from the synchronized
/// in-memory state and the verified pid records.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StopTarget {
    /// Terminate exactly this process (it was verified against its identity).
    Kill(crate::running_state::ProcessIdentity),
    /// A live process may be the game, but its identity cannot be proven.
    Unverified { pid: u32 },
    /// Nothing is running.
    Idle,
}

/// Pick what Stop should do.
///
/// The registry entry (written at the `Starting` -> `Running` transition, or
/// when a pid record was adopted) wins over the persisted record: it belongs to
/// the newest run, whereas a file could still describe a previous one. A record
/// is only a kill target when its identity matched the live process exactly.
fn stop_target(
    session_identity: Option<crate::running_state::ProcessIdentity>,
    scan: &crate::running_state::LiveScan,
    instance_name: &str,
) -> StopTarget {
    if let Some(identity) = session_identity {
        return StopTarget::Kill(identity);
    }
    if let Some(rec) = scan
        .confirmed
        .iter()
        .find(|game| game.instance == instance_name)
    {
        if let Some(identity) = rec.identity() {
            return StopTarget::Kill(identity);
        }
    }
    if let Some(rec) = scan
        .unverified
        .iter()
        .find(|game| game.instance == instance_name)
    {
        return StopTarget::Unverified { pid: rec.pid };
    }
    StopTarget::Idle
}

/// Outcome of stopping a game whose running state could not be persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StopOutcome {
    /// The process is gone.
    Stopped,
    /// Could not be stopped safely; the string is the underlying reason.
    StillRunning(String),
}

/// Stop a freshly spawned game whose running state could not be saved.
///
/// The verified `kill_process_tree` path is tried first (exactly what Stop
/// does). If it refuses or fails, the launcher falls back to the child handle
/// it owns: that handle refers to the process the launcher itself spawned, so
/// it cannot terminate a recycled pid even when no identity could be read.
fn stop_unpersisted_child(
    child: &mut std::process::Child,
    identity: Option<&crate::running_state::ProcessIdentity>,
) -> StopOutcome {
    let mut reason = String::new();
    if let Some(identity) = identity {
        match crate::running_state::kill_process_tree(identity) {
            Ok(()) => {
                let _ = child.wait();
                return StopOutcome::Stopped;
            }
            Err(err) => reason = err,
        }
    }
    // The identity we were given is missing or unusable. The handle we own
    // describes exactly this process, so read its identity from there and
    // retry the verified tree kill (taskkill /T still covers helper processes).
    if let Some(live) = crate::running_state::identity_of_child(child) {
        match crate::running_state::kill_process_tree(&live) {
            Ok(()) => {
                let _ = child.wait();
                return StopOutcome::Stopped;
            }
            Err(err) => reason = err,
        }
    }
    // Last resort: the owned handle is exact and cannot hit another process,
    // although it may leave helper processes behind.
    if child.kill().is_ok() {
        let _ = child.wait();
        return StopOutcome::Stopped;
    }
    // The process may have exited on its own between the attempts.
    if matches!(child.try_wait(), Ok(Some(_))) {
        return StopOutcome::Stopped;
    }
    StopOutcome::StillRunning(if reason.is_empty() {
        "the launcher has no verified identity for the process".to_string()
    } else {
        reason
    })
}

/// Message for a launch whose process exists but whose state was not saved.
/// A failure must never be reported as a successful launch, and a process that
/// could not be stopped must never disappear silently.
fn unpersisted_launch_error(
    instance_name: &str,
    pid: u32,
    cause: &str,
    outcome: &StopOutcome,
) -> String {
    match outcome {
        StopOutcome::Stopped => format!(
            "The game for '{}' was started, but its running state could not be saved ({}). The started game was stopped, because the launcher would not be able to track it.",
            instance_name, cause
        ),
        StopOutcome::StillRunning(reason) => format!(
            "The game for '{}' was started, but its running state could not be saved ({}). The launcher could not stop it either ({}), so a game process may still be running (PID {}). Close it manually.",
            instance_name, cause, reason, pid
        ),
    }
}

/// Force-stop a running game. Used by the "Running" badge's Stop button and
/// as the recovery path for a game orphaned by a launcher restart/update
/// (there is no in-memory child handle for those — the pid file is the
/// source of truth).
///
/// Stop is not a cancel: while the instance is still `Starting` there is no
/// process to kill, so the launch keeps going. The reservation is preserved
/// (the instance must stay protected until the game starts or the launch
/// fails) and the caller gets an explanation instead of "not running".
///
/// Stop never trusts a bare pid: the process is re-verified against the
/// identity recorded at launch (executable + exact creation time) right before
/// the kill, and the verification handle is kept open across `taskkill` so a
/// recycled pid cannot be hit by mistake.
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

    // A launcher-owned game is published in the registry together with the
    // identity of the process it spawned; an orphaned game is recovered from a
    // pid file only when that file's identity is verified.
    let session_identity = state.launches.process_identity(&instance_name);
    let scan = crate::running_state::scan_games(&data_dir);

    let identity = match stop_target(session_identity, &scan, &instance_name) {
        StopTarget::Kill(identity) => identity,
        StopTarget::Unverified { pid } => {
            // The file has no identity (older launcher) and the pid is taken.
            // Killing is forbidden; the user has to close the game by hand.
            return Err(format!(
                "Instance '{}' may still be running as process {}, but the launcher cannot verify it was started by it: the record was written by an older version. Close the game manually, then try again.",
                instance_name, pid
            ));
        }
        StopTarget::Idle => {
            match no_pid_stop(state.launches.phase(&instance_name)) {
                NoPidStop::StillStarting => {
                    return Err(format!(
                        "Instance '{}' is still starting. Wait for the game to launch, then stop it.",
                        instance_name
                    ));
                }
                NoPidStop::BeingModified => {
                    return Err(format!(
                        "Instance '{}' is being modified. Try again in a moment.",
                        instance_name
                    ));
                }
                NoPidStop::Heal => {}
            }
            // Not running anymore — heal the local state so the badge cannot
            // stick forever (e.g. the process died while the launcher slept).
            // Only a `Running` entry is cleared; a preparation that slipped
            // through would keep its reservation.
            crate::running_state::remove_pid_file(&data_dir, &instance_name);
            state.launches.clear_running(&instance_name);
            return Err(format!("Instance '{}' is not running.", instance_name));
        }
    };

    crate::running_state::kill_process_tree(&identity)?;
    events::emit_log(
        &app,
        "info",
        "launch",
        &format!(
            "Stopping game '{}' (PID {})...",
            instance_name, identity.pid
        ),
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
        state.launches.clear_running(&instance_name);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch_state::{LaunchPhase, LaunchRegistry};

    /// `AppState` with a launch registry and no Tauri app.
    fn test_state() -> AppState {
        let dir = std::env::temp_dir().join(format!(
            "vl-launch-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before unix epoch")
                .as_nanos()
        ));
        AppState {
            config: std::sync::Mutex::new(crate::AppConfig::default()),
            auth_state: std::sync::Mutex::new(crate::auth::AuthState::default()),
            launches: LaunchRegistry::new(),
            pack_watcher: std::sync::Mutex::new(None),
            active_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            room_state: std::sync::Arc::new(crate::rooms::room_state::RoomStateManager::new(&dir)),
            tailscale: crate::rooms::tailscale::TailscaleManager::new(dir),
        }
    }

    #[test]
    fn reservation_enters_starting_and_blocks_a_second_launch() {
        let state = test_state();
        let reservation = LaunchReservation::acquire(&state, "Pack")
            .expect("the first launch reserves the instance");

        assert_eq!(state.launches.phase("Pack"), Some(LaunchPhase::Starting));
        assert!(
            state.launches.running_instances().is_empty(),
            "a preparing instance is not a running game"
        );

        let err = LaunchReservation::acquire(&state, "Pack")
            .err()
            .expect("the second launch must be rejected");
        assert!(err.contains("already starting"), "got: {err}");

        drop(reservation);
    }

    #[test]
    fn a_failed_preparation_releases_the_reservation() {
        let state = test_state();
        {
            // Mirrors an early `return Err(...)` in `cmd_launch_game`: the
            // local reservation is dropped on every error path.
            let _reservation = LaunchReservation::acquire(&state, "Pack").expect("reserve");
        }
        assert_eq!(state.launches.phase("Pack"), None);
        assert!(
            LaunchReservation::acquire(&state, "Pack").is_ok(),
            "the instance must be launchable again after a failed preparation"
        );
    }

    #[test]
    fn a_successful_spawn_moves_starting_to_running_and_keeps_it_reserved() {
        let state = test_state();
        let mut reservation = LaunchReservation::acquire(&state, "Pack").expect("reserve");
        let token = reservation.token();

        reservation.mark_running(None);
        assert_eq!(state.launches.phase("Pack"), Some(LaunchPhase::Running));
        assert_eq!(state.launches.running_instances(), vec!["Pack".to_string()]);

        // Dropping the reservation must not free a running game: the exit
        // handler owns the entry now.
        drop(reservation);
        assert_eq!(state.launches.phase("Pack"), Some(LaunchPhase::Running));

        // The exit handler releases exactly this run's entry.
        assert!(state.launches.release("Pack", token));
        assert_eq!(state.launches.phase("Pack"), None);
    }

    #[test]
    fn stop_during_starting_keeps_the_reservation() {
        let state = test_state();
        let reservation = LaunchReservation::acquire(&state, "Pack").expect("reserve");

        // No process exists yet, so the Stop path takes the "no pid" branch.
        assert_eq!(
            no_pid_stop(state.launches.phase("Pack")),
            NoPidStop::StillStarting
        );
        // ... and must not heal/clear the slot.
        assert!(!state.launches.clear_running("Pack"));
        assert_eq!(state.launches.phase("Pack"), Some(LaunchPhase::Starting));

        // A second Play is still rejected while Stop was pressed.
        assert!(LaunchReservation::acquire(&state, "Pack").is_err());

        drop(reservation);
    }

    #[test]
    fn stop_after_running_keeps_the_existing_heal_behavior() {
        let state = test_state();
        let mut reservation = LaunchReservation::acquire(&state, "Pack").expect("reserve");
        reservation.mark_running(None);
        drop(reservation);

        // Once the process is gone (pid file already removed), the stale
        // `Running` entry is cleared and the caller is told it is not running.
        assert_eq!(no_pid_stop(state.launches.phase("Pack")), NoPidStop::Heal);
        assert!(state.launches.clear_running("Pack"));
        assert_eq!(state.launches.phase("Pack"), None);
    }

    #[test]
    fn stop_while_the_instance_is_being_modified_does_not_clear_it() {
        let state = test_state();
        let edit = state.launches.begin_edit("Pack").expect("reserve for edit");
        assert_eq!(
            no_pid_stop(state.launches.phase("Pack")),
            NoPidStop::BeingModified
        );
        assert!(!state.launches.clear_running("Pack"));
        assert!(state.launches.release("Pack", edit));
    }

    // ---- process identity in the Stop path -------------------------------

    fn identity(pid: u32, created: u64) -> crate::running_state::ProcessIdentity {
        crate::running_state::ProcessIdentity {
            pid,
            exe: r"c:\java\bin\java.exe".to_string(),
            created,
        }
    }

    fn record(instance: &str, pid: u32, created: u64) -> crate::running_state::RunningGame {
        crate::running_state::RunningGame {
            instance: instance.to_string(),
            pid,
            started_at: "2026-01-02T03:04:05+03:00".to_string(),
            exe: Some(r"c:\java\bin\java.exe".to_string()),
            created: Some(created.to_string()),
        }
    }

    fn scan_with(
        confirmed: Vec<crate::running_state::RunningGame>,
        unverified: Vec<crate::running_state::RunningGame>,
    ) -> crate::running_state::LiveScan {
        crate::running_state::LiveScan {
            confirmed,
            unverified,
        }
    }

    #[test]
    fn stop_kills_a_verified_record_with_its_own_identity() {
        let rec = record("Pack", 4242, 134_512_345_678_901_234);
        let scan = scan_with(vec![rec.clone()], Vec::new());
        assert_eq!(
            stop_target(None, &scan, "Pack"),
            StopTarget::Kill(rec.identity().expect("verified identity"))
        );
    }

    #[test]
    fn stop_refuses_a_record_whose_identity_cannot_be_verified() {
        // A legacy-shaped record: a live pid, no identity. It must never become
        // a kill target — only a refusal with an explanation.
        let legacy = crate::running_state::RunningGame {
            instance: "Pack".to_string(),
            pid: 4242,
            started_at: "2026-01-02T03:04:05+03:00".to_string(),
            exe: None,
            created: None,
        };
        let scan = scan_with(Vec::new(), vec![legacy]);
        assert_eq!(
            stop_target(None, &scan, "Pack"),
            StopTarget::Unverified { pid: 4242 }
        );
    }

    #[test]
    fn stop_never_uses_a_stale_record_for_a_newer_run() {
        // A new game is running as pid 222; an old record still mentions 111.
        // The registry (newest run) must win — the stale pid is never touched.
        let scan = scan_with(vec![record("Pack", 111, 1)], Vec::new());
        let session = identity(222, 2);
        assert_eq!(
            stop_target(Some(session.clone()), &scan, "Pack"),
            StopTarget::Kill(session)
        );

        // The registry identity wins over an unverified record too.
        let scan = scan_with(Vec::new(), vec![record("Pack", 111, 1)]);
        let session = identity(222, 2);
        assert_eq!(
            stop_target(Some(session.clone()), &scan, "Pack"),
            StopTarget::Kill(session)
        );
    }

    #[test]
    fn stop_without_any_process_is_idle() {
        assert_eq!(
            stop_target(None, &scan_with(Vec::new(), Vec::new()), "Pack"),
            StopTarget::Idle
        );
        // Records of another instance never leak into this one.
        let scan = scan_with(vec![record("Other", 111, 1)], Vec::new());
        assert_eq!(stop_target(None, &scan, "Pack"), StopTarget::Idle);
    }

    // ---- state persistence after spawn -----------------------------------

    #[test]
    fn an_unpersisted_launch_reports_a_possible_running_process() {
        let stopped = unpersisted_launch_error("Pack", 4242, "disk full", &StopOutcome::Stopped);
        assert!(stopped.contains("could not be saved"), "got: {stopped}");
        assert!(stopped.contains("was stopped"), "got: {stopped}");

        let running = unpersisted_launch_error(
            "Pack",
            4242,
            "disk full",
            &StopOutcome::StillRunning("access denied".to_string()),
        );
        assert!(running.contains("could not be saved"), "got: {running}");
        assert!(running.contains("disk full"), "the cause must be reported");
        assert!(
            running.contains("4242"),
            "the pid must be reported so the user can find the process: {running}"
        );
        assert!(running.contains("may still be running"), "got: {running}");
        assert!(running.contains("access denied"));
    }

    #[test]
    #[cfg(windows)]
    fn an_unpersisted_child_is_stopped_by_its_real_identity() {
        let mut child = spawn_test_process();
        let pid = child.id();
        let forged = crate::running_state::ProcessIdentity {
            pid,
            exe: r"c:\nope\java.exe".to_string(),
            created: 1,
        };
        // The given identity is refused by the verified path ...
        assert!(crate::running_state::kill_process_tree(&forged).is_err());
        // ... so the launcher re-reads the identity from the handle it owns and
        // stops the whole tree; a persistence failure never leaves an
        // untracked game (or its helper processes) behind.
        assert_eq!(
            stop_unpersisted_child(&mut child, Some(&forged)),
            StopOutcome::Stopped
        );
        assert!(
            child.try_wait().expect("wait").is_some(),
            "the spawned process must be gone"
        );
        // After the stop the pid must never look like a live game again: the
        // process is either gone, or its identity is no longer readable while
        // the terminated process object is being torn down.
        let probe = crate::running_state::read_identity(pid);
        assert!(
            !matches!(probe, crate::running_state::IdentityProbe::Found(_)),
            "the stopped process must not be reported as running, got: {probe:?}"
        );
    }

    #[cfg(windows)]
    fn spawn_test_process() -> std::process::Child {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .args(["/C", "ping", "-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawn a disposable test process")
    }
}

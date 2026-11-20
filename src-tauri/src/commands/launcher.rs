// ==================== Launcher Commands ====================
// Version installation, game launch, mod loader installation,
// playtime tracking, and game-log commands.

use crate::commands::instances::validate_instance_name;
use crate::download;
use crate::events::{self, ProgressSender};
use crate::game_logs;
use crate::install_progress::{self, InstallProgressTracker};
use crate::instances;
use crate::is_allowed_download_host;
use crate::java_download;
use crate::jvm::{self, GcFallbackReason, GcPreset};
use crate::launch;
use crate::modloaders;
use crate::playtime;
use crate::verify;
use crate::versions;
use crate::AppState;
use crate::{accounts, i18n};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

/// All `cmd_get_*_versions` commands return a `LoaderVersionPage`
/// (a slice of the full sorted version list plus `total`). The wizard
/// uses infinite scroll with `PAGE_SIZE` items per request.
const PAGE_SIZE: usize = 20;

/// Load the version JSON saved by `cmd_install_version`. A missing, corrupt
/// or mismatched file returns `None` so the caller can fall back to Mojang.
fn load_local_version_info(versions_dir: &Path, mc_version: &str) -> Option<versions::VersionInfo> {
    if crate::import::check_safe_file_name(mc_version).is_err() {
        return None;
    }
    let path = versions_dir
        .join(mc_version)
        .join(format!("{mc_version}.json"));
    let contents = std::fs::read_to_string(&path).ok()?;
    let info: versions::VersionInfo = serde_json::from_str(&contents).ok()?;
    (info.id == mc_version).then_some(info)
}

#[tauri::command]
pub async fn cmd_install_version(
    app: AppHandle,
    state: State<'_, AppState>,
    version_url: String,
    instance_id: String,
    install_id: Option<String>,
) -> Result<String, String> {
    let config = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.clone()
    };

    let install_id = install_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Create progress channel and bridge. The bridge only forwards events;
    // `done`/`error` are explicit so a failed install can never emit 100%.
    let (progress_tx, rx) = ProgressSender::new();
    events::spawn_event_bridge(app.clone(), rx);
    let tracker = InstallProgressTracker::new(install_id, instance_id.clone(), progress_tx);

    let outcome: Result<String, String> = async {
        tracker.set_stage("manifest", "Fetching version info...");
        events::emit_log(&app, "info", "install", "Fetching version info...");

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

        tracker.set_stage("manifest", "Collecting files to download...");

        // Full plan (cached files included): the downloader validates cache
        // entries and counts valid ones as completed work.
        let files = versions::collect_download_plan(
            &version_info,
            &config.libraries_dir(),
            &config.versions_dir(),
        )
        .map_err(|e| format!("Invalid version metadata: {}", e))?;

        events::emit_log(
            &app,
            "info",
            "install",
            &format!("Downloading {} game files...", files.len()),
        );

        tracker.add_files(
            "libraries",
            "Downloading libraries...",
            install_progress::planned_files(&files),
        );

        let progress_tracker = tracker.clone();
        let file_progress =
            move |path: &std::path::Path, prepared: u64, total: Option<u64>, network: u64| {
                progress_tracker.file_progress(
                    &install_progress::file_key(path),
                    prepared,
                    total,
                    network,
                );
            };
        let done_tracker = tracker.clone();
        let file_done = move |path: &std::path::Path, actual_size: u64| {
            done_tracker.file_completed(&install_progress::file_key(path), actual_size);
        };
        download::download_files_observed(
            files,
            download::DownloadCachePolicy::VerifySha1,
            |_completed, _total, _bytes_done, _bytes_total, _message| {},
            file_progress,
            file_done,
        )
        .await
        .map_err(|e| e.to_string())?;

        tracker.set_stage("libraries", "Saving version metadata...");

        // Save version JSON
        let version_dir = config.versions_dir().join(&version_info.id);
        std::fs::create_dir_all(&version_dir).map_err(|e| e.to_string())?;
        let version_json_path = version_dir.join(format!("{}.json", version_info.id));
        let json = serde_json::to_string_pretty(&version_info).map_err(|e| e.to_string())?;
        std::fs::write(version_json_path, json).map_err(|e| e.to_string())?;

        tracker.set_stage("assets", "Downloading asset index...");

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

        // Now that the index is known, the full download plan exists. Cached
        // asset objects are counted as completed by the downloader.
        let asset_files = download::collect_asset_download_plan(&asset_index, &config.assets_dir())
            .map_err(|e| e.to_string())?;
        tracker.add_files(
            "assets",
            "Downloading assets...",
            install_progress::planned_files(&asset_files),
        );
        tracker.mark_plan_complete();

        let progress_tracker = tracker.clone();
        let asset_progress =
            move |path: &std::path::Path, prepared: u64, total: Option<u64>, network: u64| {
                progress_tracker.file_progress(
                    &install_progress::file_key(path),
                    prepared,
                    total,
                    network,
                );
            };
        let done_tracker = tracker.clone();
        let asset_done = move |path: &std::path::Path, actual_size: u64| {
            done_tracker.file_completed(&install_progress::file_key(path), actual_size);
        };
        download::download_files_observed(
            asset_files,
            download::DownloadCachePolicy::VerifySha1,
            |_completed, _total, _bytes_done, _bytes_total, _message| {},
            asset_progress,
            asset_done,
        )
        .await
        .map_err(|e| e.to_string())?;

        // Mirror objects into virtual/legacy for old versions (1.6.x and older
        // read flat files from --assetsDir; 1.7.x/1.8.x use it as legacy layout).
        tracker.set_stage("finalizing", "Preparing assets...");
        download::ensure_virtual_assets(&version_info.assets, &asset_index, &config.assets_dir())
            .map_err(|e| e.to_string())?;

        events::emit_log(&app, "info", "install", "Installation complete!");
        tracker.done();
        Ok(version_info.id)
    }
    .await;

    if let Err(error) = &outcome {
        tracker.fail("error", error);
    }
    outcome
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
/// Load the local asset index, or restore it from Mojang when it is missing or
/// structurally invalid. Launch must never treat "the file exists" as proof
/// that its contents are usable.
async fn ensure_asset_index_for_launch(
    app: &AppHandle,
    config: &crate::config::AppConfig,
    version_info: &versions::VersionInfo,
) -> Result<versions::AssetIndexData, String> {
    if crate::import::check_safe_file_name(&version_info.assets).is_err() {
        return Err(format!(
            "The Minecraft version metadata names an unsafe asset index: {:?}",
            version_info.assets
        ));
    }
    let index_path = config
        .assets_dir()
        .join("indexes")
        .join(format!("{}.json", version_info.assets));
    let mut local_problem = None;
    if index_path.exists() {
        match versions::load_asset_index(&index_path)
            .and_then(|index| verify::validate_asset_index(&index).map(|_| index))
        {
            Ok(index) => return Ok(index),
            Err(e) => {
                local_problem = Some(e.to_string());
                events::emit_log(
                    app,
                    "warn",
                    "launch",
                    &format!(
                        "Local asset index {} is unusable ({}); restoring it from Mojang.",
                        version_info.assets, e
                    ),
                );
            }
        }
    }
    if !crate::is_allowed_download_host(&version_info.asset_index.url) {
        return Err(format!(
            "Minecraft assets are incomplete: the asset index host is not allowed ({}).",
            version_info.asset_index.url
        ));
    }
    let index = versions::fetch_asset_index(&version_info.asset_index.url)
        .await
        .map_err(|e| {
            let reason = local_problem
                .as_deref()
                .map(|problem| format!(" the local index is invalid ({problem});"))
                .unwrap_or_default();
            format!(
                "Minecraft assets are incomplete:{reason} the asset index could not be \
                 downloaded. Connect to the internet or run Repair. ({e})"
            )
        })?;
    verify::validate_asset_index(&index).map_err(|e| {
        format!(
            "The downloaded asset index for Minecraft {} is invalid: {e}",
            version_info.assets
        )
    })?;
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&index).map_err(|e| e.to_string())?;
    let temp = index_path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        version_info.assets,
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&temp, json).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(&temp, &index_path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("The restored asset index could not be saved: {e}"));
    }
    events::emit_log(
        app,
        "info",
        "launch",
        &format!("Restored asset index {} from Mojang.", version_info.assets),
    );
    Ok(index)
}

/// Verify every asset object before launch, then download only the objects
/// that are missing, truncated or fail their hash. Offline this returns a
/// clear error instead of letting the game start with an incomplete asset set.
async fn ensure_assets_for_launch(
    app: &AppHandle,
    config: &crate::config::AppConfig,
    version_info: &versions::VersionInfo,
    index: &versions::AssetIndexData,
) -> Result<(), String> {
    let check = verify::check_asset_objects_for_launch(&config.assets_dir(), index)
        .map_err(|e| format!("Minecraft assets could not be validated: {e}"))?;
    if check.is_healthy() {
        tracing::info!(
            target: "launcher",
            "Assets verified: {} hashed, {} cached, 0 problems",
            check.hashed,
            check.cached
        );
    } else {
        events::emit_log(
            app,
            "info",
            "launch",
            &format!(
                "Repairing {} missing or damaged Minecraft asset object(s)...",
                check.issue_count()
            ),
        );
        let forced: std::collections::HashSet<PathBuf> = check.paths.iter().cloned().collect();
        download::download_assets_repair(index, &config.assets_dir(), &forced, |completed, total, bytes_done, bytes_total, _msg| {
            tracing::info!(target: "launcher", "Asset repair {}/{} ({} / {} bytes)", completed, total, bytes_done, bytes_total);
        })
        .await
        .map_err(|e| {
            format!(
                "Minecraft assets are incomplete: {} object(s) are missing or damaged and could \
                 not be downloaded. Connect to the internet or run Repair. ({e})",
                check.issue_count()
            )
        })?;
        let recheck = verify::check_asset_objects_for_launch(&config.assets_dir(), index)
            .map_err(|e| format!("Minecraft assets could not be validated: {e}"))?;
        if !recheck.is_healthy() {
            let names: Vec<String> = recheck
                .paths
                .iter()
                .take(5)
                .filter_map(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string)
                })
                .collect();
            return Err(format!(
                "Minecraft assets are still incomplete after the download ({} object(s) missing \
                 or damaged{}). Connect to the internet or run Repair.",
                recheck.issue_count(),
                if names.is_empty() {
                    String::new()
                } else {
                    format!(": {}", names.join(", "))
                }
            ));
        }
    }
    // Virtual/legacy mirror for old versions (1.8 and older).
    download::ensure_virtual_assets(&version_info.assets, index, &config.assets_dir())
        .map_err(|e| format!("Legacy Minecraft assets could not be prepared: {e}"))?;
    Ok(())
}

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
                crate::launch_state::LaunchBusy::Editing => format!(
                    "Instance '{}' is being modified. Wait for the current operation to finish.",
                    instance_name
                ),
                crate::launch_state::LaunchBusy::Running => format!(
                    "Instance '{}' is already running. Launch it again after it exits.",
                    instance_name
                ),
                crate::launch_state::LaunchBusy::Preparing => format!(
                    "Instance '{}' is still being set up. Wait for the installation to finish.",
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

/// Backend launch gate: reserve the single-flight slot, then verify the
/// persistent readiness marker. Keeping both steps together is what makes the
/// check race-free — an import either holds the slot (this fails) or the slot
/// is ours until the marker is read, so it cannot publish/remove the directory
/// in between. Used by `cmd_launch_game`; tested directly without an AppHandle.
fn acquire_launch_gate<'a>(
    state: &'a AppState,
    instance_name: &str,
) -> Result<LaunchReservation<'a>, String> {
    let reservation = LaunchReservation::acquire(state, instance_name)?;
    let instances_dir = state
        .config
        .lock()
        .map_err(|e| e.to_string())?
        .instances_dir();
    crate::instance_readiness::ensure_launch_ready(
        &instances_dir.join(instance_name),
        instance_name,
    )
    .map_err(|e| e.to_string())?;
    Ok(reservation)
}

/// The account the launcher would use for a normal launch: the explicit
/// default, or the first stored account as the existing fallback.
fn should_bypass_elyby_skins(is_elyby: bool, enabled: bool, skip: bool) -> bool {
    skip && is_elyby && enabled
}

fn active_account(data_dir: &std::path::Path) -> Option<accounts::AccountEntry> {
    let list = accounts::list_accounts(data_dir);
    list.iter()
        .find(|a| a.default)
        .or_else(|| list.first())
        .cloned()
}

/// Result of the UI preflight. Expected preparation failures are returned as
/// `Failed` instead of a command error so the renderer can show the retry /
/// one-time bypass / cancel dialog without parsing backend strings.
#[derive(Debug, serde::Serialize)]
pub struct ElybySkinsFailure {
    pub kind: crate::authlib_injector::PrepareErrorKind,
    pub detail: String,
}

/// `cmd_launch_game` keeps returning plain strings for ordinary failures, but
/// an Ely.by preparation failure is a structured object so the renderer can
/// reopen the retry/bypass/cancel dialog even when the second preparation
/// (inside the launch command) fails.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub enum LaunchCommandError {
    Elyby { elyby_skins: ElybySkinsFailure },
    Message(String),
}

impl From<String> for LaunchCommandError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ElybySkinsPrepareResult {
    NotApplicable,
    Unsupported {
        min_version: String,
    },
    Ready {
        version: String,
    },
    Failed {
        kind: crate::authlib_injector::PrepareErrorKind,
        detail: String,
    },
}

/// Prepare Ely.by skins before the launch command so a failure can be shown
/// as a localized dialog with explicit retry/bypass/cancel choices. The
/// actual launch still re-verifies the agent; this command only warms and
/// validates the same cache.
#[tauri::command]
pub async fn cmd_prepare_elyby_skins(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
) -> Result<ElybySkinsPrepareResult, String> {
    validate_instance_name(&instance_name)?;
    let config = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.clone()
    };
    let Some(account) = active_account(&config.data_dir) else {
        return Ok(ElybySkinsPrepareResult::NotApplicable);
    };
    let is_elyby = matches!(&account.account_type, accounts::AccountType::ElyBy);
    if !is_elyby || !config.elyby_skins_in_game {
        return Ok(ElybySkinsPrepareResult::NotApplicable);
    }
    let instance = instances::get_instance(&config.instances_dir(), &instance_name)
        .map_err(|e| e.to_string())?;
    if !crate::authlib_injector::is_supported_mc_version(&instance.mc_version) {
        return Ok(ElybySkinsPrepareResult::Unsupported {
            min_version: crate::authlib_injector::MIN_MC_VERSION.to_string(),
        });
    }
    match crate::authlib_injector::prepare(&config.data_dir).await {
        Ok(_) => Ok(ElybySkinsPrepareResult::Ready {
            version: crate::authlib_injector::VERSION.to_string(),
        }),
        Err(e) => {
            events::emit_log(
                &app,
                "warn",
                "launch",
                &format!(
                    "Ely.by Skin Integration preflight failed ({:?}): {}",
                    e.kind, e.detail
                ),
            );
            Ok(ElybySkinsPrepareResult::Failed {
                kind: e.kind,
                detail: e.detail,
            })
        }
    }
}

/// Launch-time Fabric integrity preparation.
///
/// This is the exact path used by `cmd_launch_game` before the game process is
/// even considered. It verifies/re-downloads every Fabric loader library,
/// enriches legacy profiles with repository checksums, and persists the
/// upgraded profile so later launches need no checksum lookup. Returns an
/// optional save warning without aborting the launch (the files were already
/// verified in memory); verification failures are returned as errors.
async fn prepare_fabric_libraries_for_launch(
    config: &crate::config::AppConfig,
    instance: &mut instances::Instance,
) -> std::result::Result<Option<String>, String> {
    let verified = match instance.loader_profile.as_mut() {
        Some(profile) => {
            modloaders::fabric::ensure_libraries(profile, &config.libraries_dir()).await
        }
        None => Ok(false),
    };
    match verified {
        Ok(true) => {
            // A legacy profile gained repository checksums; persist them so
            // the lookup and verification happen once, not every launch.
            match instances::save_instance(&config.instances_dir(), instance, None) {
                Ok(()) => Ok(None),
                Err(e) => Ok(Some(format!(
                    "Fabric loader libraries were verified, but the upgraded checksum metadata could not be saved: {}",
                    e
                ))),
            }
        }
        Ok(false) => Ok(None),
        Err(e) => Err(format!("Failed to repair Fabric loader libraries: {}", e)),
    }
}

/// Launch-time Forge/NeoForge integrity preparation.
///
/// Uses the same typed metadata/repository checksum pipeline as install,
/// Verify and Repair. A profile enriched with checksums is persisted before
/// the game is launched; verification failure aborts the launch.
async fn prepare_maven_loader_libraries_for_launch(
    config: &crate::config::AppConfig,
    instance: &mut instances::Instance,
) -> std::result::Result<Option<String>, String> {
    let (profile, hosts, label): (&mut modloaders::LoaderProfile, &[&str], &str) =
        match instance.loader {
            instances::LoaderType::Forge => (
                instance
                    .loader_profile
                    .as_mut()
                    .ok_or_else(|| "Forge loader profile is missing".to_string())?,
                modloaders::artifact::FORGE_HOSTS,
                "Forge",
            ),
            instances::LoaderType::NeoForge => (
                instance
                    .loader_profile
                    .as_mut()
                    .ok_or_else(|| "NeoForge loader profile is missing".to_string())?,
                modloaders::artifact::NEOFORGE_HOSTS,
                "NeoForge",
            ),
            _ => return Err("Not a Maven-backed loader".to_string()),
        };

    match modloaders::artifact::ensure_libraries(profile, &config.libraries_dir(), hosts, label).await
    {
        Ok(true) => match instances::save_instance(&config.instances_dir(), instance, None) {
            Ok(()) => Ok(None),
            Err(e) => Ok(Some(format!(
                "{} loader libraries were verified, but the upgraded checksum metadata could not be saved: {}",
                label, e
            ))),
        },
        Ok(false) => Ok(None),
        Err(e) => Err(format!("Failed to repair {} loader libraries: {}", label, e)),
    }
}

#[tauri::command]
pub async fn cmd_launch_game(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_name: String,
    server_address: Option<String>,
    server_port: Option<u16>,
    skip_elyby_skins: Option<bool>,
) -> Result<(), LaunchCommandError> {
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
        return Err(msg.into());
    }

    // Guard against launching the same instance twice concurrently — a
    // second click on a running instance must be a no-op, not a second copy.
    // The instance enters `Starting` NOW (before the long download/prepare
    // phase) and is released automatically if this launch fails before the
    // game starts; on success it becomes `Running` until the process exits
    // (see the exit handler). The same gate also refuses an instance whose
    // persistent readiness marker is still present (interrupted import or an
    // import that is preparing in this process).
    let mut reservation = match acquire_launch_gate(state.inner(), &instance_name) {
        Ok(r) => r,
        Err(msg) => {
            events::emit_log(&app, "warn", "launch", &msg);
            return Err(msg.into());
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
    let Some(account) = active_account(&data_dir) else {
        let msg =
            "No account available to launch the game. Please add an account first.".to_string();
        events::emit_log(&app, "error", "launch", &msg);
        return Err(msg.into());
    };

    let is_elyby = matches!(&account.account_type, accounts::AccountType::ElyBy);
    let (access_token, uuid, username, user_type) = match account.account_type {
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
            (
                String::new(),
                uuid_val,
                account.name.clone(),
                "legacy".to_string(),
            )
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
                "mojang".to_string(),
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
                            "msa".to_string(),
                        )
                    } else {
                        let msg = format!(
                            "Stored Microsoft session for '{}' is incomplete. Please sign in again.",
                            account.name
                        );
                        events::emit_log(&app, "error", "launch", &msg);
                        return Err(msg.into());
                    }
                }
                Err(e) => {
                    events::emit_log(&app, "error", "launch", &e);
                    return Err(e.into());
                }
            }
        }
    };

    let mut instance = match instances::get_instance(&config.instances_dir(), &instance_name) {
        Ok(i) => i,
        Err(e) => {
            let msg = format!("Failed to load instance '{}': {}", instance_name, e);
            events::emit_log(&app, "error", "launch", &msg);
            return Err(e.to_string().into());
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
        return Err(msg.into());
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
                return Err(msg.into());
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
                return Err(msg.into());
            }
        };
        instance.loader_profile = Some(profile);
        if let Err(e) = instances::save_instance(&config.instances_dir(), &instance, None) {
            let msg = format!("Failed to save installed {} loader: {}", loader_str, e);
            events::emit_log(&app, "error", "launch", &msg);
            return Err(msg.into());
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

    // A fully installed instance must launch offline. Prefer the version JSON
    // written by cmd_install_version; only fall back to Mojang when it is
    // missing or unreadable/corrupt.
    let version_info = if let Some(local) =
        load_local_version_info(&config.versions_dir(), &instance.mc_version)
    {
        events::emit_log(
            &app,
            "info",
            "launch",
            &format!(
                "Using installed version info for {} (offline-capable)",
                instance.mc_version
            ),
        );
        local
    } else {
        events::emit_log(&app, "info", "launch", "Fetching version manifest...");
        let manifest = match versions::fetch_version_manifest().await {
            Ok(m) => m,
            Err(e) => {
                let msg = format!("Failed to fetch version manifest: {}", e);
                events::emit_log(&app, "error", "launch", &msg);
                return Err(msg.into());
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
        match versions::fetch_version_info(&version_url).await {
            Ok(v) => v,
            Err(e) => {
                let msg = format!(
                    "Failed to fetch version info for {}: {}",
                    instance.mc_version, e
                );
                events::emit_log(&app, "error", "launch", &msg);
                return Err(msg.into());
            }
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

    // Ely.by in-game skins: prepare the pinned Java agent only for the active
    // Ely.by account. The agent is optional for every other account type and
    // must never change their launch command. A failure here is fatal instead
    // of silently launching with a feature the user asked for.
    let skip_elyby_skins = skip_elyby_skins.unwrap_or(false);
    let authlib_injector = if should_bypass_elyby_skins(
        is_elyby,
        config.elyby_skins_in_game,
        skip_elyby_skins,
    ) {
        events::emit_log(
            &app,
            "warn",
            "launch",
            "Ely.by Skin Integration skipped for this launch at the user's request.",
        );
        events::emit_launch_warning(&app, "elyby_skins_bypassed", None);
        None
    } else if crate::authlib_injector::should_attach(
        is_elyby,
        config.elyby_skins_in_game,
        &instance.mc_version,
    ) {
        events::emit_log(
            &app,
            "info",
            "launch",
            "Preparing Ely.by Skin Integration (authlib-injector)...",
        );
        match crate::authlib_injector::prepare(&config.data_dir).await {
            Ok(agent) => {
                events::emit_log(
                    &app,
                    "info",
                    "launch",
                    &format!(
                        "Ely.by Skin Integration: authlib-injector {} verified (build {}).",
                        crate::authlib_injector::VERSION,
                        crate::authlib_injector::BUILD_NUMBER
                    ),
                );
                Some(agent)
            }
            Err(e) => {
                events::emit_log(
                    &app,
                    "error",
                    "launch",
                    &format!(
                        "Ely.by Skin Integration could not be prepared ({:?}): {}",
                        e.kind, e.detail
                    ),
                );
                return Err(LaunchCommandError::Elyby {
                    elyby_skins: ElybySkinsFailure {
                        kind: e.kind,
                        detail: e.detail,
                    },
                });
            }
        }
    } else {
        if is_elyby && config.elyby_skins_in_game {
            events::emit_log(
                &app,
                "warn",
                "launch",
                &format!(
                    "Ely.by Skin Integration requires Minecraft {} or newer; launching without in-game Ely.by skins.",
                    crate::authlib_injector::MIN_MC_VERSION
                ),
            );
            events::emit_launch_warning(
                &app,
                "elyby_skins_unsupported_mc",
                Some(crate::authlib_injector::MIN_MC_VERSION),
            );
        }
        None
    };

    // Ensure a usable Java installation exists (download + verify through the
    // unified policy if missing). The returned installation — probed by M3.1,
    // so its architecture is known — is the ONE JVM for this run: the spawn
    // step below receives it verbatim and must not pick its own. The probe's
    // architecture also decides the native selection everywhere below, which
    // is why this runs BEFORE any version-file download.
    let prepared_java =
        ensure_java_for_launch(&app, &data_dir, &version_info, &instance, &config).await?;
    let launch_target = crate::versions::TargetArch::from_java_arch(prepared_java.arch);

    // The actual JVM is known now. Resolve the same preset/heap the launch
    // profile will use and surface a visible warning if ZGC cannot apply.
    // This is intentionally before spawning so the user sees G1GC, not a
    // silently substituted preset.
    let requested_gc = GcPreset::from_str(instance.effective_gc_preset(&config.default_gc_preset));
    let total_ram_mb = crate::config::detect_total_ram_mb();
    let effective_memory_mb =
        instance.effective_memory_mb(config.default_memory_mb, config.max_memory_mb, total_ram_mb);
    let gc_decision = jvm::decide_gc(
        requested_gc,
        effective_memory_mb,
        prepared_java.major_version,
    );
    if let Some(reason) = gc_decision.fallback {
        let detail = match reason {
            GcFallbackReason::JavaUnknown => "the Java version could not be determined".to_string(),
            GcFallbackReason::JavaTooOld { java_major } => {
                format!("Java {} is older than Java 17", java_major)
            }
            GcFallbackReason::HeapTooSmall { memory_mb } => {
                format!("the heap is {} MB, below the 8 GB floor", memory_mb)
            }
        };
        events::emit_log(
            &app,
            "warn",
            "launch",
            &format!(
                "ZGC requested for '{}', but {}; using G1GC.",
                instance_name, detail
            ),
        );
        events::emit_gc_launch_warning(
            &app,
            reason.kind(),
            prepared_java.major_version,
            effective_memory_mb,
        );
    }

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
        // `collect_downloads_for` already drops valid files. Maven fallback
        // artifacts without checksum/size are kept when their integrity
        // manifest is missing/stale, so do not filter them out by existence.
        .filter(|(_, path, sha1, size)| !path.exists() || (sha1.is_empty() && *size == 0))
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

    // Verify the asset index and every object it references before building
    // the command. "index.json exists" is not proof that the assets exist:
    // a corrupt index is restored online, and a missing/damaged object is
    // re-downloaded. Offline, this stops with an actionable error instead of
    // launching a game that will fail later.
    let asset_index = ensure_asset_index_for_launch(&app, &config, &version_info).await?;
    ensure_assets_for_launch(&app, &config, &version_info, &asset_index).await?;

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
                    return Err(msg.into());
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
                    return Err(msg.into());
                }
            }
        }
        _ => {}
    }

    // Self-heal loader libraries: if one was deleted or quarantined after
    // installation, download it now instead of silently dropping it from the
    // classpath (which surfaces later as NoClassDefFoundError).
    if instance.loader == instances::LoaderType::Fabric {
        // Fabric libraries have typed checksums persisted in the profile
        // (metadata SHA-1/256/512 or an explicit same-origin repository
        // checksum). Never fall back to "file exists" for this loader.
        match prepare_fabric_libraries_for_launch(&config, &mut instance).await {
            Ok(Some(warning)) => events::emit_log(&app, "warn", "launch", &warning),
            Ok(None) => {}
            Err(msg) => {
                events::emit_log(&app, "error", "launch", &msg);
                return Err(msg.into());
            }
        }
    } else if matches!(
        instance.loader,
        instances::LoaderType::Forge | instances::LoaderType::NeoForge
    ) {
        // Forge/NeoForge libraries are verified through their trusted
        // installer metadata (or an official Maven sidecar for legacy
        // profiles), never through the Fabric-specific path.
        match prepare_maven_loader_libraries_for_launch(&config, &mut instance).await {
            Ok(Some(warning)) => events::emit_log(&app, "warn", "launch", &warning),
            Ok(None) => {}
            Err(msg) => {
                events::emit_log(&app, "error", "launch", &msg);
                return Err(msg.into());
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
        return Err(msg.into());
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
        let launch_user_type = user_type.clone();
        let launch_authlib_injector = authlib_injector.clone();
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
                    user_type: &launch_user_type,
                    authlib_injector: launch_authlib_injector.as_ref(),
                },
                launch_server.as_deref().map(|h| (h, server_port)),
                launch_session_log.as_deref(),
                Some(&launch_java),
                total_ram_mb,
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
        return Err(msg.into());
    }
    let child_handle: Arc<std::sync::Mutex<Option<std::process::Child>>> =
        Arc::new(std::sync::Mutex::new(Some(child)));

    // Start the playtime session BEFORE releasing the reservation and before
    // the "close on launch" window close: the app-exit handler flushes active
    // sessions, and the exit watcher finalizes this one by pid. If the
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
                        let outcome = stop_unpersisted_child(&mut c, process_identity.as_ref());
                        if matches!(outcome, StopOutcome::StillRunning(_)) {
                            let msg = unpersisted_launch_error(
                                &instance_name,
                                pid,
                                "failed to start playtime session",
                                &outcome,
                            );
                            events::emit_log(&app, "error", "launch", &msg);
                            // Keep the pid record: the game is still running
                            // and must remain discoverable/recoverable.
                            if let Some(p) = session_log.as_deref() {
                                game_logs::clear_current_log_path_if(p);
                            }
                            return Err(msg.into());
                        }
                    }
                }
                crate::running_state::remove_pid_file(&data_dir, &instance_name);
                // The reservation is still armed: returning drops it and
                // releases the `Starting` entry automatically.
                if let Some(p) = session_log.as_deref() {
                    game_logs::clear_current_log_path_if(p);
                }
                return Err(format!("Failed to start playtime session: {}", e).into());
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

    // The window action (do nothing / minimize / close) is performed by the
    // frontend's `game_started` handler using the saved preference. It only
    // runs after this event, i.e. after the process is confirmed running.

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
        // The game is gone: a host room must stop advertising this game's LAN
        // port immediately, even if the Rooms page is closed. Only the exact
        // registered instance may clear it.
        {
            let app_state = app_clone.state::<AppState>();
            match app_state
                .room_state
                .clear_minecraft_port_if_host_instance(&instance_clone)
            {
                Ok(true) => {
                    let _ = app_clone.emit("room_status_changed", serde_json::json!({}));
                }
                Ok(false) => {}
                Err(e) => {
                    events::emit_log(
                        &app_clone,
                        "warn",
                        "rooms",
                        &format!("Failed to clear the room LAN port after game exit: {}", e),
                    );
                }
            }
        }
        // Emit the completion before releasing the registry entry. Otherwise
        // a new launch can acquire the slot and emit `game_started` first,
        // and the older completion would remove the new run from the UI.
        let _ = app_clone.emit(
            "launch_complete",
            events::LaunchEventPayload {
                instance_id: instance_clone.clone(),
                status: "exited".into(),
                pid: Some(pid_for_exit),
                exit_code: Some(exit_code),
            },
        );
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

/// State shared by every loader-install command. The launch-registry guard and
/// the instance-name reservation are held for the whole command, so a rename,
/// delete or launch cannot invalidate the instance between reading its config
/// and saving the installed profile.
struct LoaderInstallContext<'a> {
    _mutation: crate::commands::instances::InstanceMutationGuard<'a>,
    _name: instances::InstanceNameReservation,
    instances_dir: std::path::PathBuf,
    libraries_dir: std::path::PathBuf,
    versions_dir: std::path::PathBuf,
    instance: instances::Instance,
}

impl<'a> LoaderInstallContext<'a> {
    fn acquire(state: &'a AppState, instance_name: &str) -> Result<Self, String> {
        // No config lock may be held here: the mutation guard reads data_dir
        // through the same mutex.
        let mutation =
            crate::commands::instances::InstanceMutationGuard::acquire(state, instance_name)?;
        let (instances_dir, libraries_dir, versions_dir) = {
            let config = state.config.lock().map_err(|e| e.to_string())?;
            (
                config.instances_dir(),
                config.libraries_dir(),
                config.versions_dir(),
            )
        };
        let name = instances::InstanceNameReservation::acquire(&instances_dir, &[instance_name])
            .map_err(|e| e.to_string())?;
        let instance =
            instances::get_instance(&instances_dir, instance_name).map_err(|e| e.to_string())?;
        Ok(Self {
            _mutation: mutation,
            _name: name,
            instances_dir,
            libraries_dir,
            versions_dir,
            instance,
        })
    }

    /// Save metadata of the instance this operation started with. The
    /// reservation is already held, so the non-reserving variant must be used:
    /// acquiring it again would deadlock the name registry.
    fn save(&self, instance: &instances::Instance) -> Result<(), String> {
        instances::save_instance_existing_reserved(&self.instances_dir, instance, None)
            .map_err(|e| e.to_string())
    }
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
    let ctx = LoaderInstallContext::acquire(state.inner(), &instance_name)?;
    ensure_loader_mc_match(lang, "Fabric", &ctx.instance, &mc_version)?;
    let profile = modloaders::fabric::install(&mc_version, &loader_version, &ctx.libraries_dir)
        .await
        .map_err(|e| {
            i18n::tr(
                lang,
                "loader_install_failed",
                &[("loader", "Fabric"), ("error", &e.to_string())],
            )
        })?;

    let mut instance = ctx.instance.clone();
    instance.loader = instances::LoaderType::Fabric;
    instance.loader_version = Some(loader_version.clone());
    instance.loader_profile = Some(profile);
    ctx.save(&instance)?;
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
    let ctx = LoaderInstallContext::acquire(state.inner(), &instance_name)?;
    ensure_loader_mc_match(lang, "Forge", &ctx.instance, &mc_version)?;
    let profile = modloaders::forge::install(
        &mc_version,
        &loader_version,
        &ctx.libraries_dir,
        &ctx.versions_dir,
    )
    .await
    .map_err(|e| {
        i18n::tr(
            lang,
            "loader_install_failed",
            &[("loader", "Forge"), ("error", &e.to_string())],
        )
    })?;

    let mut instance = ctx.instance.clone();
    instance.loader = instances::LoaderType::Forge;
    instance.loader_version = Some(loader_version.clone());
    instance.loader_profile = Some(profile);
    ctx.save(&instance)?;
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
    let ctx = LoaderInstallContext::acquire(state.inner(), &instance_name)?;
    ensure_loader_mc_match(lang, "NeoForge", &ctx.instance, &mc_version)?;
    let profile = modloaders::neoforge::install(
        &mc_version,
        &loader_version,
        &ctx.libraries_dir,
        &ctx.versions_dir,
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

    let mut instance = ctx.instance.clone();
    instance.loader = instances::LoaderType::NeoForge;
    instance.loader_version = Some(loader_version.clone());
    instance.loader_profile = Some(profile);
    ctx.save(&instance)?;
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

    let ctx = LoaderInstallContext::acquire(state.inner(), &instance_name)?;
    let loader_type = match ctx.instance.loader {
        instances::LoaderType::Fabric => "Fabric",
        instances::LoaderType::Forge => "Forge",
        instances::LoaderType::NeoForge => "NeoForge",
        instances::LoaderType::Vanilla => {
            return Err(i18n::tr(lang, "instance_no_loader", &[]));
        }
    };
    let loader_version = ctx
        .instance
        .loader_version
        .clone()
        .ok_or_else(|| i18n::tr(lang, "instance_no_loader_version", &[]))?;
    let mc_version = ctx.instance.mc_version.clone();

    let installing_msg = i18n::tr(
        lang,
        "installing_loader",
        &[
            ("loader", loader_type),
            ("version", &loader_version),
            ("mc", &mc_version),
        ],
    );
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({ "stage": "start", "message": installing_msg }),
    );
    let downloading_msg = i18n::tr(lang, "downloading_loader_libs", &[("loader", loader_type)]);
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({ "stage": "download", "message": downloading_msg }),
    );

    let profile = modloaders::install_loader(
        loader_type,
        &mc_version,
        &loader_version,
        &ctx.libraries_dir,
        &ctx.versions_dir,
        Some(&app),
    )
    .await
    .map_err(|e| {
        let msg = i18n::tr(
            lang,
            "loader_install_failed",
            &[("loader", loader_type), ("error", &e.to_string())],
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

    // Save profile to the instance this operation started with. The name
    // reservation and launch guard held by `ctx` make the path stable.
    let mut instance = ctx.instance.clone();
    instance.loader_profile = Some(profile);
    ctx.save(&instance)?;

    let msg_done = i18n::tr(lang, "loader_install_success", &[("loader", loader_type)]);
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

    let ctx = LoaderInstallContext::acquire(state.inner(), &instance_name)?;
    let loader_type = match ctx.instance.loader {
        instances::LoaderType::Fabric => "Fabric",
        instances::LoaderType::Forge => "Forge",
        instances::LoaderType::NeoForge => "NeoForge",
        instances::LoaderType::Vanilla => {
            return Err(i18n::tr(lang, "instance_no_loader", &[]));
        }
    };
    let mc_version = ctx.instance.mc_version.clone();
    let old_version = ctx.instance.loader_version.clone().unwrap_or_default();

    let installing_msg = i18n::tr(
        lang,
        "installing_loader",
        &[
            ("loader", loader_type),
            ("version", &loader_version),
            ("mc", &mc_version),
        ],
    );
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({ "stage": "start", "message": installing_msg }),
    );
    let downloading_msg = i18n::tr(lang, "downloading_loader_libs", &[("loader", loader_type)]);
    let _ = app.emit(
        "loader-install-progress",
        serde_json::json!({ "stage": "download", "message": downloading_msg }),
    );

    let profile = modloaders::install_loader(
        loader_type,
        &mc_version,
        &loader_version,
        &ctx.libraries_dir,
        &ctx.versions_dir,
        Some(&app),
    )
    .await
    .map_err(|e| {
        let msg = i18n::tr(
            lang,
            "loader_install_failed",
            &[("loader", loader_type), ("error", &e.to_string())],
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

    // Install succeeded: record the new version and profile together on the
    // instance that was reserved for this operation.
    let mut instance = ctx.instance.clone();
    instance.loader_version = Some(loader_version.clone());
    instance.loader_profile = Some(profile);
    ctx.save(&instance)?;

    let msg_done = i18n::tr(lang, "loader_install_success", &[("loader", loader_type)]);
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
            // `GameLogSession::instance_name` is the real header name when
            // available and the sanitized filename stem otherwise. Sanitize
            // both sides so instances with spaces/brackets match too.
            sessions.retain(|s| game_logs::sanitize_instance_name(&s.instance_name) == wanted);
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
pub async fn cmd_read_game_log_incremental(
    state: State<'_, AppState>,
    path: String,
    byte_offset: u64,
    initial_max_lines: Option<usize>,
    file_token: Option<String>,
) -> Result<game_logs::IncrementalLogChunk, String> {
    let data_dir = {
        let c = state.config.lock().map_err(|e| e.to_string())?;
        c.data_dir.clone()
    };
    tauri::async_runtime::spawn_blocking(move || {
        let safe_path = game_logs::validate_log_path(&data_dir, &path)?;
        game_logs::read_game_log_incremental(
            &safe_path,
            byte_offset,
            initial_max_lines,
            file_token.as_deref(),
        )
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
        // Emit before clearing the registry so a completion can never remove
        // a newer run that starts immediately after this one is released.
        let _ = app.emit(
            "launch_complete",
            events::LaunchEventPayload {
                instance_id: instance_name.clone(),
                status: "exited".into(),
                pid: Some(identity.pid),
                exit_code: None,
            },
        );
        crate::running_state::remove_pid_file(&data_dir, &instance_name);
        state.launches.clear_running(&instance_name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::tests::{
        direct_proxy_lock, mock_dir, sha512_hex, spawn_mock, MockServer, MockSpec,
    };
    use crate::instances::{Instance, LoaderType};
    use crate::launch_state::{LaunchPhase, LaunchRegistry};
    use crate::modloaders::{LoaderLibrary, LoaderProfile};
    use std::sync::atomic::Ordering;

    const LEGACY_ASM_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

    fn launch_config(data_dir: &Path) -> crate::config::AppConfig {
        crate::config::AppConfig {
            data_dir: data_dir.to_path_buf(),
            ..Default::default()
        }
    }

    fn mock_base(server: &MockServer) -> String {
        server.url.trim_end_matches("artifact.bin").to_string()
    }

    fn legacy_fabric_instance(name: &str, base: &str, loader_path: &str) -> Instance {
        let mut instance = Instance::new(
            name,
            "1.20.1",
            LoaderType::Fabric,
            Some("0.19.5".to_string()),
        );
        instance.loader_profile = Some(LoaderProfile {
            main_class: "net.fabricmc.loader.impl.launch.knot.KnotClient".to_string(),
            libraries: vec![
                LoaderLibrary {
                    name: "net.fabricmc:fabric-loader:0.19.5".to_string(),
                    url: base.to_string(),
                    path: loader_path.to_string(),
                    sha1: None,
                    sha256: None,
                    sha512: None,
                    checksum_source: None,
                    size: None,
                },
                LoaderLibrary {
                    name: "org.ow2.asm:asm:9.6".to_string(),
                    url: "https://maven.fabricmc.net/".to_string(),
                    path: "org/ow2/asm/asm/9.6/asm-9.6.jar".to_string(),
                    sha1: Some(LEGACY_ASM_SHA1.to_string()),
                    sha256: None,
                    sha512: None,
                    checksum_source: None,
                    size: Some(3),
                },
            ],
            jvm_args: Vec::new(),
            game_args: Vec::new(),
            legacy_args: false,
        });
        instance
    }

    fn legacy_maven_loader_instance(
        name: &str,
        loader: LoaderType,
        base: &str,
        loader_path: &str,
    ) -> Instance {
        let version = match loader {
            LoaderType::Forge => "47.4.28",
            LoaderType::NeoForge => "47.1.105",
            _ => "1.0",
        };
        let mut instance = Instance::new(name, "1.20.1", loader, Some(version.to_string()));
        instance.loader_profile = Some(LoaderProfile {
            main_class: "test.Main".to_string(),
            libraries: vec![LoaderLibrary {
                name: "example:loader:1.0".to_string(),
                url: format!("{base}{loader_path}"),
                path: loader_path.to_string(),
                sha1: None,
                sha256: None,
                sha512: None,
                checksum_source: None,
                size: None,
            }],
            jvm_args: Vec::new(),
            game_args: Vec::new(),
            legacy_args: false,
        });
        instance
    }

    fn seed_launch_instance(
        config: &crate::config::AppConfig,
        instance: &Instance,
    ) -> std::path::PathBuf {
        instances::create_instance(&config.instances_dir(), instance, &LaunchRegistry::new())
            .expect("create test instance");
        let asm = config
            .libraries_dir()
            .join("org/ow2/asm/asm/9.6/asm-9.6.jar");
        std::fs::create_dir_all(asm.parent().unwrap()).unwrap();
        std::fs::write(&asm, b"abc").unwrap();
        config
            .instances_dir()
            .join(&instance.name)
            .join("instance.json")
    }

    fn seed_loader_file(config: &crate::config::AppConfig, path: &str, bytes: &[u8]) {
        let dest = config.libraries_dir().join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, bytes).unwrap();
    }

    fn assert_no_part_files(path: &Path) {
        for suffix in [".part", ".part.meta"] {
            let mut value = path.as_os_str().to_os_string();
            value.push(suffix);
            assert!(
                !std::path::PathBuf::from(value).exists(),
                "temporary file for {} must not remain",
                path.display()
            );
        }
    }

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

    #[tokio::test]
    async fn launch_preparation_migrates_legacy_fabric_profile_and_reuses_it() {
        let _proxy = direct_proxy_lock().await;
        let body = b"fabric-loader-artifact";
        let expected = sha512_hex(body);
        let server = spawn_mock(vec![MockSpec::ok(expected.as_bytes()), MockSpec::ok(body)]).await;
        let data_dir = mock_dir("launch_fabric_migration");
        let config = launch_config(&data_dir);
        let loader_path = "net/fabricmc/fabric-loader/0.19.5/fabric-loader-0.19.5.jar";
        let mut instance =
            legacy_fabric_instance("Legacy Fabric", &mock_base(&server), loader_path);
        let json_path = seed_launch_instance(&config, &instance);
        seed_loader_file(&config, loader_path, b"old-placeholder");

        // Scenario A: the real launch-time preparation path migrates, verifies
        // and persists the profile before any game process is considered.
        let warning = prepare_fabric_libraries_for_launch(&config, &mut instance)
            .await
            .expect("online preparation");
        assert!(warning.is_none());
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);

        let first_bytes = std::fs::read(&json_path).unwrap();
        let saved: Instance = serde_json::from_slice(&first_bytes).unwrap();
        let saved_libs = &saved.loader_profile.as_ref().unwrap().libraries;
        assert_eq!(saved_libs[0].sha512.as_deref(), Some(expected.as_str()));
        assert_eq!(saved_libs[0].checksum_source.as_deref(), Some("repository"));
        assert_eq!(saved_libs[0].sha1, None);
        assert_eq!(saved_libs[1].sha1.as_deref(), Some(LEGACY_ASM_SHA1));
        assert_eq!(saved_libs[1].size, Some(3));
        assert_eq!(saved.mc_version, "1.20.1");
        assert_eq!(saved.loader_version.as_deref(), Some("0.19.5"));
        assert_eq!(
            std::fs::read(config.libraries_dir().join(loader_path)).unwrap(),
            body
        );
        assert_no_part_files(&config.libraries_dir().join(loader_path));

        // Scenario B: reload from disk, then run preparation again. The saved
        // checksum must be used and no metadata/checksum request may be made.
        let mut reloaded =
            instances::get_instance(&config.instances_dir(), "Legacy Fabric").unwrap();
        let warning = prepare_fabric_libraries_for_launch(&config, &mut reloaded)
            .await
            .expect("second local preparation");
        assert!(warning.is_none());
        assert_eq!(
            server.hits.load(Ordering::SeqCst),
            2,
            "second preparation must not touch the network"
        );
        assert_eq!(
            std::fs::read(&json_path).unwrap(),
            first_bytes,
            "an already-valid profile must not be rewritten"
        );
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn launch_preparation_offline_failure_preserves_files_profile_and_neighbors() {
        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![
            MockSpec::status(404),
            MockSpec::status(404),
            MockSpec::status(404),
        ])
        .await;
        let data_dir = mock_dir("launch_fabric_offline");
        let config = launch_config(&data_dir);
        let loader_path = "net/fabricmc/fabric-loader/0.19.5/fabric-loader-0.19.5.jar";
        let mut instance =
            legacy_fabric_instance("Legacy Fabric", &mock_base(&server), loader_path);
        let json_path = seed_launch_instance(&config, &instance);
        seed_loader_file(&config, loader_path, b"working-loader");
        let before_json = std::fs::read(&json_path).unwrap();
        let before_lib = std::fs::read(config.libraries_dir().join(loader_path)).unwrap();

        let neighbor = Instance::new("Neighbor", "1.20.1", LoaderType::Vanilla, None);
        let neighbor_json = seed_launch_instance(&config, &neighbor);
        let before_neighbor = std::fs::read(&neighbor_json).unwrap();

        let err = prepare_fabric_libraries_for_launch(&config, &mut instance)
            .await
            .expect_err("offline checksum lookup must fail");
        assert!(err.contains("no repository checksum"), "got: {err}");
        assert_eq!(
            std::fs::read(config.libraries_dir().join(loader_path)).unwrap(),
            before_lib
        );
        assert_eq!(std::fs::read(&json_path).unwrap(), before_json);
        assert_eq!(std::fs::read(&neighbor_json).unwrap(), before_neighbor);
        let saved: Instance = serde_json::from_slice(&before_json).unwrap();
        let saved_lib = &saved.loader_profile.as_ref().unwrap().libraries[0];
        assert!(saved_lib.sha1.is_none());
        assert!(saved_lib.sha256.is_none());
        assert!(saved_lib.sha512.is_none());
        assert!(saved_lib.checksum_source.is_none());
        assert_no_part_files(&config.libraries_dir().join(loader_path));
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn launch_preparation_checksum_mismatch_rolls_back() {
        let _proxy = direct_proxy_lock().await;
        let wrong = "0".repeat(128);
        let hostile = b"hostile-artifact";
        let server = spawn_mock(vec![
            MockSpec::ok(wrong.as_bytes()),
            MockSpec::ok(hostile),
            MockSpec::ok(hostile),
            MockSpec::ok(hostile),
            MockSpec::ok(hostile),
            MockSpec::ok(hostile),
        ])
        .await;
        let data_dir = mock_dir("launch_fabric_mismatch");
        let config = launch_config(&data_dir);
        let loader_path = "net/fabricmc/fabric-loader/0.19.5/fabric-loader-0.19.5.jar";
        let mut instance =
            legacy_fabric_instance("Legacy Fabric", &mock_base(&server), loader_path);
        let json_path = seed_launch_instance(&config, &instance);
        seed_loader_file(&config, loader_path, b"working-loader");
        let before_json = std::fs::read(&json_path).unwrap();
        let before_lib = std::fs::read(config.libraries_dir().join(loader_path)).unwrap();

        let err = prepare_fabric_libraries_for_launch(&config, &mut instance)
            .await
            .expect_err("a wrong repository checksum must abort preparation");
        assert!(err.contains("SHA512 mismatch"), "got: {err}");
        assert_eq!(
            std::fs::read(config.libraries_dir().join(loader_path)).unwrap(),
            before_lib,
            "the working artifact must survive a failed verification"
        );
        assert_eq!(
            std::fs::read(&json_path).unwrap(),
            before_json,
            "a failed migration must not persist the bogus checksum"
        );
        let saved: Instance = serde_json::from_slice(&before_json).unwrap();
        let saved_lib = &saved.loader_profile.as_ref().unwrap().libraries[0];
        assert!(saved_lib.sha512.is_none());
        assert!(saved_lib.checksum_source.is_none());
        assert_no_part_files(&config.libraries_dir().join(loader_path));
        assert_eq!(server.hits.load(Ordering::SeqCst), 6);
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn launch_preparation_migrates_forge_and_neoforge_profiles() {
        let _proxy = direct_proxy_lock().await;
        for (loader, loader_path, name) in [
            (
                LoaderType::Forge,
                "net/minecraftforge/securejarhandler/2.1.10/securejarhandler-2.1.10.jar",
                "Legacy Forge",
            ),
            (
                LoaderType::NeoForge,
                "net/neoforged/fancymodloader/loader/47.2.2/loader-47.2.2.jar",
                "Legacy NeoForge",
            ),
        ] {
            let body = b"loader-artifact";
            let expected = sha512_hex(body);
            let server =
                spawn_mock(vec![MockSpec::ok(expected.as_bytes()), MockSpec::ok(body)]).await;
            let data_dir = mock_dir(&format!("launch_{loader:?}_migration"));
            let config = launch_config(&data_dir);
            let mut instance = legacy_maven_loader_instance(
                name,
                loader.clone(),
                &mock_base(&server),
                loader_path,
            );
            let json_path = seed_launch_instance(&config, &instance);
            seed_loader_file(&config, loader_path, b"old-placeholder");

            let warning = prepare_maven_loader_libraries_for_launch(&config, &mut instance)
                .await
                .expect("online Maven loader preparation");
            assert!(warning.is_none());
            assert_eq!(server.hits.load(Ordering::SeqCst), 2);

            let first_bytes = std::fs::read(&json_path).unwrap();
            let saved: Instance = serde_json::from_slice(&first_bytes).unwrap();
            let saved_lib = &saved.loader_profile.as_ref().unwrap().libraries[0];
            assert_eq!(saved_lib.sha512.as_deref(), Some(expected.as_str()));
            assert_eq!(saved_lib.checksum_source.as_deref(), Some("repository"));

            let mut reloaded = instances::get_instance(&config.instances_dir(), name).unwrap();
            let warning = prepare_maven_loader_libraries_for_launch(&config, &mut reloaded)
                .await
                .expect("second local Maven loader preparation");
            assert!(warning.is_none());
            assert_eq!(
                server.hits.load(Ordering::SeqCst),
                2,
                "{loader:?}: second preparation must not touch the network"
            );
            assert_eq!(std::fs::read(&json_path).unwrap(), first_bytes);
            assert_no_part_files(&config.libraries_dir().join(loader_path));
            let _ = std::fs::remove_dir_all(&data_dir);
        }
    }

    #[tokio::test]
    async fn launch_preparation_forge_offline_failure_preserves_files_and_profile() {
        let _proxy = direct_proxy_lock().await;
        for (loader, loader_path, name) in [
            (
                LoaderType::Forge,
                "net/minecraftforge/securejarhandler/2.1.10/securejarhandler-2.1.10.jar",
                "Offline Forge",
            ),
            (
                LoaderType::NeoForge,
                "net/neoforged/fancymodloader/loader/47.2.2/loader-47.2.2.jar",
                "Offline NeoForge",
            ),
        ] {
            let server = spawn_mock(vec![
                MockSpec::status(404),
                MockSpec::status(404),
                MockSpec::status(404),
            ])
            .await;
            let data_dir = mock_dir(&format!("launch_{loader:?}_offline"));
            let config = launch_config(&data_dir);
            let mut instance = legacy_maven_loader_instance(
                name,
                loader.clone(),
                &mock_base(&server),
                loader_path,
            );
            let json_path = seed_launch_instance(&config, &instance);
            seed_loader_file(&config, loader_path, b"working-loader");
            let before_json = std::fs::read(&json_path).unwrap();
            let before_lib = std::fs::read(config.libraries_dir().join(loader_path)).unwrap();

            let err = prepare_maven_loader_libraries_for_launch(&config, &mut instance)
                .await
                .expect_err("offline Maven loader preparation must fail");
            assert!(err.contains("No repository checksum"), "got: {err}");
            assert_eq!(
                std::fs::read(config.libraries_dir().join(loader_path)).unwrap(),
                before_lib
            );
            assert_eq!(std::fs::read(&json_path).unwrap(), before_json);
            let saved: Instance = serde_json::from_slice(&before_json).unwrap();
            let saved_lib = &saved.loader_profile.as_ref().unwrap().libraries[0];
            assert!(saved_lib.sha512.is_none());
            assert!(saved_lib.checksum_source.is_none());
            assert_no_part_files(&config.libraries_dir().join(loader_path));
            let _ = std::fs::remove_dir_all(&data_dir);
        }
    }

    #[test]
    fn skip_flag_only_bypasses_an_enabled_elyby_account() {
        assert!(should_bypass_elyby_skins(true, true, true));
        assert!(!should_bypass_elyby_skins(true, true, false));
        assert!(!should_bypass_elyby_skins(true, false, true));
        assert!(!should_bypass_elyby_skins(false, true, true));
        assert!(!should_bypass_elyby_skins(false, false, true));
    }

    #[test]
    fn launch_elyby_error_is_a_structured_object() {
        let error = LaunchCommandError::Elyby {
            elyby_skins: ElybySkinsFailure {
                kind: crate::authlib_injector::PrepareErrorKind::Checksum,
                detail: "bad hash".to_string(),
            },
        };
        let json = serde_json::to_value(&error).expect("serialize launch error");
        assert_eq!(json["elyby_skins"]["kind"], "checksum");
        assert_eq!(json["elyby_skins"]["detail"], "bad hash");

        let ordinary = LaunchCommandError::from("No account available".to_string());
        assert_eq!(
            serde_json::to_value(&ordinary).unwrap(),
            serde_json::Value::String("No account available".to_string())
        );
    }

    #[test]
    fn elyby_preflight_result_uses_stable_status_and_kind() {
        let failed = serde_json::to_value(ElybySkinsPrepareResult::Failed {
            kind: crate::authlib_injector::PrepareErrorKind::Checksum,
            detail: "bad hash".to_string(),
        })
        .expect("serialize failed preflight");
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["kind"], "checksum");
        assert_eq!(failed["detail"], "bad hash");

        let unsupported = serde_json::to_value(ElybySkinsPrepareResult::Unsupported {
            min_version: "1.7.2".to_string(),
        })
        .expect("serialize unsupported preflight");
        assert_eq!(unsupported["status"], "unsupported");
        assert_eq!(unsupported["min_version"], "1.7.2");
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

    #[test]
    fn local_version_json_is_preferred_and_corruption_falls_back() {
        let dir = std::env::temp_dir().join(format!(
            "vl-launch-offline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let versions_dir = dir.join("versions");
        let version_dir = versions_dir.join("1.20.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        let json_path = version_dir.join("1.20.1.json");

        let manifest = serde_json::json!({
            "id": "1.20.1",
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "libraries": [],
            "downloads": {
                "client": { "sha1": "", "size": 0, "url": "" }
            },
            "assetIndex": {
                "id": "5",
                "sha1": "",
                "size": 0,
                "totalSize": 0,
                "url": ""
            },
            "assets": "5"
        });
        std::fs::write(&json_path, serde_json::to_string(&manifest).unwrap()).unwrap();

        let loaded = load_local_version_info(&versions_dir, "1.20.1")
            .expect("installed version JSON must be used without network");
        assert_eq!(loaded.id, "1.20.1");

        // Corrupt or mismatched metadata must not be trusted: the caller
        // falls back to the network instead of launching with bad data.
        std::fs::write(&json_path, b"{not json").unwrap();
        assert!(load_local_version_info(&versions_dir, "1.20.1").is_none());

        let mut wrong_id = manifest.clone();
        wrong_id["id"] = serde_json::json!("1.21.1");
        std::fs::write(&json_path, serde_json::to_string(&wrong_id).unwrap()).unwrap();
        assert!(load_local_version_info(&versions_dir, "1.20.1").is_none());

        // A tampered instance.json cannot escape versions/.
        assert!(load_local_version_info(&versions_dir, "../../evil").is_none());
        assert!(load_local_version_info(&versions_dir, "1.20.1-missing").is_none());

        let _ = std::fs::remove_dir_all(&dir);
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

    // ---- loader installation lifecycle -----------------------------------

    fn state_in(dir: &Path) -> AppState {
        let config = crate::AppConfig {
            data_dir: dir.to_path_buf(),
            ..crate::AppConfig::default()
        };
        AppState {
            config: std::sync::Mutex::new(config),
            auth_state: std::sync::Mutex::new(crate::auth::AuthState::default()),
            launches: LaunchRegistry::new(),
            pack_watcher: std::sync::Mutex::new(None),
            active_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            room_state: std::sync::Arc::new(crate::rooms::room_state::RoomStateManager::new(dir)),
            tailscale: crate::rooms::tailscale::TailscaleManager::new(dir.to_path_buf()),
        }
    }

    fn loader_test_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vl-loader-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn launch_gate_refuses_a_pending_instance_and_releases_the_slot() {
        let dir = loader_test_dir("launch-gate");
        let state = state_in(&dir);
        let instances_dir = state.config.lock().unwrap().instances_dir();
        crate::instances::create_instance(
            &instances_dir,
            &crate::instances::Instance::new(
                "Pack",
                "1.20.1",
                crate::instances::LoaderType::Vanilla,
                None,
            ),
            &state.launches,
        )
        .unwrap();

        crate::instance_readiness::write_pending(
            &instances_dir.join("Pack"),
            &crate::instance_readiness::PendingSetup {
                stage: crate::instance_readiness::SetupStage::Preparing,
                operation: "import".to_string(),
                started_at: "2026-11-14T00:00:00Z".to_string(),
                token: "gate-owner".to_string(),
            },
        )
        .unwrap();
        assert!(
            crate::instance_readiness::read_pending(&instances_dir.join("Pack")).is_some(),
            "test marker must be readable"
        );
        assert_eq!(
            state.config.lock().unwrap().instances_dir(),
            instances_dir,
            "the test and the gate must look at the same directory"
        );
        assert!(
            crate::instance_readiness::ensure_launch_ready(&instances_dir.join("Pack"), "Pack")
                .is_err(),
            "the readiness helper itself must refuse"
        );

        // This is the backend entry point used by cmd_launch_game: it must
        // refuse even when the UI is bypassed.
        let err = match acquire_launch_gate(&state, "Pack") {
            Ok(_) => panic!("a pending instance must not be launchable"),
            Err(e) => e,
        };
        assert!(err.contains("not ready"), "got: {err}");
        assert_eq!(
            state.launches.phase("Pack"),
            None,
            "a refused launch must not leave the instance reserved"
        );

        crate::instance_readiness::clear_pending(&instances_dir.join("Pack"), "gate-owner")
            .unwrap();
        let reservation = acquire_launch_gate(&state, "Pack").expect("ready instance launches");
        assert_eq!(
            state.launches.phase("Pack"),
            Some(crate::launch_state::LaunchPhase::Starting)
        );
        drop(reservation);
        assert_eq!(state.launches.phase("Pack"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loader_install_holds_the_instance_against_rename_and_delete() {
        let dir = loader_test_dir("hold");
        let state = state_in(&dir);
        let instances_dir = state.config.lock().unwrap().instances_dir();
        instances::create_instance(
            &instances_dir,
            &instances::Instance::new("Pack", "1.20.1", instances::LoaderType::Vanilla, None),
            &state.launches,
        )
        .unwrap();

        let ctx = LoaderInstallContext::acquire(&state, "Pack").expect("reserve loader install");

        let err = instances::delete_instance(&instances_dir, "Pack")
            .expect_err("delete must not race a loader install")
            .to_string();
        assert!(err.contains("operation is already"), "got: {err}");
        assert!(
            instances_dir.join("Pack").is_dir(),
            "the failed delete must leave the instance intact"
        );

        let renamed =
            instances::Instance::new("Renamed", "1.20.1", instances::LoaderType::Vanilla, None);
        let err = instances::save_instance(&instances_dir, &renamed, Some("Pack"))
            .expect_err("rename must not race a loader install")
            .to_string();
        assert!(err.contains("operation is already"), "got: {err}");
        assert!(instances_dir.join("Pack").is_dir());
        assert!(!instances_dir.join("Renamed").exists());

        // Releasing the guard (a failed install) makes the instance editable.
        drop(ctx);
        assert!(
            crate::commands::instances::InstanceMutationGuard::acquire(&state, "Pack").is_ok(),
            "a failed install must not leave the mutation guard held"
        );
        instances::delete_instance(&instances_dir, "Pack").expect("delete after release");
        assert!(!instances_dir.join("Pack").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_reservation_and_world_mutation_guard_are_mutually_exclusive() {
        let dir = loader_test_dir("world-launch-guard");
        let state = state_in(&dir);

        let reservation = LaunchReservation::acquire(&state, "Pack").expect("launch reservation");
        let err = match crate::commands::instances::InstanceMutationGuard::acquire(&state, "Pack") {
            Ok(_) => panic!("a world mutation must be refused while a launch is starting"),
            Err(e) => e,
        };
        assert!(err.contains("still starting"), "got: {err}");
        drop(reservation);

        let guard = crate::commands::instances::InstanceMutationGuard::acquire(&state, "Pack")
            .expect("a world mutation must be allowed after the launch slot is released");
        assert!(
            LaunchReservation::acquire(&state, "Pack").is_err(),
            "a launch must be refused while a world mutation owns the instance"
        );
        drop(guard);

        assert!(
            LaunchReservation::acquire(&state, "Pack").is_ok(),
            "the launch slot must be available again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loader_save_never_recreates_a_removed_instance() {
        let dir = loader_test_dir("no-recreate");
        let state = state_in(&dir);
        let instances_dir = state.config.lock().unwrap().instances_dir();
        instances::create_instance(
            &instances_dir,
            &instances::Instance::new("Pack", "1.20.1", instances::LoaderType::Vanilla, None),
            &state.launches,
        )
        .unwrap();

        let ctx = LoaderInstallContext::acquire(&state, "Pack").expect("reserve loader install");
        // Simulate a delete that happened outside this process (the in-process
        // name registry cannot see another launcher/process).
        std::fs::remove_dir_all(instances_dir.join("Pack")).unwrap();

        let err = ctx
            .save(&ctx.instance)
            .expect_err("saving must not resurrect a deleted instance");
        assert!(err.contains("no longer exists"), "got: {err}");
        assert!(
            !instances_dir.join("Pack").exists(),
            "a failed save must not recreate the directory"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loader_guard_and_launch_are_mutually_exclusive() {
        let dir = loader_test_dir("launch");
        let state = state_in(&dir);

        let guard = crate::commands::instances::InstanceMutationGuard::acquire(&state, "Pack")
            .expect("edit guard");
        assert_eq!(
            state.launches.begin_launch("Pack"),
            Err(crate::launch_state::LaunchBusy::Editing),
            "a launch must not start while a loader install holds the instance"
        );
        drop(guard);

        let launch = state.launches.begin_launch("Pack").expect("launch reserve");
        assert!(
            crate::commands::instances::InstanceMutationGuard::acquire(&state, "Pack").is_err(),
            "a loader install must not start while the game is Starting/Running"
        );
        state.launches.release("Pack", launch);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

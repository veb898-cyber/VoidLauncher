use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Hard bounds for any heap value the launcher will hand to the JVM.
/// The UI and the global config use the same bounds; the per-instance value
/// is additionally capped by the global `max_memory_mb` system ceiling.
pub const MIN_MEMORY_MB: u32 = 2048;
pub const MAX_MEMORY_MB: u32 = 32768;

/// Amount of RAM reserved for Windows and other processes, in MB.
const SYSTEM_MEMORY_RESERVE_MB: u64 = 2048;
/// Maximum share of total RAM the launcher may hand to Minecraft.
const MAX_MEMORY_PERCENT: u64 = 80;
const MEMORY_STEP_MB: u64 = 512;

/// Current config schema/migration marker. Bump only together with a
/// deterministic, non-ambiguous migration.
pub const CONFIG_VERSION: u32 = 1;

/// Tiered memory recommendation based on total system RAM (in MB).
///
///   total ≤ 8192   → 4096 (4 GB)
///   total ≤ 16384  → 6144 (6 GB)
///   total ≥ 24576  → 8192 (8 GB, so the ZGC preset is selectable)
///
/// Falls back to 4096 if RAM is unknown.
pub fn recommended_memory_mb(total_ram_mb: u64) -> u32 {
    if total_ram_mb == 0 {
        return 4096;
    }
    if total_ram_mb >= 24 * 1024 {
        return 8192;
    }
    if total_ram_mb >= 16 * 1024 {
        return 6144;
    }
    4096
}

/// Safe global memory ceiling for a known RAM total.
///
/// Leaves 2 GB or 20% for the OS, whichever is larger, caps at
/// [`MAX_MEMORY_MB`] and snaps down to the 512 MB grid. `None` means the
/// system cannot safely provide even the minimum heap (or RAM is unknown):
/// callers must preserve the existing configured limit instead of inventing
/// a new one.
///
/// Keep this formula in lockstep with `getSafeMaxMemoryMb` in
/// `src/lib/memory.ts`; both sides enforce the same contract.
pub fn max_safe_memory_mb(total_ram_mb: u64) -> Option<u32> {
    if total_ram_mb < 4096 {
        return None;
    }
    let usable = total_ram_mb.saturating_sub(SYSTEM_MEMORY_RESERVE_MB);
    let percent_cap = total_ram_mb.saturating_mul(MAX_MEMORY_PERCENT) / 100;
    let safe = usable.min(percent_cap).min(u64::from(MAX_MEMORY_MB));
    let floored = (safe / MEMORY_STEP_MB) * MEMORY_STEP_MB;
    (floored >= u64::from(MIN_MEMORY_MB)).then_some(floored as u32)
}

/// Default memory pair for a fresh config.
///
/// `default_memory_mb` is the tiered recommendation, but never above the
/// safe ceiling. Unknown RAM (0) keeps the historical 4 GB fallback; a known
/// system below 4 GB gets the minimum heap rather than an unsafe "safe max".
fn default_memory_limits(total_ram_mb: u64) -> (u32, u32) {
    let recommended = recommended_memory_mb(total_ram_mb);
    let max_memory_mb = match max_safe_memory_mb(total_ram_mb) {
        Some(safe) => safe,
        // RAM detection failed: keep the historical conservative fallback.
        None if total_ram_mb == 0 => recommended.clamp(MIN_MEMORY_MB, MAX_MEMORY_MB),
        // Known tiny system: do not claim a safe heap above the hard floor.
        None => MIN_MEMORY_MB,
    };
    let max_memory_mb = max_memory_mb.clamp(MIN_MEMORY_MB, MAX_MEMORY_MB);
    let default_memory_mb = recommended.clamp(MIN_MEMORY_MB, max_memory_mb);
    (default_memory_mb, max_memory_mb)
}

/// Guard an explicit memory-limit change: an existing limit may be lowered or
/// kept, but raising it is allowed only up to the safe maximum for this
/// machine. Unknown RAM leaves the caller's existing UI decision untouched.
pub fn validate_memory_cap(
    previous_max_mb: u32,
    new_max_mb: u32,
    total_ram_mb: u64,
) -> Result<(), String> {
    if new_max_mb <= previous_max_mb {
        return Ok(());
    }
    let Some(safe) = max_safe_memory_mb(total_ram_mb) else {
        return Err("The safe memory maximum is unknown on this system. \
             Raising the memory limit is not allowed until RAM detection succeeds."
            .to_string());
    };
    if new_max_mb > safe {
        return Err(format!(
            "The memory limit cannot exceed the safe maximum for this system \
             ({} MB). Lower the limit or use the detected value.",
            safe
        ));
    }
    Ok(())
}

/// The same explicit-growth rule for the default heap. Existing values above
/// the safe maximum are preserved, but an attempt to increase them is rejected
/// until the user chooses a value within the detected ceiling.
pub fn validate_default_memory_cap(
    previous_mb: u32,
    new_mb: u32,
    total_ram_mb: u64,
) -> Result<(), String> {
    if new_mb <= previous_mb {
        return Ok(());
    }
    let Some(safe) = max_safe_memory_mb(total_ram_mb) else {
        return Err("The safe memory maximum is unknown on this system. \
             Raising the default memory is not allowed until RAM detection succeeds."
            .to_string());
    };
    if new_mb > safe {
        return Err(format!(
            "The default memory cannot exceed the safe maximum for this system \
             ({} MB). Lower it or use the detected value.",
            safe
        ));
    }
    Ok(())
}

/// Detect total system RAM in MB using sysinfo. Returns 0 on failure.
pub fn detect_total_ram_mb() -> u64 {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    sys.total_memory() / (1024 * 1024)
}

/// Application configuration persisted to disk
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    /// Base directory for all launcher data (instances, versions, assets)
    pub data_dir: PathBuf,
    /// Microsoft Azure App Client ID for OAuth2 (code-only, not editable in UI)
    #[serde(default = "default_client_id")]
    pub client_id: String,
    /// Default JVM initial memory (Xms) in MB. Also used as Xmx for the
    /// default heap; the actual value is capped by `max_memory_mb`.
    pub default_memory_mb: u32,
    /// Global memory ceiling (MB). It is a system-safety limit, not a second
    /// heap value; the launch path caps both default and per-instance memory
    /// with it. Existing values are never raised automatically.
    pub max_memory_mb: u32,
    /// Default GC preset for new instances: "standard" | "g1gc" | "zgc"
    #[serde(default = "default_gc_preset")]
    pub default_gc_preset: String,
    /// Default JVM arguments
    pub default_jvm_args: Vec<String>,
    /// Monotonic config migration marker. `0` (or a missing field) is an
    /// untouched legacy config; new configs start at the current version.
    /// Ambiguous preference changes (for example, whether `g1gc` was the old
    /// default or an explicit user choice) are never guessed from this marker.
    #[serde(default)]
    pub config_version: u32,
    /// Custom Java path (None = auto-detect)
    pub java_path: Option<PathBuf>,
    /// What to do with the launcher window after the game process has
    /// successfully started: "do_nothing" | "minimize" | "close".
    #[serde(default = "default_after_launch_action")]
    pub after_launch_action: String,
    /// Deprecated pre-1.6 setting: migrated into `after_launch_action`.
    #[serde(default, skip_serializing)]
    pub close_on_launch: bool,
    /// Route all launcher HTTP traffic through a proxy
    #[serde(default)]
    pub proxy_enabled: bool,
    /// Proxy host (e.g. "127.0.0.1")
    #[serde(default)]
    pub proxy_addr: String,
    /// Proxy port (e.g. 8080)
    #[serde(default)]
    pub proxy_port: u16,
    /// Use the Windows system proxy instead of the manual address above.
    /// `None` (field absent) is migrated from the old automatic behaviour:
    /// system proxy only when no manual proxy was configured.
    #[serde(default)]
    pub use_system_proxy: Option<bool>,
    /// Never fall back to a direct connection after a proxy failure.
    /// Defaults to true, including for configs written before this field.
    #[serde(default = "default_proxy_only")]
    pub proxy_only: bool,
    /// Show snapshots in version list
    pub show_snapshots: bool,
    /// Show old versions (alpha/beta)
    pub show_old_versions: bool,
    /// CurseForge API key
    #[serde(default)]
    pub curseforge_api_key: String,
    /// Load the pinned authlib-injector Java agent for Ely.by accounts so
    /// skins work inside Minecraft. Has no effect on Microsoft/offline
    /// accounts. Defaults to true for configs written before this field
    /// existed.
    #[serde(default = "default_elyby_skins_in_game")]
    pub elyby_skins_in_game: bool,
}

fn default_client_id() -> String {
    "c36a9fb6-4f2a-41ff-90bd-ae7cc92031eb".to_string()
}

fn default_gc_preset() -> String {
    "g1gc".to_string()
}

fn default_elyby_skins_in_game() -> bool {
    true
}

fn default_proxy_only() -> bool {
    true
}

fn default_after_launch_action() -> String {
    "do_nothing".to_string()
}

pub const AFTER_LAUNCH_ACTIONS: [&str; 3] = ["do_nothing", "minimize", "close"];

impl Default for AppConfig {
    fn default() -> Self {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("VoidLauncher");

        // Auto-pick a sensible memory default based on system RAM on first
        // launch. New installs also get the safe system ceiling; existing
        // configs keep their stored max_memory_mb (no ambiguous migration).
        let total_ram = detect_total_ram_mb();
        let (default_memory_mb, max_memory_mb) = default_memory_limits(total_ram);
        // ZGC is the default on machines with >= 24 GB RAM (heap >= 8 GB):
        // low pause times win over G1GC there. The launch path validates the
        // actual instance Java and reports a visible fallback warning when
        // ZGC cannot be used.
        let gc_default = if total_ram >= 24 * 1024 {
            "zgc"
        } else {
            "g1gc"
        };
        tracing::info!(
            target: "config",
            "First launch: detected {} MB RAM, defaulting to {} MB (limit {} MB), GC preset {}",
            total_ram, default_memory_mb, max_memory_mb, gc_default
        );

        Self {
            data_dir,
            // Default uses Prism Launcher's registered Azure App ID
            client_id: "c36a9fb6-4f2a-41ff-90bd-ae7cc92031eb".into(),
            default_memory_mb,
            max_memory_mb,
            default_gc_preset: gc_default.into(),
            // NOTE: This list must NOT contain a GC selector flag
            // (UseG1GC / UseZGC / UseParallelGC / …) — the chosen preset
            // in `default_gc_preset` is the single source of truth for
            // which GC the JVM starts with. It must also stay free of
            // GC *tuning* flags: the g1gc preset (Prism-style) already
            // provides the full client set, and duplicated tuning flags
            // from here would override it (custom args are appended after
            // the preset). Only flags that are meaningful for EVERY preset
            // belong below.
            default_jvm_args: vec![
                // Mods occasionally call System.gc() (often in loops) -
                // disabling explicit GC prevents surprise Full GC pauses.
                "-XX:+DisableExplicitGC".into(),
            ],
            config_version: CONFIG_VERSION,
            java_path: None,
            after_launch_action: default_after_launch_action(),
            close_on_launch: false,
            proxy_enabled: false,
            proxy_addr: String::new(),
            proxy_port: 0,
            use_system_proxy: Some(true),
            proxy_only: true,
            show_snapshots: false,
            show_old_versions: false,
            // CurseForge public API key taken from Prism Launcher's open-source
            // defaults — NOT a secret, safe to ship in the repo (CF treats it
            // as an identifier, not a credential).
            curseforge_api_key: "$2a$10$wuAJuNZuted3NORVmpgUC.m8sI.pv1tOPKZyBgLFGjxFp/br0lZCC"
                .into(),
            elyby_skins_in_game: true,
        }
    }
}

impl AppConfig {
    /// Legacy `default_jvm_args` set shipped before the Prism-style G1GC
    /// preset existed (server-oriented Aikar tuning that fought the preset
    /// and meant nothing under ZGC). Configs that still carry this exact
    /// list are assumed to be untouched defaults and get migrated to the
    /// slim current set.
    fn is_legacy_jvm_args(args: &[String]) -> bool {
        let legacy = [
            "-XX:+ParallelRefProcEnabled",
            "-XX:MaxGCPauseMillis=200",
            "-XX:+UnlockExperimentalVMOptions",
            "-XX:+DisableExplicitGC",
            "-XX:G1NewSizePercent=30",
            "-XX:G1MaxNewSizePercent=40",
            "-XX:G1HeapRegionSize=8M",
            "-XX:G1ReservePercent=20",
            "-XX:G1HeapWastePercent=5",
            "-XX:G1MixedGCCountTarget=4",
            "-XX:InitiatingHeapOccupancyPercent=15",
            "-XX:G1MixedGCLiveThresholdPercent=90",
            "-XX:G1RSetUpdatingPauseTimePercent=5",
            "-XX:SurvivorRatio=32",
            "-XX:+PerfDisableSharedMem",
            "-XX:MaxTenuringThreshold=1",
        ];
        args.len() == legacy.len() && legacy.iter().zip(args).all(|(l, a)| *l == a)
    }

    /// Migrate a freshly loaded config without guessing ambiguous user
    /// preferences:
    ///   * clamp impossible heap values;
    ///   * record the config schema version once (legacy configs have no
    ///     marker, so an explicit `g1gc`/6 GB choice is preserved rather than
    ///     reinterpreted as an old default);
    ///   * legacy server-style `default_jvm_args` → slim preset-neutral set.
    ///
    /// Returns true when something changed (caller should persist).
    fn migrate(&mut self) -> bool {
        let mut changed = false;

        // A hand-edited or corrupted config must never hand absurd heaps to
        // the JVM (0 MB aborts, multi-TB values make Windows refuse to start).
        let default_memory = self.default_memory_mb.clamp(MIN_MEMORY_MB, MAX_MEMORY_MB);
        if default_memory != self.default_memory_mb {
            self.default_memory_mb = default_memory;
            changed = true;
        }
        let max_memory = self.max_memory_mb.clamp(MIN_MEMORY_MB, MAX_MEMORY_MB);
        if max_memory != self.max_memory_mb {
            self.max_memory_mb = max_memory;
            changed = true;
        }

        if self.config_version < CONFIG_VERSION {
            // Version 0 has no reliable record of whether a value was the old
            // default or an explicit user choice. Never rewrite an ambiguous
            // preference here: keep G1GC/6 GB exactly as stored. Only record
            // that the schema has been seen.
            self.config_version = CONFIG_VERSION;
            tracing::info!(
                target: "config",
                "Migrated config schema to version {} (user choices preserved)",
                CONFIG_VERSION
            );
            changed = true;
        }

        if Self::is_legacy_jvm_args(&self.default_jvm_args) {
            self.default_jvm_args = vec!["-XX:+DisableExplicitGC".into()];
            tracing::info!(
                target: "config",
                "Migrated legacy default_jvm_args to slim preset-neutral set"
            );
            changed = true;
        }

        // Pre-1.6 automatically used the Windows proxy when no manual proxy
        // was configured. Preserve that, but make the source explicit: a
        // configured manual proxy keeps manual mode, otherwise system mode.
        if self.use_system_proxy.is_none() {
            self.use_system_proxy = Some(!self.has_manual_proxy());
            tracing::info!(
                target: "config",
                "Migrated proxy source to {}",
                if self.use_system_proxy == Some(true) { "Windows system proxy" } else { "manual proxy" }
            );
            changed = true;
        }

        // Pre-1.6 had a single "close on launch" boolean. Keep the old
        // preference: on -> close, off -> do nothing. The new selector value
        // wins if it is already set (new configs never write the old field).
        if self.close_on_launch {
            if self.after_launch_action == "do_nothing" {
                self.after_launch_action = "close".into();
            }
            self.close_on_launch = false;
            changed = true;
        }
        if !AFTER_LAUNCH_ACTIONS.contains(&self.after_launch_action.as_str()) {
            self.after_launch_action = default_after_launch_action();
            changed = true;
        }

        changed
    }
}

/// Keep a copy of a config that failed to parse before the launcher replaces
/// it with defaults. Without this a single bad byte silently discarded every
/// user setting (data dir, Java path, proxy, API key) with no way back.
fn backup_corrupt_config(path: &std::path::Path) {
    let backup = path.with_file_name(format!(
        "config.json.bad-{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));
    match std::fs::copy(path, &backup) {
        Ok(_) => tracing::warn!(
            target: "config",
            "Preserved unreadable config as {}",
            backup.display()
        ),
        Err(e) => tracing::warn!(
            target: "config",
            "Could not preserve unreadable config at {}: {}",
            path.display(),
            e
        ),
    }
}

impl AppConfig {
    /// Load config from disk or create default.
    ///
    /// **Never panics.** Any of the following are handled gracefully by
    /// falling back to `Default::default()` and (re)writing a fresh
    /// config file:
    ///   * file does not exist  → first launch
    ///   * file is empty        → corruption / interrupted write
    ///   * file is unreadable   → permission error / disk error
    ///   * JSON parse error     → manual edit, partial write, BOM, etc.
    ///   * serde flatten error  → unknown / wrong-type fields
    ///
    /// Every fallback path is logged via `tracing::warn!` so the
    /// `launcher.log` file shows exactly what went wrong, even if the
    /// user just deletes `config.json` and the issue never recurs.
    pub fn load(data_dir: &std::path::Path) -> Self {
        let config_path = data_dir.join("config.json");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(contents) => {
                    if contents.trim().is_empty() {
                        tracing::warn!(
                            target: "config",
                            "Config file at {} is empty; rewriting with defaults",
                            config_path.display()
                        );
                    } else {
                        match serde_json::from_str::<Self>(&contents) {
                            Ok(mut config) => {
                                tracing::info!(
                                    target: "config",
                                    "Loaded config from {}",
                                    config_path.display()
                                );
                                if config.migrate() {
                                    if let Err(e) = config.save() {
                                        tracing::warn!(
                                            target: "config",
                                            "Failed to persist migrated config: {}",
                                            e
                                        );
                                    }
                                }
                                return config;
                            }
                            Err(e) => {
                                backup_corrupt_config(&config_path);
                                tracing::warn!(
                                    target: "config",
                                    "Failed to parse config at {}: {}; rewriting with defaults",
                                    config_path.display(),
                                    e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    backup_corrupt_config(&config_path);
                    tracing::warn!(
                        target: "config",
                        "Failed to read config at {}: {}; rewriting with defaults",
                        config_path.display(),
                        e
                    );
                }
            }
        } else {
            tracing::info!(
                target: "config",
                "No config at {}; creating with defaults",
                config_path.display()
            );
        }
        let config = Self {
            data_dir: data_dir.to_path_buf(),
            ..Default::default()
        };
        if let Err(e) = config.save() {
            tracing::warn!(
                target: "config",
                "Failed to write fresh config at {}: {}",
                config_path.display(),
                e
            );
        }
        config
    }

    /// Save config to disk
    pub fn save(&self) -> crate::error::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        let config_path = self.data_dir.join("config.json");
        let json = serde_json::to_string_pretty(self)?;
        // Atomic replace: a crash or full disk during Save must leave the
        // previous working config intact, never a truncated file.
        crate::instances::write_file_atomic(&config_path, json.as_bytes())
    }

    /// Get versions directory
    pub fn versions_dir(&self) -> PathBuf {
        self.data_dir.join("versions")
    }

    /// Get instances directory
    pub fn instances_dir(&self) -> PathBuf {
        self.data_dir.join("instances")
    }

    /// Get assets directory
    pub fn assets_dir(&self) -> PathBuf {
        self.data_dir.join("assets")
    }

    /// Get libraries directory
    pub fn libraries_dir(&self) -> PathBuf {
        self.data_dir.join("libraries")
    }

    /// Get auth tokens file
    pub fn auth_file(&self) -> PathBuf {
        self.data_dir.join("auth.json")
    }

    /// Proxy `host:port` for reqwest (`None` when disabled). The scheme is
    /// resolved automatically (HTTP first, then SOCKS5) in `download.rs`.
    pub fn proxy_url(&self) -> Option<String> {
        proxy_url_from(self.proxy_enabled, &self.proxy_addr, self.proxy_port)
    }

    /// Effective system-proxy choice after migration.
    pub fn use_system_proxy(&self) -> bool {
        self.use_system_proxy.unwrap_or(true)
    }

    /// Validate the memory/GC pair before it is persisted.
    ///
    /// This deliberately returns an error instead of silently clamping: the
    /// renderer shows the same limits, so a rejected save means the user saw
    /// an invalid combination and should fix it (or raise the explicit cap).
    pub fn validate_memory_and_gc(&self) -> Result<(), String> {
        if self.default_memory_mb > self.max_memory_mb {
            return Err(format!(
                "Default memory ({} MB) exceeds the global memory limit ({} MB). \
                 Increase the limit or lower the default memory.",
                self.default_memory_mb, self.max_memory_mb
            ));
        }
        if self.default_gc_preset.eq_ignore_ascii_case("zgc") && self.default_memory_mb < 8192 {
            return Err(
                "ZGC requires at least 8 GB of heap. Increase the default memory \
                 or choose the G1GC preset."
                    .to_string(),
            );
        }
        Ok(())
    }

    fn has_manual_proxy(&self) -> bool {
        !self.proxy_addr.trim().is_empty() && self.proxy_port > 0
    }
}

/// Format a proxy `host:port`, or `None` when the master switch is off or the
/// address/port are incomplete. Shared by the persisted config and the
/// immediate (unsaved) preview from the Network settings tab.
pub fn proxy_url_from(enabled: bool, addr: &str, port: u16) -> Option<String> {
    if enabled && !addr.trim().is_empty() && port > 0 {
        Some(format!("{}:{}", addr.trim(), port))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> AppConfig {
        AppConfig::default()
    }

    #[test]
    fn safe_memory_ceiling_matches_the_frontend_contract() {
        assert_eq!(max_safe_memory_mb(0), None);
        assert_eq!(max_safe_memory_mb(3 * 1024), None);
        assert_eq!(max_safe_memory_mb(4 * 1024), Some(2048));
        assert_eq!(max_safe_memory_mb(8 * 1024), Some(6144));
        assert_eq!(max_safe_memory_mb(16 * 1024), Some(12800));
        assert_eq!(max_safe_memory_mb(24 * 1024), Some(19456));
        assert_eq!(max_safe_memory_mb(32 * 1024), Some(26112));
        assert_eq!(
            max_safe_memory_mb(64 * 1024),
            Some(MAX_MEMORY_MB),
            "the 512 MB cap must never exceed the backend hard limit"
        );
    }

    #[test]
    fn fresh_config_memory_defaults_follow_the_safe_ceiling() {
        assert_eq!(default_memory_limits(0), (4096, 4096));
        assert_eq!(default_memory_limits(3 * 1024), (2048, 2048));
        assert_eq!(default_memory_limits(4 * 1024), (2048, 2048));
        assert_eq!(default_memory_limits(8 * 1024), (4096, 6144));
        assert_eq!(default_memory_limits(16 * 1024), (6144, 12800));
        assert_eq!(default_memory_limits(24 * 1024), (8192, 19456));
        assert_eq!(default_memory_limits(32 * 1024), (8192, 26112));
        assert_eq!(default_memory_limits(64 * 1024), (8192, MAX_MEMORY_MB));
    }

    #[test]
    fn memory_and_gc_validation_rejects_untruthful_pairs() {
        let mut config = base();
        config.default_memory_mb = 8192;
        config.max_memory_mb = 6144;
        assert!(config.validate_memory_and_gc().is_err());

        config.max_memory_mb = 8192;
        config.default_gc_preset = "zgc".into();
        config.default_memory_mb = 6144;
        assert!(config.validate_memory_and_gc().is_err());

        config.default_memory_mb = 8192;
        assert!(config.validate_memory_and_gc().is_ok());

        config.default_gc_preset = "g1gc".into();
        config.default_memory_mb = 4096;
        assert!(config.validate_memory_and_gc().is_ok());
    }

    #[test]
    fn memory_limit_can_only_rise_to_the_safe_maximum() {
        // Existing 8 GB cap on a 32 GB system: the safe ceiling is ~25.5 GB.
        assert!(validate_memory_cap(8192, 8192, 32 * 1024).is_ok());
        assert!(validate_memory_cap(8192, 4096, 32 * 1024).is_ok());
        assert!(validate_memory_cap(8192, 26112, 32 * 1024).is_ok());
        assert!(validate_memory_cap(8192, 26624, 32 * 1024).is_err());
        assert!(validate_memory_cap(8192, 32768, 32 * 1024).is_err());

        // A legacy value above the safe ceiling is preserved while unchanged;
        // only a further increase is rejected.
        assert!(validate_memory_cap(32768, 32768, 8 * 1024).is_ok());
        assert!(validate_memory_cap(8192, 16384, 8 * 1024).is_err());
    }

    #[test]
    fn memory_limit_cannot_rise_when_the_safe_maximum_is_unknown() {
        // Unknown RAM and a known tiny system both have no safe ceiling.
        for total_ram_mb in [0, 3 * 1024] {
            assert!(validate_memory_cap(8192, 8192, total_ram_mb).is_ok());
            assert!(validate_memory_cap(8192, 4096, total_ram_mb).is_ok());
            assert!(validate_memory_cap(8192, 12288, total_ram_mb).is_err());
            assert!(validate_memory_cap(8192, 32768, total_ram_mb).is_err());
        }
    }

    #[test]
    fn default_memory_can_only_rise_to_the_safe_maximum() {
        assert!(validate_default_memory_cap(8192, 8192, 32 * 1024).is_ok());
        assert!(validate_default_memory_cap(8192, 4096, 32 * 1024).is_ok());
        assert!(validate_default_memory_cap(8192, 26112, 32 * 1024).is_ok());
        assert!(validate_default_memory_cap(8192, 26624, 32 * 1024).is_err());
        assert!(validate_default_memory_cap(32768, 32768, 8 * 1024).is_ok());
        assert!(validate_default_memory_cap(8192, 16384, 8 * 1024).is_err());
        assert!(validate_default_memory_cap(4096, 4096, 0).is_ok());
        assert!(validate_default_memory_cap(4096, 8192, 0).is_err());
        assert!(validate_default_memory_cap(2048, 4096, 3 * 1024).is_err());
    }

    #[test]
    fn proxy_url_disabled_by_default() {
        assert_eq!(base().proxy_url(), None);
    }

    #[test]
    fn elyby_skins_are_enabled_by_default() {
        assert!(base().elyby_skins_in_game);
    }

    #[test]
    fn proxy_only_is_enabled_by_default() {
        assert!(base().proxy_only);
    }

    #[test]
    fn legacy_config_without_elyby_setting_defaults_to_true() {
        // Every field that has no serde default; the new Ely.by flag is
        // intentionally absent, exactly as in configs written before 1.5.x.
        let legacy = serde_json::json!({
            "data_dir": "C:\\VoidLauncher",
            "default_memory_mb": 4096,
            "max_memory_mb": 8192,
            "default_jvm_args": ["-XX:+DisableExplicitGC"],
            "java_path": null,
            "close_on_launch": false,
            "show_snapshots": false,
            "show_old_versions": false
        });
        let config: AppConfig = serde_json::from_value(legacy).expect("legacy config parses");
        assert!(config.elyby_skins_in_game);
        assert!(config.proxy_only);
        assert_eq!(
            config.config_version, 0,
            "a config without the marker is a legacy config"
        );
    }

    #[test]
    fn legacy_memory_cap_is_preserved_on_load() {
        let dir = std::env::temp_dir().join(format!("vl-config-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = serde_json::json!({
            "data_dir": dir,
            "default_memory_mb": 4096,
            "max_memory_mb": 4096,
            "default_jvm_args": ["-XX:+DisableExplicitGC"],
            "java_path": null,
            "show_snapshots": false,
            "show_old_versions": false
        });
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let loaded = AppConfig::load(&dir);
        assert_eq!(
            loaded.default_memory_mb, 4096,
            "a stored default must survive migration"
        );
        assert_eq!(
            loaded.max_memory_mb, 4096,
            "a stored memory cap must never be raised automatically"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_proxy_source_migrates_from_manual_or_automatic_system_mode() {
        let mut manual = base();
        manual.use_system_proxy = None;
        manual.proxy_addr = "127.0.0.1".into();
        manual.proxy_port = 8080;
        assert!(manual.migrate());
        assert_eq!(manual.use_system_proxy, Some(false));

        let mut automatic = base();
        automatic.use_system_proxy = None;
        automatic.proxy_addr = String::new();
        automatic.proxy_port = 0;
        assert!(automatic.migrate());
        assert_eq!(automatic.use_system_proxy, Some(true));
    }

    #[test]
    fn explicit_proxy_source_is_preserved_and_new_default_is_system() {
        assert!(base().use_system_proxy());

        let mut manual = base();
        manual.proxy_enabled = true;
        manual.use_system_proxy = Some(false);
        manual.proxy_addr = "127.0.0.1".into();
        manual.proxy_port = 8080;
        let _ = manual.migrate();
        assert_eq!(manual.use_system_proxy, Some(false));
        assert_eq!(manual.proxy_url(), Some("127.0.0.1:8080".into()));
        assert_eq!(manual.proxy_addr, "127.0.0.1");
        assert_eq!(manual.proxy_port, 8080);

        let mut system = base();
        system.use_system_proxy = Some(true);
        system.proxy_addr = "10.0.0.1".into();
        system.proxy_port = 3128;
        let _ = system.migrate();
        assert_eq!(system.use_system_proxy, Some(true));
        assert_eq!(
            system.proxy_addr, "10.0.0.1",
            "manual settings must be kept"
        );
        assert_eq!(system.proxy_port, 3128);
    }

    #[test]
    fn proxy_source_and_manual_values_survive_save_and_load() {
        let dir = std::env::temp_dir().join(format!("vl-config-proxy-{}", uuid::Uuid::new_v4()));
        let mut config = base();
        config.data_dir = dir.clone();
        config.proxy_enabled = true;
        config.use_system_proxy = Some(true);
        config.proxy_addr = "10.0.0.7".into();
        config.proxy_port = 3128;
        config.proxy_only = true;
        config.after_launch_action = "minimize".into();
        config.save().unwrap();

        let loaded = AppConfig::load(&dir);
        assert!(loaded.proxy_enabled);
        assert_eq!(loaded.use_system_proxy, Some(true));
        assert_eq!(loaded.proxy_addr, "10.0.0.7");
        assert_eq!(loaded.proxy_port, 3128);
        assert!(loaded.proxy_only);
        assert_eq!(loaded.after_launch_action, "minimize");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_migration_never_reinterprets_an_explicit_gc_choice() {
        // A legacy config has no marker. Its stored g1gc may be the old
        // default or an explicit choice; migration must preserve it.
        let mut legacy = base();
        legacy.config_version = 0;
        legacy.default_gc_preset = "g1gc".into();
        legacy.default_memory_mb = 8192;
        legacy.max_memory_mb = 8192;
        assert!(legacy.migrate(), "schema marker is recorded once");
        assert_eq!(legacy.default_gc_preset, "g1gc");
        assert_eq!(
            legacy.max_memory_mb, 8192,
            "an existing memory cap must never be raised by migration"
        );
        assert_eq!(legacy.config_version, CONFIG_VERSION);
        assert!(
            !legacy.migrate(),
            "a second run must not change anything (idempotent migration)"
        );
        assert_eq!(legacy.default_gc_preset, "g1gc");

        // Explicit ZGC survives as long as the heap is large enough.
        let mut zgc = base();
        zgc.config_version = 0;
        zgc.default_gc_preset = "zgc".into();
        zgc.default_memory_mb = 8192;
        zgc.max_memory_mb = 8192;
        assert!(zgc.migrate());
        assert_eq!(zgc.default_gc_preset, "zgc");

        // The old 6 GB value is preserved too: it may be a deliberate choice.
        let mut six_gb = base();
        six_gb.config_version = 0;
        six_gb.default_gc_preset = "g1gc".into();
        six_gb.default_memory_mb = 6144;
        six_gb.max_memory_mb = 6144;
        assert!(six_gb.migrate());
        assert_eq!(six_gb.default_memory_mb, 6144);
        assert_eq!(six_gb.max_memory_mb, 6144);

        // An explicit low-heap ZGC choice is preserved rather than rewritten;
        // Settings marks it invalid and the launch path reports the fallback.
        let mut low = base();
        low.config_version = 0;
        low.default_gc_preset = "zgc".into();
        low.default_memory_mb = 4096;
        low.max_memory_mb = 4096;
        assert!(low.migrate());
        assert_eq!(low.default_gc_preset, "zgc");
    }

    #[test]
    fn absurd_memory_values_are_clamped() {
        let mut low = base();
        low.default_memory_mb = 0;
        low.max_memory_mb = 1;
        assert!(low.migrate());
        assert_eq!(low.default_memory_mb, 2048);
        assert_eq!(low.max_memory_mb, 2048);

        let mut high = base();
        high.default_memory_mb = 1_000_000;
        high.max_memory_mb = u32::MAX;
        assert!(high.migrate());
        assert_eq!(high.default_memory_mb, 32768);
        assert_eq!(high.max_memory_mb, 32768);
    }

    #[test]
    fn config_save_replaces_atomically_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("vl-config-atomic-{}", uuid::Uuid::new_v4()));
        let mut config = base();
        config.data_dir = dir.clone();
        config.default_memory_mb = 6144;
        config.save().unwrap();

        let path = dir.join("config.json");
        let first: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(first["default_memory_mb"], 6144);
        assert!(
            !dir.join(".config.json.tmp").exists(),
            "atomic save must not leave a temp file"
        );

        config.default_memory_mb = 8192;
        config.save().unwrap();
        let second: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(second["default_memory_mb"], 8192);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_config_is_preserved_before_defaults_are_written() {
        let dir = std::env::temp_dir().join(format!("vl-config-corrupt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, b"{ definitely not json").unwrap();

        let loaded = AppConfig::load(&dir);
        assert_eq!(loaded.data_dir, dir);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"data_dir\""),
            "a fresh valid config must be written"
        );

        let backup = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("config.json.bad-")
            })
            .expect("corrupt config must be backed up");
        assert_eq!(
            std::fs::read_to_string(backup.path()).unwrap(),
            "{ definitely not json"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_close_on_launch_migrates_to_close_action() {
        let mut c = base();
        c.close_on_launch = true;
        c.after_launch_action = "do_nothing".into();
        assert!(c.migrate());
        assert_eq!(c.after_launch_action, "close");
        assert!(!c.close_on_launch);
    }

    #[test]
    fn explicit_after_launch_action_survives_migration() {
        let mut c = base();
        c.after_launch_action = "minimize".into();
        c.close_on_launch = false;
        assert!(!c.migrate());
        assert_eq!(c.after_launch_action, "minimize");

        c.after_launch_action = "bogus".into();
        assert!(c.migrate());
        assert_eq!(c.after_launch_action, "do_nothing");
    }

    #[test]
    fn proxy_url_enabled() {
        let mut c = base();
        c.proxy_enabled = true;
        c.proxy_addr = " 127.0.0.1 ".into();
        c.proxy_port = 8080;
        assert_eq!(c.proxy_url(), Some("127.0.0.1:8080".to_string()));
    }

    #[test]
    fn proxy_url_ignores_missing_addr_or_port() {
        let mut c = base();
        c.proxy_enabled = true;
        assert_eq!(c.proxy_url(), None);
        c.proxy_addr = "127.0.0.1".into();
        assert_eq!(c.proxy_url(), None);
        c.proxy_addr = String::new();
        c.proxy_port = 8080;
        assert_eq!(c.proxy_url(), None);
    }
}

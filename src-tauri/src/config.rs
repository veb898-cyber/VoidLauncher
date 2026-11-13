use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    /// Default JVM initial memory (Xms) in MB
    pub default_memory_mb: u32,
    /// Default JVM max memory (Xmx) in MB
    pub max_memory_mb: u32,
    /// Default GC preset for new instances: "standard" | "g1gc" | "zgc"
    #[serde(default = "default_gc_preset")]
    pub default_gc_preset: String,
    /// Default JVM arguments
    pub default_jvm_args: Vec<String>,
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

        // Auto-pick a sensible memory default based on system RAM on first launch.
        let total_ram = detect_total_ram_mb();
        let recommended = recommended_memory_mb(total_ram);
        // ZGC is the default on machines with >= 24 GB RAM (heap >= 8 GB):
        // low pause times win over G1GC there, and `build_jvm_args` still
        // falls back to G1GC if Java < 17 or the heap is too small.
        let gc_default = if total_ram >= 24 * 1024 {
            "zgc"
        } else {
            "g1gc"
        };
        tracing::info!(
            target: "config",
            "First launch: detected {} MB RAM, defaulting to {} MB, GC preset {}",
            total_ram, recommended, gc_default
        );

        Self {
            data_dir,
            // Default uses Prism Launcher's registered Azure App ID
            client_id: "c36a9fb6-4f2a-41ff-90bd-ae7cc92031eb".into(),
            default_memory_mb: recommended,
            max_memory_mb: recommended,
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
                // Mods occasionally call System.gc() (often in loops) —
                // disabling explicit GC prevents surprise Full GC pauses.
                "-XX:+DisableExplicitGC".into(),
            ],
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

    /// Migrate pre-0.1.7 defaults in a freshly loaded config so the new
    /// Prism-style GC behaviour applies without the user touching settings:
    ///   * 6 GB default on big-RAM machines → 8 GB (matches the tiered
    ///     recommendation now that ≥ 24 GB RAM maps to 8 GB);
    ///   * ZGC preset with less than 8 GB of heap → G1GC (ZGC requires
    ///     8+ GB; on 6 GB it stalled the client during pack/server loads);
    ///   * legacy server-style `default_jvm_args` → slim preset-neutral set.
    ///
    /// Returns true when something changed (caller should persist).
    fn migrate(&mut self) -> bool {
        let mut changed = false;
        let total_ram = detect_total_ram_mb();

        // A hand-edited or corrupted config must never hand absurd heaps to
        // the JVM (0 MB aborts, multi-TB values make Windows refuse to start).
        let default_memory = self.default_memory_mb.clamp(2048, 32768);
        if default_memory != self.default_memory_mb {
            self.default_memory_mb = default_memory;
            changed = true;
        }
        let max_memory = self.max_memory_mb.clamp(2048, 32768);
        if max_memory != self.max_memory_mb {
            self.max_memory_mb = max_memory;
            changed = true;
        }

        if total_ram >= 24 * 1024 && self.default_memory_mb == 6144 && self.max_memory_mb == 6144 {
            self.default_memory_mb = 8192;
            self.max_memory_mb = 8192;
            tracing::info!(
                target: "config",
                "Migrated default memory 6144 -> 8192 MB (system has {} MB RAM)",
                total_ram
            );
            changed = true;
        }

        if self.default_gc_preset.eq_ignore_ascii_case("zgc") && self.default_memory_mb < 8192 {
            self.default_gc_preset = "g1gc".into();
            tracing::info!(
                target: "config",
                "Migrated default GC preset zgc -> g1gc (ZGC needs >= 8 GB heap, current: {} MB)",
                self.default_memory_mb
            );
            changed = true;
        }

        // ZGC became the default for machines with >= 24 GB RAM (heaps >= 8 GB).
        // Only flip untouched-looking configs: heap still at the recommended
        // 8 GB and the old hard-coded "g1gc" default. A manual choice (any
        // other memory value or preset) is left alone.
        if total_ram >= 24 * 1024
            && self.default_memory_mb >= 8192
            && self.max_memory_mb >= 8192
            && self.default_gc_preset.eq_ignore_ascii_case("g1gc")
        {
            self.default_gc_preset = "zgc".into();
            tracing::info!(
                target: "config",
                "Migrated default GC preset g1gc -> zgc (system has {} MB RAM, heap {} MB)",
                total_ram, self.default_memory_mb
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

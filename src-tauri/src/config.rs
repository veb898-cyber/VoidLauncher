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
    /// Close launcher when game starts
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
    /// Show snapshots in version list
    pub show_snapshots: bool,
    /// Show old versions (alpha/beta)
    pub show_old_versions: bool,
    /// CurseForge API key
    #[serde(default)]
    pub curseforge_api_key: String,
}

fn default_client_id() -> String {
    "c36a9fb6-4f2a-41ff-90bd-ae7cc92031eb".to_string()
}

fn default_gc_preset() -> String {
    "g1gc".to_string()
}

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
        let gc_default = if total_ram >= 24 * 1024 { "zgc" } else { "g1gc" };
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
            close_on_launch: false,
            proxy_enabled: false,
            proxy_addr: String::new(),
            proxy_port: 0,
            show_snapshots: false,
            show_old_versions: false,
            curseforge_api_key: "$2a$10$wuAJuNZuted3NORVmpgUC.m8sI.pv1tOPKZyBgLFGjxFp/br0lZCC".into(),
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
    /// Returns true when something changed (caller should persist).
    fn migrate(&mut self) -> bool {
        let mut changed = false;
        let total_ram = detect_total_ram_mb();

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

        changed
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
                            Err(e) => tracing::warn!(
                                target: "config",
                                "Failed to parse config at {}: {}; rewriting with defaults",
                                config_path.display(),
                                e
                            ),
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    target: "config",
                    "Failed to read config at {}: {}; rewriting with defaults",
                    config_path.display(),
                    e
                ),
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
        std::fs::write(config_path, json)?;
        Ok(())
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

    /// Get icon cache file
    pub fn icon_cache_file(&self) -> PathBuf {
        self.data_dir.join("icon_cache.json")
    }

    /// Proxy URL for reqwest (`None` when disabled). E.g. `http://127.0.0.1:8080`.
    pub fn proxy_url(&self) -> Option<String> {
        if self.proxy_enabled && !self.proxy_addr.trim().is_empty() && self.proxy_port > 0 {
            Some(format!("http://{}:{}", self.proxy_addr.trim(), self.proxy_port))
        } else {
            None
        }
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
    fn proxy_url_enabled() {
        let mut c = base();
        c.proxy_enabled = true;
        c.proxy_addr = " 127.0.0.1 ".into();
        c.proxy_port = 8080;
        assert_eq!(c.proxy_url(), Some("http://127.0.0.1:8080".to_string()));
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

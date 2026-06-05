use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Tiered memory recommendation based on total system RAM (in MB).
///
///   total ≤ 8192   → 4096 (4 GB)
///   total ≤ 16384  → 6144 (6 GB)
///   total ≥ 32768  → 8192 (8 GB, so the ZGC preset is selectable)
///
/// Falls back to 4096 if RAM is unknown.
pub fn recommended_memory_mb(total_ram_mb: u64) -> u32 {
    if total_ram_mb == 0 {
        return 4096;
    }
    if total_ram_mb >= 32 * 1024 {
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
        eprintln!(
            "[config] First launch: detected {} MB RAM, defaulting to {} MB",
            total_ram, recommended
        );

        Self {
            data_dir,
            // Default uses Prism Launcher's registered Azure App ID
            client_id: "c36a9fb6-4f2a-41ff-90bd-ae7cc92031eb".into(),
            default_memory_mb: recommended,
            max_memory_mb: recommended,
            default_gc_preset: "g1gc".into(),
            // NOTE: This list must NOT contain a GC selector flag
            // (UseG1GC / UseZGC / UseParallelGC / …). The user's chosen
            // preset in `default_gc_preset` is the single source of
            // truth for which GC the JVM starts with — adding
            // `-XX:+UseG1GC` here would conflict with the ZGC preset
            // and crash the JVM with "multiple garbage collectors
            // selected". Only GC-*tuning* flags (region size, pause
            // target, mixed-GC counts, etc.) belong below.
            default_jvm_args: vec![
                "-XX:+ParallelRefProcEnabled".into(),
                "-XX:MaxGCPauseMillis=200".into(),
                "-XX:+UnlockExperimentalVMOptions".into(),
                "-XX:+DisableExplicitGC".into(),
                "-XX:G1NewSizePercent=30".into(),
                "-XX:G1MaxNewSizePercent=40".into(),
                "-XX:G1HeapRegionSize=8M".into(),
                "-XX:G1ReservePercent=20".into(),
                "-XX:G1HeapWastePercent=5".into(),
                "-XX:G1MixedGCCountTarget=4".into(),
                "-XX:InitiatingHeapOccupancyPercent=15".into(),
                "-XX:G1MixedGCLiveThresholdPercent=90".into(),
                "-XX:G1RSetUpdatingPauseTimePercent=5".into(),
                "-XX:SurvivorRatio=32".into(),
                "-XX:+PerfDisableSharedMem".into(),
                "-XX:MaxTenuringThreshold=1".into(),
            ],
            java_path: None,
            close_on_launch: false,
            show_snapshots: false,
            show_old_versions: false,
            curseforge_api_key: "$2a$10$wuAJuNZuted3NORVmpgUC.m8sI.pv1tOPKZyBgLFGjxFp/br0lZCC".into(),
        }
    }
}

impl AppConfig {
    /// Load config from disk or create default
    pub fn load(data_dir: &std::path::Path) -> Self {
        let config_path = data_dir.join("config.json");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(contents) => match serde_json::from_str(&contents) {
                    Ok(config) => return config,
                    Err(e) => eprintln!("Failed to parse config: {}", e),
                },
                Err(e) => eprintln!("Failed to read config: {}", e),
            }
        }
        let config = Self {
            data_dir: data_dir.to_path_buf(),
            ..Default::default()
        };
        let _ = config.save();
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
}

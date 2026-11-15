use crate::error::{LauncherError, Result};
use crate::modloaders::LoaderProfile;
use chrono::Utc;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Validate one on-disk path component. Free-form world names may start with
/// a dot, so instance-specific restrictions live in
/// [`validate_instance_dir_name`].
pub(crate) fn validate_path_component(name: &str, what: &str) -> Result<()> {
    if name.is_empty() {
        return Err(LauncherError::Instance(format!("{} is required.", what)));
    }
    if name != name.trim() {
        return Err(LauncherError::Instance(format!(
            "{} may not start or end with spaces.",
            what
        )));
    }
    if name.ends_with('.') {
        return Err(LauncherError::Instance(format!(
            "{} may not end with a dot.",
            what
        )));
    }
    if name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains(':')
    {
        return Err(LauncherError::Instance(format!(
            "{} contains invalid path characters.",
            what
        )));
    }
    if name == "." || name == ".." {
        return Err(LauncherError::Instance(format!(
            "Invalid {}: '{}'.",
            what, name
        )));
    }
    // Windows reserves the device name itself and any extension of it
    // (`CON`, `CON.txt`, ...), so inspect the stem.
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Err(LauncherError::Instance(format!(
            "'{}' is a reserved name.",
            name
        )));
    }
    for ch in name.chars() {
        if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            return Err(LauncherError::Instance(format!(
                "{} contains an invalid character: {:?}",
                what, ch
            )));
        }
    }
    Ok(())
}

/// Validate a name used as an instance directory. This is the single
/// backend gate for IPC-provided names and metadata read from disk: it
/// guarantees the joined path stays one level below `instances_dir`.
pub(crate) fn validate_instance_dir_name(name: &str, what: &str) -> Result<()> {
    validate_path_component(name, what)?;
    if name.starts_with('.') {
        return Err(LauncherError::Instance(format!(
            "{} may not start with a dot.",
            what
        )));
    }
    Ok(())
}

/// Process-wide name registry for instance directory operations. The
/// single-instance mutex already excludes another launcher process; this
/// registry closes the intra-process gap between "target does not exist" and
/// the actual rename/copy/delete. Import reservations use the same keys, so
/// create/import/rename/duplicate/restore/delete cannot occupy one target at
/// the same time.
static INSTANCE_NAME_REGISTRY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn normalized_instance_base(instances_dir: &Path) -> PathBuf {
    let absolute = if instances_dir.is_absolute() {
        instances_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(instances_dir)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    // Two lexical paths can name the same directory through a junction or a
    // symlink. Resolve the real path when it exists so create/rename/delete
    // and import all share one registry key. If it does not exist yet, the
    // lexical key is the best available and all callers still agree on it.
    std::fs::canonicalize(&normalized)
        .map(strip_verbatim_prefix)
        .unwrap_or(normalized)
}

#[cfg(windows)]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{}", rest))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest.to_string())
    } else {
        path
    }
}

#[cfg(not(windows))]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

fn instance_name_key(instances_dir: &Path, name: &str) -> String {
    format!(
        "{}\u{0}{}",
        normalized_instance_base(instances_dir)
            .to_string_lossy()
            .to_lowercase(),
        name.to_lowercase()
    )
}

#[derive(Debug)]
pub(crate) struct InstanceNameReservation {
    keys: Vec<String>,
}

impl InstanceNameReservation {
    /// Atomically reserve one or more names. Fails instead of waiting: the
    /// caller returns a clear "operation in progress" error to the user.
    pub(crate) fn acquire(instances_dir: &Path, names: &[&str]) -> Result<Self> {
        for name in names {
            validate_instance_dir_name(name, "Instance name")?;
        }
        let mut keys: Vec<String> = names
            .iter()
            .map(|name| instance_name_key(instances_dir, name))
            .collect();
        keys.sort();
        keys.dedup();

        let registry = INSTANCE_NAME_REGISTRY.get_or_init(|| Mutex::new(HashSet::new()));
        let mut held = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if keys.iter().any(|key| held.contains(key)) {
            return Err(LauncherError::Instance(
                "Another operation is already creating, renaming or deleting this instance. \
                 Try again in a moment."
                    .to_string(),
            ));
        }
        for key in &keys {
            held.insert(key.clone());
        }
        Ok(Self { keys })
    }
}

impl Drop for InstanceNameReservation {
    fn drop(&mut self) {
        let registry = INSTANCE_NAME_REGISTRY.get_or_init(|| Mutex::new(HashSet::new()));
        let mut held = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for key in &self.keys {
            held.remove(key);
        }
    }
}

/// Deserialize `memory_mb` without letting one bad field make the whole
/// instance unreadable. Older/hand-edited files can contain `null`, `0`,
/// negative numbers or absurd values; those must not turn a valid instance
/// into an invisible/broken one. Negative/zero values inherit the global
/// default; oversized values are capped and then re-capped by the global
/// `max_memory_mb` at launch/save time.
fn deserialize_optional_memory_mb<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct MemoryVisitor;

    impl<'de> Visitor<'de> for MemoryVisitor {
        type Value = Option<u32>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an optional non-negative integer memory value")
        }

        fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(MemoryVisitor)
        }

        fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
            Ok(if value <= 0 {
                None
            } else {
                Some(value.min(crate::config::MAX_MEMORY_MB as i64) as u32)
            })
        }

        fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
            Ok(if value == 0 {
                None
            } else {
                Some(value.min(crate::config::MAX_MEMORY_MB as u64) as u32)
            })
        }

        fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(if !value.is_finite() || value <= 0.0 {
                None
            } else {
                Some(value.min(crate::config::MAX_MEMORY_MB as f64) as u32)
            })
        }
    }

    deserializer.deserialize_option(MemoryVisitor)
}

/// Instance configuration
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Instance {
    /// Unique instance name
    pub name: String,
    /// Minecraft version ID (e.g., "26.1.2")
    pub mc_version: String,
    /// Mod loader type
    pub loader: LoaderType,
    /// Mod loader version (if applicable)
    pub loader_version: Option<String>,
    /// Cached loader profile (main class override, libraries, args)
    pub loader_profile: Option<LoaderProfile>,
    /// Custom JVM memory in MB (None = use global default)
    #[serde(default, deserialize_with = "deserialize_optional_memory_mb")]
    pub memory_mb: Option<u32>,
    /// Custom JVM arguments (None = use global default)
    pub jvm_args: Option<Vec<String>>,
    /// GC preset: "standard" | "g1gc" | "zgc" (None = default to "g1gc")
    #[serde(default)]
    pub gc_preset: Option<String>,
    /// Custom Java path (None = use global default / auto-detect)
    pub java_path: Option<PathBuf>,
    /// Custom game resolution
    pub resolution: Option<Resolution>,
    /// Instance icon (base64 or path)
    pub icon: Option<String>,
    /// Instance banner for the home page card (base64 data URL)
    #[serde(default)]
    pub banner: Option<String>,
    /// When the instance was created
    pub created_at: String,
    /// When the instance was last played
    pub last_played: Option<String>,
    /// Total play time in seconds
    pub play_time_seconds: u64,
    /// Notes / description
    pub notes: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum LoaderType {
    Vanilla,
    Fabric,
    Forge,
    NeoForge,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Instance {
    /// Create a new instance that inherits the launcher's global memory/GC
    /// defaults. Explicit per-instance overrides are written only when the
    /// user changes them in the editor.
    pub fn new(
        name: &str,
        mc_version: &str,
        loader: LoaderType,
        loader_version: Option<String>,
    ) -> Self {
        Self {
            name: name.to_string(),
            mc_version: mc_version.to_string(),
            loader,
            loader_version,
            loader_profile: None,
            // None = inherit the current global default at launch time. Do
            // not copy the value here: a copied value would freeze the old
            // default and make later global changes invisible.
            memory_mb: None,
            jvm_args: None,
            gc_preset: None,
            java_path: None,
            resolution: None,
            icon: None,
            banner: None,
            created_at: Utc::now().to_rfc3339(),
            last_played: None,
            play_time_seconds: 0,
            notes: String::new(),
        }
    }

    /// Effective memory at launch: the explicit per-instance override wins,
    /// otherwise the current global default is inherited. The result is always
    /// within [MIN_MEMORY_MB, global max], so a hand-edited/old instance can
    /// never hand `-Xmx0M` or a multi-terabyte value to the JVM.
    pub fn effective_memory_mb(&self, default_memory_mb: u32, max_memory_mb: u32) -> u32 {
        let ceiling =
            max_memory_mb.clamp(crate::config::MIN_MEMORY_MB, crate::config::MAX_MEMORY_MB);
        let chosen = self.memory_mb.unwrap_or(default_memory_mb);
        let effective = chosen.clamp(crate::config::MIN_MEMORY_MB, ceiling);
        if effective != chosen {
            tracing::warn!(
                target: "launcher",
                "Instance '{}' memory {} MB is outside the allowed range; using {} MB (global limit {} MB)",
                self.name,
                chosen,
                effective,
                ceiling
            );
        }
        effective
    }

    /// Clamp an explicitly stored per-instance value for persistence.
    /// `None` stays `None` (inherit the global default).
    pub fn normalized_memory_mb(&self, max_memory_mb: u32) -> Option<u32> {
        self.memory_mb.map(|value| {
            value.clamp(
                crate::config::MIN_MEMORY_MB,
                max_memory_mb.clamp(crate::config::MIN_MEMORY_MB, crate::config::MAX_MEMORY_MB),
            )
        })
    }

    /// Effective GC preset at launch: explicit override, otherwise the
    /// current global default. Memory and GC inherit independently.
    pub fn effective_gc_preset<'a>(&'a self, default_gc_preset: &'a str) -> &'a str {
        self.gc_preset.as_deref().unwrap_or(default_gc_preset)
    }

    /// Get the instance directory path
    pub fn dir(&self, instances_dir: &Path) -> PathBuf {
        instances_dir.join(&self.name)
    }

    /// Get the .minecraft directory inside the instance
    pub fn minecraft_dir(&self, instances_dir: &Path) -> PathBuf {
        self.dir(instances_dir).join(".minecraft")
    }

    /// Get mods directory
    pub fn mods_dir(&self, instances_dir: &Path) -> PathBuf {
        self.minecraft_dir(instances_dir).join("mods")
    }

    /// Get config file path
    pub fn config_file(&self, instances_dir: &Path) -> PathBuf {
        self.dir(instances_dir).join("instance.json")
    }

    /// Get Prism-compatible instance.cfg path
    pub fn prism_cfg_file(&self, instances_dir: &Path) -> PathBuf {
        self.dir(instances_dir).join("instance.cfg")
    }
}

/// List all instances (supports both instance.json and instance.cfg)
pub fn list_instances(instances_dir: &Path) -> Result<Vec<Instance>> {
    let mut instances = Vec::new();

    if !instances_dir.exists() {
        return Ok(instances);
    }

    for entry in std::fs::read_dir(instances_dir)? {
        let entry = entry?;
        let dir_path = entry.path();
        if !dir_path.is_dir() {
            continue;
        }
        // Restore staging/park directories are dot-prefixed. They may contain
        // a valid instance.json after an interrupted restore, but they are
        // never real instances and must not appear in the picker.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let json_path = dir_path.join("instance.json");
        let cfg_path = dir_path.join("instance.cfg");

        if json_path.exists() {
            match std::fs::read_to_string(&json_path) {
                Ok(contents) => match serde_json::from_str::<Instance>(&contents) {
                    Ok(instance) => instances.push(instance),
                    Err(e) => {
                        tracing::warn!(target: "launcher", "Failed to parse instance at {:?}: {}", json_path, e)
                    }
                },
                Err(e) => {
                    tracing::warn!(target: "launcher", "Failed to read instance at {:?}: {}", json_path, e)
                }
            }
        } else if cfg_path.exists() {
            // Import Prism/MultiMC format on the fly
            if let Some(instance) = parse_prism_cfg(&cfg_path) {
                // Save as instance.json for future fast loading
                let json_path = dir_path.join("instance.json");
                if let Ok(json) = serde_json::to_string_pretty(&instance) {
                    let _ = std::fs::write(&json_path, &json);
                }
                instances.push(instance);
            }
        }
    }

    // Sort by last played, then by name
    instances.sort_by(|a, b| {
        b.last_played
            .as_deref()
            .unwrap_or("")
            .cmp(a.last_played.as_deref().unwrap_or(""))
    });

    Ok(instances)
}

/// One instance plus its transient readiness state for the UI.
///
/// `setup_state` is computed from the on-disk marker on every listing, never
/// persisted into `instance.json`, so a completed setup cannot leave a stale
/// "incomplete" flag behind.
#[derive(Debug, serde::Serialize)]
pub struct InstanceSummary {
    #[serde(flatten)]
    pub instance: Instance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_state: Option<crate::instance_readiness::InstanceSetupState>,
}

/// List instances annotated with their setup/readiness state.
pub fn list_instance_summaries(instances_dir: &Path) -> Result<Vec<InstanceSummary>> {
    let instances = list_instances(instances_dir)?;
    Ok(instances
        .into_iter()
        .map(|instance| {
            let setup_state = crate::instance_readiness::setup_state(&instance.dir(instances_dir));
            InstanceSummary {
                instance,
                setup_state,
            }
        })
        .collect())
}

/// Create a new instance.
///
/// The whole creation runs under an [`crate::import::InstanceReservation`]:
/// the instance is not launchable until its directories and metadata are
/// written and the readiness marker is retired.
pub fn create_instance(
    instances_dir: &Path,
    instance: &Instance,
    launches: &crate::launch_state::LaunchRegistry,
) -> Result<()> {
    validate_instance_dir_name(&instance.name, "Instance name")?;
    let mut reservation = crate::import::InstanceReservation::reserve(
        instances_dir,
        &instance.name,
        launches,
        "create",
    )?;

    // Create directory structure
    std::fs::create_dir_all(instance.minecraft_dir(instances_dir))?;
    std::fs::create_dir_all(instance.mods_dir(instances_dir))?;
    std::fs::create_dir_all(instance.minecraft_dir(instances_dir).join("resourcepacks"))?;
    std::fs::create_dir_all(instance.minecraft_dir(instances_dir).join("shaderpacks"))?;
    std::fs::create_dir_all(instance.minecraft_dir(instances_dir).join("config"))?;

    // The reservation already holds the name, so write without acquiring it
    // again.
    save_instance_reserved(instances_dir, instance, None)?;

    // Metadata exists; nothing else is needed for a fresh vanilla instance.
    reservation.commit()
}

/// Save instance config to disk, plus Prism-compatible instance.cfg and pack.png.
/// If `old_name` differs from `instance.name`, the instance directory is renamed first.
///
/// This is the public entry point: it validates both names and reserves the
/// source and target before touching the filesystem.
pub fn save_instance(
    instances_dir: &Path,
    instance: &Instance,
    old_name: Option<&str>,
) -> Result<()> {
    validate_instance_dir_name(&instance.name, "Instance name")?;
    let mut names = vec![instance.name.as_str()];
    if let Some(prev) = old_name {
        validate_instance_dir_name(prev, "Instance name")?;
        names.push(prev);
    }
    let _reservation = InstanceNameReservation::acquire(instances_dir, &names)?;
    save_instance_reserved(instances_dir, instance, old_name)
}

/// Write instance metadata and move the directory if needed. The caller must
/// already hold [`InstanceNameReservation`] for the old and new names.
pub(crate) fn save_instance_reserved(
    instances_dir: &Path,
    instance: &Instance,
    old_name: Option<&str>,
) -> Result<()> {
    validate_instance_dir_name(&instance.name, "Instance name")?;
    if let Some(prev) = old_name {
        validate_instance_dir_name(prev, "Instance name")?;
    }

    // Rename directory if name changed
    if let Some(prev) = old_name {
        if prev != instance.name {
            let old_dir = instances_dir.join(prev);
            let new_dir = instances_dir.join(&instance.name);
            if !old_dir.exists() {
                return Err(LauncherError::Instance(format!(
                    "Instance '{}' no longer exists",
                    prev
                )));
            }
            move_directory_safely(&old_dir, &new_dir, &instance.name)?;
        }
    }

    let config_path = instance.config_file(instances_dir);
    let config_dir = config_path
        .parent()
        .ok_or_else(|| LauncherError::Instance("Instance config has no parent directory".into()))?;
    if !config_dir.exists() {
        std::fs::create_dir_all(config_dir)?;
    }
    let json = {
        // Persist only values the JVM can safely receive. The global limit is
        // applied again in cmd_save_instance/effective_memory_mb; this absolute
        // clamp also covers imports and legacy callers.
        let mut stored = instance.clone();
        stored.memory_mb = stored
            .memory_mb
            .map(|value| value.clamp(crate::config::MIN_MEMORY_MB, crate::config::MAX_MEMORY_MB));
        stored.gc_preset = stored.gc_preset.and_then(|value| {
            let normalized = value.trim().to_lowercase();
            matches!(normalized.as_str(), "standard" | "g1gc" | "zgc").then_some(normalized)
        });
        serde_json::to_string_pretty(&stored)?
    };
    write_file_atomic(&config_path, json.as_bytes())?;

    // Write Prism-compatible instance.cfg
    write_prism_cfg(instance, instances_dir)?;

    // Write Prism-compatible mmc-pack.json
    write_mmc_pack_json(instance, instances_dir)?;

    // Write pack.png if instance has an icon
    if let Some(ref icon) = instance.icon {
        write_pack_png(instances_dir, &instance.name, icon);
    }

    Ok(())
}

/// Save metadata for an instance that must already exist. This is the variant
/// for long-running operations (loader install, post-import preparation): if
/// the directory disappeared while the operation was working, it must fail
/// instead of recreating a ghost instance with only a config file.
pub(crate) fn save_instance_existing_reserved(
    instances_dir: &Path,
    instance: &Instance,
    old_name: Option<&str>,
) -> Result<()> {
    validate_instance_dir_name(&instance.name, "Instance name")?;
    if let Some(prev) = old_name {
        validate_instance_dir_name(prev, "Instance name")?;
    }
    let will_move = old_name.is_some_and(|prev| prev != instance.name);
    if !will_move && !instances_dir.join(&instance.name).exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' no longer exists",
            instance.name
        )));
    }
    save_instance_reserved(instances_dir, instance, old_name)
}

/// Public entry point for editing an existing instance without recreating it.
pub fn save_instance_existing(
    instances_dir: &Path,
    instance: &Instance,
    old_name: Option<&str>,
) -> Result<()> {
    validate_instance_dir_name(&instance.name, "Instance name")?;
    let mut names = vec![instance.name.as_str()];
    if let Some(prev) = old_name {
        validate_instance_dir_name(prev, "Instance name")?;
        names.push(prev);
    }
    let _reservation = InstanceNameReservation::acquire(instances_dir, &names)?;
    save_instance_existing_reserved(instances_dir, instance, old_name)
}

/// Write a file atomically: the contents go to a sibling temp file first,
/// which is then renamed over the target. A crash or power loss mid-save
/// can never leave a truncated/half-written config behind — the old file
/// stays intact until the final rename.
///
/// If the direct rename is blocked (Windows sharing violation), a `.bak`
/// copy is made while the original stays in place; the new file is swapped
/// in and the backup is removed only after that succeeds. A concurrent
/// reader therefore never observes a missing file, and a failed swap leaves
/// the original untouched.
pub(crate) fn write_file_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| LauncherError::Instance("File path has no parent directory".into()))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| LauncherError::Instance("File path has no file name".into()))?;
    let tmp_path = parent.join(format!(".{}.tmp", file_name));
    let bak_path = parent.join(format!(".{}.bak", file_name));

    std::fs::write(&tmp_path, contents)?;
    // Flush the temp file before the rename so a crash cannot leave a
    // renamed-but-empty file on a delayed-write filesystem.
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&tmp_path) {
        let _ = f.sync_all();
    }

    match std::fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(first_err) => {
            if !path.exists() {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(first_err.into());
            }
            // Keep the original in place while the replacement is prepared:
            // a concurrent reader must never observe the file missing. A copy
            // (not a rename) also leaves the live file usable if the second
            // rename still fails.
            let _ = std::fs::remove_file(&bak_path);
            if let Err(e) = std::fs::copy(path, &bak_path) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e.into());
            }
            match std::fs::rename(&tmp_path, path) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&bak_path);
                    Ok(())
                }
                Err(e) => {
                    // The original was never moved, so it is still intact.
                    let _ = std::fs::remove_file(&tmp_path);
                    let _ = std::fs::remove_file(&bak_path);
                    Err(e.into())
                }
            }
        }
    }
}

/// Delete an instance
pub fn delete_instance(instances_dir: &Path, name: &str) -> Result<()> {
    validate_instance_dir_name(name, "Instance name")?;
    let _reservation = InstanceNameReservation::acquire(instances_dir, &[name])?;
    let dir = instances_dir.join(name);
    if !dir.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

/// Get an instance by name (supports both instance.json and instance.cfg)
pub fn get_instance(instances_dir: &Path, name: &str) -> Result<Instance> {
    validate_instance_dir_name(name, "Instance name")?;
    let dir = instances_dir.join(name);
    let json_path = dir.join("instance.json");
    let cfg_path = dir.join("instance.cfg");

    if json_path.exists() {
        let contents = std::fs::read_to_string(&json_path)?;
        let instance = serde_json::from_str(&contents)?;
        return Ok(instance);
    }

    if cfg_path.exists() {
        if let Some(instance) = parse_prism_cfg(&cfg_path) {
            // Save as instance.json for future fast loading
            if let Ok(json) = serde_json::to_string_pretty(&instance) {
                let _ = std::fs::write(&json_path, &json);
            }
            return Ok(instance);
        }
    }

    Err(LauncherError::Instance(format!(
        "Instance '{}' not found",
        name
    )))
}

/// What the user chose to leave out of a duplicate (roadmap: duplicate must
/// show what it copies and let saves / screenshots / logs be excluded).
#[derive(Debug, Clone, Copy, Default)]
pub struct DuplicateExclusions {
    pub saves: bool,
    pub screenshots: bool,
    /// Per-launch junk: `.minecraft/logs` and `.minecraft/crash-reports`.
    pub logs: bool,
}

/// Duplicate an instance. Metadata, mods, configs, resource packs and
/// shaders always travel to the copy; the source's backup history (`.backups`)
/// and parked update stashes never do — they belong to the original, and a
/// copy inheriting its backup list would offer restic restores of the wrong
/// instance. `exclude` drops the user's optional picks (saves, screenshots,
/// logs with crash reports).
pub fn duplicate_instance(
    instances_dir: &Path,
    name: &str,
    new_name: &str,
    exclude: DuplicateExclusions,
    launches: &crate::launch_state::LaunchRegistry,
) -> Result<Instance> {
    validate_instance_dir_name(name, "Instance name")?;
    validate_instance_dir_name(new_name, "Instance name")?;
    // Keep the source stable while it is copied, and publish the copy through
    // the same readiness lifecycle as every other creation path: the target
    // carries a pending marker until the copied metadata is rewritten.
    let _source_guard = InstanceNameReservation::acquire(instances_dir, &[name])?;
    let src = instances_dir.join(name);
    if !src.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    if crate::instance_readiness::read_pending(&src).is_some() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' is not fully set up yet. Finish its setup before duplicating it.",
            name
        )));
    }
    let mut reservation = crate::import::InstanceReservation::reserve(
        instances_dir,
        new_name,
        launches,
        "duplicate",
    )?;
    let dst = instances_dir.join(new_name);
    copy_dir_filtered(&src, &dst, "", &|rel| duplicate_skip(rel, exclude))?;
    // Read and update the instance config.
    let mut instance = match get_instance(instances_dir, new_name) {
        Ok(instance) => instance,
        Err(e) => {
            return Err(e);
        }
    };
    instance.name = new_name.to_string();
    instance.last_played = None;
    instance.play_time_seconds = 0;
    instance.created_at = Utc::now().to_rfc3339();
    save_instance_reserved(instances_dir, &instance, None)?;
    reservation.commit()?;
    Ok(instance)
}

/// Paths (relative to the instance root, `/`-separated) a duplicate leaves
/// behind. Directories matched here are not descended into at all.
fn duplicate_skip(rel: &str, exclude: DuplicateExclusions) -> bool {
    // Readiness/ownership markers belong to one directory only; a copy must
    // never inherit them.
    if rel == crate::instance_readiness::PENDING_MARKER || rel == ".void-import-owner" {
        return true;
    }
    // Backup history and parked stashes always stay with the source.
    if rel == ".backups" || rel.starts_with(".backups/") {
        return true;
    }
    if rel.ends_with(".voidlauncher-old") {
        return true;
    }
    let mc = rel.strip_prefix(".minecraft/").unwrap_or("");
    if mc.is_empty() && rel != ".minecraft" {
        // Top-level entry (metadata, icon, …) — never excluded.
        return false;
    }
    let dir = mc.split('/').next().unwrap_or("");
    match dir {
        "saves" => exclude.saves,
        "screenshots" => exclude.screenshots,
        "logs" | "crash-reports" => exclude.logs,
        _ => false,
    }
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    copy_dir_filtered(src, dst, "", &|_| false)
}

/// Hidden sibling used for an atomic copy/rename. A UUID keeps concurrent
/// operations from sharing a staging path.
fn unique_instance_sibling(target: &Path, tag: &str) -> Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| LauncherError::Instance("Instance path has no parent".into()))?;
    Ok(parent.join(format!(
        ".voidlauncher-{}-{}.tmp",
        tag,
        uuid::Uuid::new_v4().simple()
    )))
}

/// Copy an instance tree into a hidden staging directory, then rename it into
/// place. A pre-existing target is never merged into or overwritten: the
/// final rename fails and the staging copy is removed, leaving the source
/// untouched.
fn copy_tree_to_new_path(src: &Path, dst: &Path, skip: &dyn Fn(&str) -> bool) -> Result<()> {
    if dst.exists() {
        return Err(LauncherError::Instance(format!(
            "An instance named '{}' already exists",
            dst.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
        )));
    }
    let staging = unique_instance_sibling(dst, "copy")?;
    if let Err(e) = copy_dir_filtered(src, &staging, "", skip) {
        let _ = remove_dir_all_force(&staging);
        return Err(e);
    }
    match std::fs::rename(&staging, dst) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = remove_dir_all_force(&staging);
            Err(LauncherError::Instance(format!(
                "Could not install the copied instance at '{}': {}. The source instance was not changed.",
                dst.display(),
                e
            )))
        }
    }
}

/// Rename a directory while never merging into an existing target. If the
/// direct rename is blocked on Windows, copy to a hidden sibling, rename that
/// into place, and only then remove the original. A failed copy leaves the
/// original untouched.
fn move_directory_safely(old_dir: &Path, new_dir: &Path, instance_name: &str) -> Result<()> {
    move_directory_with(old_dir, new_dir, instance_name, &|from, to| {
        std::fs::rename(from, to)
    })
}

fn move_directory_with(
    old_dir: &Path,
    new_dir: &Path,
    instance_name: &str,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    if new_dir.exists() {
        // A case-only rename resolves to the same Windows directory; anything
        // else would merge two instances. Never overwrite the other one.
        let same_dir = old_dir
            .canonicalize()
            .ok()
            .zip(new_dir.canonicalize().ok())
            .is_some_and(|(old, new)| old == new);
        if !same_dir {
            return Err(LauncherError::Instance(format!(
                "An instance named '{}' already exists",
                instance_name
            )));
        }
        if old_dir.file_name() == new_dir.file_name() {
            return Ok(());
        }
        return rename_case_only(old_dir, new_dir);
    }

    match rename(old_dir, new_dir) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            tracing::warn!(
                target: "launcher",
                "fs::rename failed ({}), falling back to copy+delete",
                rename_err
            );
            copy_tree_to_new_path(old_dir, new_dir, &|_| false)?;
            if let Err(e) = remove_dir_all_force(old_dir) {
                return Err(LauncherError::Instance(format!(
                    "Instance '{}' was copied to '{}' but the original could not be removed: {}. \
                     Both copies are intact; close the game or Explorer windows and remove '{}' manually.",
                    instance_name,
                    new_dir.display(),
                    e,
                    old_dir.display()
                )));
            }
            Ok(())
        }
    }
}

/// Case-only rename through a hidden intermediate name so a failure can be
/// rolled back. If the rollback itself fails, the instance is preserved at
/// the reported staging path.
fn rename_case_only(old_dir: &Path, new_dir: &Path) -> Result<()> {
    let staging = unique_instance_sibling(old_dir, "rename")?;
    std::fs::rename(old_dir, &staging).map_err(|error| {
        LauncherError::Instance(format!(
            "Could not rename instance directory '{}': {}",
            old_dir.display(),
            error
        ))
    })?;
    match std::fs::rename(&staging, new_dir) {
        Ok(()) => Ok(()),
        Err(error) => match std::fs::rename(&staging, old_dir) {
            Ok(()) => Err(LauncherError::Instance(format!(
                "Could not rename instance directory '{}': {}",
                old_dir.display(),
                error
            ))),
            Err(rollback) => Err(LauncherError::Instance(format!(
                "Could not rename instance directory '{}': {}; restoring the original name also failed ({}). \
                 The instance is preserved at '{}'.",
                old_dir.display(),
                error,
                rollback,
                staging.display()
            ))),
        },
    }
}

/// Recursive copy that asks `skip` about each entry's path relative to the
/// copy root (`/`-separated). A skipped directory is not descended into.
fn copy_dir_filtered(
    src: &std::path::Path,
    dst: &std::path::Path,
    rel_prefix: &str,
    skip: &dyn Fn(&str) -> bool,
) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = if rel_prefix.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", rel_prefix, name)
        };
        if skip(&rel) {
            continue;
        }
        let dst_path = dst.join(&name);
        if path.is_dir() {
            copy_dir_filtered(&path, &dst_path, &rel, skip)?;
        } else {
            std::fs::copy(&path, &dst_path)?;
        }
    }
    Ok(())
}

/// Save instance icon (base64 data URL)
pub fn save_instance_icon(instances_dir: &Path, name: &str, icon_data: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.icon = Some(icon_data.to_string());
    save_instance(instances_dir, &instance, None)?;
    Ok(())
}

/// Save instance banner (base64 data URL or gradient:name, empty to remove)
pub fn save_instance_banner(instances_dir: &Path, name: &str, banner_data: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.banner = if banner_data.is_empty() {
        None
    } else {
        Some(banner_data.to_string())
    };
    save_instance(instances_dir, &instance, None)?;
    Ok(())
}

/// World folder sizes are expensive to walk (region files can be thousands per
/// world), and the worlds tab is reopened often. Cache them briefly; the stamp
/// still invalidates immediately when the world folder or its key metadata
/// changes, so a newly saved world does not keep an old size for long.
const WORLD_SIZE_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
struct CachedWorldSize {
    stamp: u64,
    size: u64,
    cached_at: Instant,
}

static WORLD_SIZE_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedWorldSize>>> = OnceLock::new();

fn hash_path_metadata(hasher: &mut std::collections::hash_map::DefaultHasher, path: &Path) {
    use std::hash::Hash;
    path.hash(hasher);
    match std::fs::metadata(path) {
        Ok(meta) => {
            meta.len().hash(hasher);
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            modified.hash(hasher);
        }
        Err(_) => 0u8.hash(hasher),
    }
}

fn world_size_stamp(world: &Path, last_modified: Option<i64>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    world.hash(&mut hasher);
    last_modified.hash(&mut hasher);
    hash_path_metadata(&mut hasher, world);
    for name in ["level.dat", "level.dat_old", "session.lock", "region"] {
        hash_path_metadata(&mut hasher, &world.join(name));
    }
    hasher.finish()
}

fn cached_world_size(world: &Path, last_modified: Option<i64>) -> u64 {
    let stamp = world_size_stamp(world, last_modified);
    let cache = WORLD_SIZE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(entry) = guard.get(world) {
            if entry.stamp == stamp && entry.cached_at.elapsed() < WORLD_SIZE_CACHE_TTL {
                return entry.size;
            }
        }
    }
    let size = dir_size(world);
    if let Ok(mut guard) = cache.lock() {
        guard.retain(|_, entry| entry.cached_at.elapsed() < WORLD_SIZE_CACHE_TTL);
        guard.insert(
            world.to_path_buf(),
            CachedWorldSize {
                stamp,
                size,
                cached_at: Instant::now(),
            },
        );
    }
    size
}

/// List saves (worlds)
pub fn list_saves(instances_dir: &Path, name: &str) -> Result<Vec<SaveEntry>> {
    let instance = get_instance(instances_dir, name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let mut saves = Vec::new();
    if !saves_dir.exists() {
        return Ok(saves);
    }
    for entry in std::fs::read_dir(&saves_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let path = entry.path();
            let world_name = entry.file_name().to_string_lossy().to_string();
            let last_modified = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64
                });
            let level = parse_level_dat(&path.join("level.dat"));
            let game_mode = level.game_type.map(|gt| match gt {
                0 => "Survival".to_string(),
                1 => "Creative".to_string(),
                2 => "Adventure".to_string(),
                3 => "Hardcore".to_string(),
                _ => format!("Type {gt}"),
            });
            let icon_data = read_world_icon(instances_dir, name, &world_name);
            saves.push(SaveEntry {
                name: world_name,
                last_modified,
                size_bytes: cached_world_size(&path, last_modified),
                game_mode,
                seed: level.seed,
                icon_data,
            });
        }
    }
    saves.sort_by(|a, b| {
        b.last_modified
            .unwrap_or(0)
            .cmp(&a.last_modified.unwrap_or(0))
    });
    Ok(saves)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SaveEntry {
    pub name: String,
    pub last_modified: Option<i64>,
    pub size_bytes: u64,
    #[serde(default)]
    pub game_mode: Option<String>,
    #[serde(default)]
    pub seed: Option<i64>,
    #[serde(default)]
    pub icon_data: Option<String>,
}

/// List screenshots
pub fn list_screenshots(instances_dir: &Path, name: &str) -> Result<Vec<ScreenshotEntry>> {
    let instance = get_instance(instances_dir, name)?;
    let dir = instance.minecraft_dir(instances_dir).join("screenshots");
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext == "png" || ext == "jpg" || ext == "jpeg" {
            let meta = std::fs::metadata(&path)?;
            out.push(ScreenshotEntry {
                filename: entry.file_name().to_string_lossy().to_string(),
                last_modified: meta.modified().ok().map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64
                }),
                size_bytes: meta.len(),
            });
        }
    }
    out.sort_by(|a, b| {
        b.last_modified
            .unwrap_or(0)
            .cmp(&a.last_modified.unwrap_or(0))
    });
    Ok(out)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScreenshotEntry {
    pub filename: String,
    pub last_modified: Option<i64>,
    pub size_bytes: u64,
}

pub fn read_screenshot(
    instances_dir: &Path,
    instance_name: &str,
    filename: &str,
) -> Result<String> {
    let instance = get_instance(instances_dir, instance_name)?;
    let path = instance
        .minecraft_dir(instances_dir)
        .join("screenshots")
        .join(filename);
    let buf = std::fs::read(&path)
        .map_err(|e| LauncherError::Instance(format!("Cannot read {}: {}", path.display(), e)))?;
    if buf.is_empty() {
        return Err(LauncherError::Instance(format!(
            "Screenshot is empty: {}",
            path.display()
        )));
    }
    // Screenshots may be PNG or JPG depending on the in-game setting; sniff the
    // MIME from the file extension so the browser decodes the data URL correctly.
    let mime = match Path::new(filename).extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") => {
            "image/jpeg"
        }
        Some(ext) if ext.eq_ignore_ascii_case("png") => "image/png",
        _ => "image/png",
    };
    Ok(format!("data:{mime};base64,{}", base64_encode(&buf)))
}

pub fn delete_screenshot(instances_dir: &Path, instance_name: &str, filename: &str) -> Result<()> {
    // The filename reaches the filesystem through the command layer; strip
    // any path components so a crafted name cannot escape the screenshots dir.
    let safe_filename = Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| LauncherError::Instance("Invalid screenshot filename".into()))?;
    if safe_filename.is_empty() || safe_filename == "." || safe_filename == ".." {
        return Err(LauncherError::Instance(
            "Invalid screenshot filename".into(),
        ));
    }
    let instance = get_instance(instances_dir, instance_name)?;
    let path = instance
        .minecraft_dir(instances_dir)
        .join("screenshots")
        .join(safe_filename);
    if !path.exists() {
        return Err(LauncherError::Instance(format!(
            "Screenshot not found: {}",
            safe_filename
        )));
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

/// List resource packs or shader packs (returns entries without icon - fetch via cmd_get_pack_icon)
pub fn list_packs(instances_dir: &Path, name: &str, pack_type: &str) -> Result<Vec<PackEntry>> {
    let instance = get_instance(instances_dir, name)?;
    let mc_dir = instance.minecraft_dir(instances_dir);
    let packs_dir = mc_dir.join(pack_type);
    let mut packs = Vec::new();
    if !packs_dir.exists() {
        return Ok(packs);
    }
    // Roll back any file parked by an interrupted update before listing.
    recover_stashed_content_files(&packs_dir);

    // Auto-migration for resource packs: un-rename any *.disabled back to *.zip
    // without activating them in options.txt, preserving disabled state cleanly.
    if pack_type == "resourcepacks" {
        if let Ok(entries) = std::fs::read_dir(&packs_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let fname = entry.file_name().to_string_lossy().to_string();
                if let Some(clean) = fname.strip_suffix(".disabled") {
                    let target = packs_dir.join(clean);
                    if !target.exists() {
                        let _ = std::fs::rename(&p, target);
                    }
                }
            }
        }
    }

    let active_resource_packs = if pack_type == "resourcepacks" {
        let options_path = mc_dir.join("options.txt");
        read_options_resource_packs(&options_path).0
    } else {
        Vec::new()
    };

    // Load Prism metadata once for the whole listing (was re-read per file).
    let packwiz = load_packwiz_index_cached(&packs_dir);
    for entry in std::fs::read_dir(&packs_dir)? {
        let entry = entry?;
        let path = entry.path();
        let filename = entry.file_name().to_string_lossy().to_string();
        // Skip hidden files (incl. Prism `.index/`), sidecar metadata and
        // Prism/packwiz `.pw.toml` metadata files
        if filename.starts_with('.')
            || filename.ends_with(".voidlauncher.json")
            || filename.ends_with(".pw.toml")
        {
            continue;
        }
        let is_dir = path.is_dir();
        let meta = std::fs::metadata(&path)?;

        // Skip tiny non-zip config presets in shaderpacks (e.g. "Better MC - High.json" at 373 bytes)
        if pack_type == "shaderpacks" && !is_dir && meta.len() < 1024 {
            let lower = filename.to_lowercase();
            if lower.ends_with(".json")
                || lower.ends_with(".txt")
                || lower.ends_with(".cfg")
                || lower.ends_with(".properties")
                || lower.ends_with(".toml")
            {
                continue;
            }
        }

        // Read sidecar metadata (project_name is the Modrinth/CurseForge display name).
        // The version comes only from that metadata — never from the file name
        // or pack.mcmeta's pack_format, which are not the pack's own version.
        let (provider, version, project_id, project_name) =
            read_pack_sidecar(&path, &packwiz).unwrap_or_default();

        let (explicit_name, pack_desc) = if is_dir {
            read_pack_meta_from_dir(&path)
        } else {
            read_pack_meta_from_zip(&path)
        };

        // Name resolution priority:
        // 1. Sidecar project_name (Modrinth/CurseForge display name)
        // 2. Explicit pack.name in pack.mcmeta (if provided)
        // 3. Clean filename stem without .zip/.disabled
        // Note: pack.description is NEVER used as the pack's name.
        let name = if !project_name.is_empty() {
            project_name
        } else if let Some(n) = explicit_name {
            n
        } else {
            clean_pack_stem_for_mc(&filename, &instance.mc_version)
        };

        let description = pack_desc.unwrap_or_default();

        let enabled = if pack_type == "resourcepacks" {
            is_pack_enabled_in_list(&filename, &active_resource_packs)
        } else {
            !filename.ends_with(".disabled")
        };

        packs.push(PackEntry {
            filename,
            name,
            is_dir,
            file_size: meta.len(),
            provider,
            version,
            project_id,
            description,
            enabled,
        });
    }
    if pack_type == "resourcepacks" {
        // options.txt stores resource packs from lowest to highest priority;
        // the game UI (and this list) shows the highest priority first.
        packs.sort_by(|a, b| {
            let pa = active_resource_pack_priority(&a.filename, &active_resource_packs);
            let pb = active_resource_pack_priority(&b.filename, &active_resource_packs);
            match (pa, pb) {
                (Some(pa), Some(pb)) => pb.cmp(&pa).then_with(|| a.name.cmp(&b.name)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            }
        });
    } else {
        packs.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(packs)
}

/// Position of `filename` in the active resource-pack list (0 = lowest
/// priority). Returns `None` for a pack that is not active.
fn active_resource_pack_priority(filename: &str, active: &[String]) -> Option<usize> {
    let clean = filename.strip_suffix(".disabled").unwrap_or(filename);
    active.iter().position(|id| {
        let id = id.strip_prefix("file/").unwrap_or(id);
        id == clean
    })
}

/// Write file contents atomically with retry and safe rollback on Windows.
/// Ensures the target file is never left truncated or missing if replacement fails.
fn atomic_write_file_impl<R>(target: &Path, content: &str, mut rename_fn: R) -> std::io::Result<()>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("tmp");
    std::fs::write(&tmp, content)?;

    let mut last_err = None;
    // 1. Direct atomic replace
    for _ in 0..5 {
        match rename_fn(&tmp, target) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }

    // 2. Safe fallback:
    // If target exists, move target -> old, move tmp -> target, then delete old.
    // If moving tmp -> target fails, immediately restore old -> target so original is untouched.
    if target.exists() {
        let old = target.with_extension("old");
        let _ = std::fs::remove_file(&old);
        if rename_fn(target, &old).is_ok() {
            if rename_fn(&tmp, target).is_ok() {
                let _ = std::fs::remove_file(&old);
                return Ok(());
            } else {
                let _ = rename_fn(&old, target);
                let _ = std::fs::remove_file(&tmp);
                return Err(last_err
                    .unwrap_or_else(|| std::io::Error::other("Failed to replace target file")));
            }
        }
    }

    let _ = std::fs::remove_file(&tmp);
    Err(last_err.unwrap_or_else(|| std::io::Error::other("Failed to atomically write file")))
}

pub(crate) fn atomic_write_file(target: &Path, content: &str) -> std::io::Result<()> {
    atomic_write_file_impl(target, content, |from, to| std::fs::rename(from, to))
}

/// Read active resource packs from options.txt.
/// Returns (active_pack_identifiers, line_ending).
pub fn read_options_resource_packs(path: &Path) -> (Vec<String>, String) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return (Vec::new(), "\n".to_string());
    };
    let line_ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
    .to_string();
    for line in content.lines() {
        if let Some(val_str) = line.strip_prefix("resourcePacks:") {
            if let Ok(packs) = serde_json::from_str::<Vec<String>>(val_str.trim()) {
                return (packs, line_ending);
            }
        }
    }
    (Vec::new(), line_ending)
}

/// Check if a pack filename is active in the given list of identifiers.
pub fn is_pack_enabled_in_list(filename: &str, active_packs: &[String]) -> bool {
    let clean_target = filename.strip_suffix(".disabled").unwrap_or(filename);
    active_packs.iter().any(|id| {
        if id == clean_target {
            return true;
        }
        if let Some(rest) = id.strip_prefix("file/") {
            if rest == clean_target {
                return true;
            }
        }
        false
    })
}

/// Read options.txt while distinguishing "missing" from a real read error.
///
/// A temporary ACL/AV/encoding failure must never be treated as an empty file:
/// the resource-pack toggles rewrite the whole file and would otherwise erase
/// every keybind, graphics and audio setting.
fn read_options_file(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LauncherError::Instance(format!(
            "Failed to read options.txt: {error}"
        ))),
    }
}

/// Modify options.txt to enable or disable a resource pack.
pub fn set_options_resource_pack_enabled(path: &Path, filename: &str, enabled: bool) -> Result<()> {
    let clean_target = filename.strip_suffix(".disabled").unwrap_or(filename);
    if clean_target.is_empty() || clean_target.contains(['/', '\\']) {
        return Err(LauncherError::Instance(
            "Invalid resource pack filename".to_string(),
        ));
    }
    let content = read_options_file(path)?.unwrap_or_default();
    let line_ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut lines: Vec<String> = if content.is_empty() {
        Vec::new()
    } else {
        content.lines().map(|s| s.to_string()).collect()
    };

    let mut pack_line_idx = None;
    let mut current_packs: Vec<String> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        if let Some(val_str) = line.strip_prefix("resourcePacks:") {
            pack_line_idx = Some(idx);
            match serde_json::from_str::<Vec<String>>(val_str.trim()) {
                Ok(packs) => current_packs = packs,
                Err(error) => {
                    return Err(LauncherError::Instance(format!(
                        "Cannot parse resourcePacks in options.txt: {error}"
                    )))
                }
            }
            break;
        }
    }

    if current_packs.is_empty() && pack_line_idx.is_none() {
        current_packs.push("vanilla".to_string());
    }

    if enabled {
        let is_already_enabled = current_packs.iter().any(|id| {
            id == clean_target || id.strip_prefix("file/").is_some_and(|s| s == clean_target)
        });
        if !is_already_enabled {
            current_packs.push(format!("file/{}", clean_target));
        }
    } else {
        current_packs.retain(|id| {
            let is_match =
                id == clean_target || id.strip_prefix("file/").is_some_and(|s| s == clean_target);
            !is_match
        });
    }

    let json_packs = serde_json::to_string(&current_packs).map_err(|e| {
        LauncherError::Instance(format!("Failed to serialize resource packs: {}", e))
    })?;
    let new_pack_line = format!("resourcePacks:{}", json_packs);

    if let Some(idx) = pack_line_idx {
        lines[idx] = new_pack_line;
    } else {
        lines.push(new_pack_line);
    }

    let mut new_content = lines.join(line_ending);
    new_content.push_str(line_ending);

    if path.exists() {
        let bak_path = path.with_extension("txt.bak");
        let _ = std::fs::copy(path, bak_path);
    }

    atomic_write_file(path, &new_content)
        .map_err(|e| LauncherError::Instance(format!("Failed to write options.txt: {}", e)))
}

/// Reorder the active resource packs in options.txt.
///
/// `filenames` follows the on-disk contract: lowest priority first, highest
/// priority last (the UI shows the reverse order and reverses it before
/// calling). Only packs that are already active are touched: a name that is
/// currently disabled is ignored instead of being silently enabled. Non-file
/// system packs (`vanilla`, `mod_resources`, ...) keep their order.
pub fn set_options_resource_packs_order(path: &Path, filenames: &[String]) -> Result<()> {
    let Some(content) = read_options_file(path)? else {
        // Reordering must not invent options.txt.
        return Ok(());
    };
    let line_ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    let mut lines: Vec<String> = if content.is_empty() {
        Vec::new()
    } else {
        content.lines().map(|s| s.to_string()).collect()
    };

    let mut pack_line_idx = None;
    let mut current_packs: Vec<String> = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        if let Some(val_str) = line.strip_prefix("resourcePacks:") {
            pack_line_idx = Some(idx);
            match serde_json::from_str::<Vec<String>>(val_str.trim()) {
                Ok(packs) => current_packs = packs,
                Err(error) => {
                    return Err(LauncherError::Instance(format!(
                        "Cannot parse resourcePacks in options.txt: {error}"
                    )))
                }
            }
            break;
        }
    }

    // Nothing to reorder and no line to preserve: never invent an empty
    // resource-pack list for an instance whose options.txt is absent.
    if pack_line_idx.is_none() && current_packs.is_empty() {
        return Ok(());
    }

    let mut system_packs = Vec::new();
    let mut active_files: Vec<(String, String)> = Vec::new();
    for id in current_packs {
        if let Some(clean) = id.strip_prefix("file/") {
            active_files.push((clean.to_string(), id));
        } else {
            system_packs.push(id);
        }
    }

    let mut ordered_files = Vec::new();
    let mut used = vec![false; active_files.len()];
    for filename in filenames {
        let clean = filename.strip_suffix(".disabled").unwrap_or(filename);
        if clean.is_empty() || clean.contains(['/', '\\']) {
            return Err(LauncherError::Instance(
                "Invalid resource pack filename".to_string(),
            ));
        }
        if let Some(pos) = active_files.iter().position(|(name, _)| name == clean) {
            if !used[pos] {
                used[pos] = true;
                ordered_files.push(active_files[pos].1.clone());
            }
        }
    }
    // An active pack omitted from the request is kept: the command reorders,
    // it does not enable or disable packs as a side effect.
    for (pos, (_, id)) in active_files.iter().enumerate() {
        if !used[pos] {
            ordered_files.push(id.clone());
        }
    }

    let mut new_packs = system_packs;
    new_packs.extend(ordered_files);

    let json_packs = serde_json::to_string(&new_packs).map_err(|e| {
        LauncherError::Instance(format!("Failed to serialize resource packs: {}", e))
    })?;
    let new_pack_line = format!("resourcePacks:{}", json_packs);

    if let Some(idx) = pack_line_idx {
        lines[idx] = new_pack_line;
    } else {
        lines.push(new_pack_line);
    }

    let mut new_content = lines.join(line_ending);
    new_content.push_str(line_ending);

    if path.exists() {
        let bak_path = path.with_extension("txt.bak");
        let _ = std::fs::copy(path, bak_path);
    }

    atomic_write_file(path, &new_content)
        .map_err(|e| LauncherError::Instance(format!("Failed to write options.txt: {}", e)))
}

pub fn set_resource_pack_enabled(
    instances_dir: &Path,
    instance_name: &str,
    filename: &str,
    enabled: bool,
) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let options_path = instance.minecraft_dir(instances_dir).join("options.txt");
    set_options_resource_pack_enabled(&options_path, filename, enabled)
}

pub fn set_resource_packs_order(
    instances_dir: &Path,
    instance_name: &str,
    filenames: &[String],
) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let options_path = instance.minecraft_dir(instances_dir).join("options.txt");
    set_options_resource_packs_order(&options_path, filenames)
}

/// Read the icon for a single pack (mirrors mod icon approach)
pub fn read_pack_icon(
    instances_dir: &Path,
    instance_name: &str,
    pack_type: &str,
    filename: &str,
) -> Result<Option<String>> {
    let instance = get_instance(instances_dir, instance_name)?;
    let pack_path = instance
        .minecraft_dir(instances_dir)
        .join(pack_type)
        .join(filename);
    if !pack_path.exists() {
        return Ok(None);
    }
    if pack_path.is_dir() {
        Ok(read_pack_icon_from_dir(&pack_path))
    } else {
        Ok(read_pack_icon_from_zip(&pack_path))
    }
}

/// Stem of a content filename: strip `.disabled`, then `.jar`/`.zip`.
pub(crate) fn content_stem(filename: &str) -> &str {
    let s = filename.strip_suffix(".disabled").unwrap_or(filename);
    s.strip_suffix(".jar")
        .or_else(|| s.strip_suffix(".zip"))
        .unwrap_or(s)
}

/// Sidecar metadata path (new layout): a single hidden `.index/` folder
/// inside the content directory — same layout as Prism Launcher — instead
/// of a metadata file next to each mod/pack.
pub(crate) fn sidecar_meta_path(content_dir: &std::path::Path, filename: &str) -> PathBuf {
    content_dir
        .join(".index")
        .join(format!("{}.voidlauncher.json", content_stem(filename)))
}

/// Legacy sidecar path (old layout): a metadata file next to the content file.
pub(crate) fn legacy_sidecar_path(content_dir: &std::path::Path, filename: &str) -> PathBuf {
    content_dir.join(format!("{}.voidlauncher.json", content_stem(filename)))
}

/// Read sidecar metadata: new `.index/` layout first, legacy layout as fallback.
///
/// Legacy sidecars historically used two name variants: the content stem
/// (`Faithful 32x.voidlauncher.json`) and the full filename including the
/// extension (`Faithful 32x.zip.voidlauncher.json`). Both are probed.
pub(crate) fn read_sidecar_meta(
    content_dir: &std::path::Path,
    filename: &str,
) -> Option<serde_json::Value> {
    for p in [
        sidecar_meta_path(content_dir, filename),
        legacy_sidecar_path(content_dir, filename),
        content_dir.join(format!("{}.voidlauncher.json", filename)),
    ] {
        if let Ok(contents) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str(&contents) {
                return Some(v);
            }
        }
    }
    None
}

/// Remove sidecar metadata in both layouts (used when a content file is deleted).
pub(crate) fn remove_sidecar_meta(content_dir: &std::path::Path, filename: &str) {
    let meta = sidecar_meta_path(content_dir, filename);
    if crate::import::ensure_no_reparse_points(content_dir, &meta).is_ok() {
        let _ = std::fs::remove_file(meta);
    }
    let legacy = legacy_sidecar_path(content_dir, filename);
    if crate::import::ensure_no_reparse_points(content_dir, &legacy).is_ok() {
        let _ = std::fs::remove_file(legacy);
    }
}

/// Suffix for a content file parked aside while its replacement downloads.
/// The parked name no longer ends in `.jar`/`.zip`, so game loaders and the
/// launcher's own content listings ignore it until it is restored or removed.
pub(crate) const STASH_SUFFIX: &str = ".voidlauncher-old";

/// Park the old content file(s) of an update aside by renaming them — never
/// deleting: if the replacement download fails, the old file must still be
/// there. An update replaces the mod regardless of the exact on-disk name, so
/// the exact old filename plus both `.jar`/`.zip` twins of its stem are
/// parked (covers `.disabled` variants and same-stem leftovers).
pub(crate) fn stash_old_content_files(
    content_dir: &std::path::Path,
    old_filename: &str,
) -> std::result::Result<Vec<(PathBuf, PathBuf)>, String> {
    if let Some(base) = content_dir.parent() {
        crate::import::ensure_no_reparse_points(base, content_dir)
            .map_err(|e| format!("Refusing to park content through a junction/symlink: {e}"))?;
    }
    // `old_filename` is untrusted frontend input: strip any path components
    // so it can never escape the content directory.
    let old = std::path::Path::new(old_filename)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or("Invalid old file name")?;
    // Park only this exact file and its enabled/disabled twin. A same-stem
    // file of the other extension (e.g. `Foo.zip` next to `Foo.jar`) is a
    // different content item and must never be deleted by a successful
    // replacement of `Foo.jar`.
    let enabled = old.strip_suffix(".disabled").unwrap_or(old).to_string();
    let disabled = format!("{enabled}.disabled");
    let mut candidates: Vec<PathBuf> = Vec::new();
    for name in [&enabled, &disabled] {
        let candidate = content_dir.join(name);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    let mut stashed = Vec::new();
    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        let file_name = candidate
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid old file name")?;
        let park = content_dir.join(format!("{}{}", file_name, STASH_SUFFIX));
        // Drop a stale park left by an interrupted earlier update of the
        // same file — the live file below is the one the user has right now.
        let _ = std::fs::remove_file(&park);
        match std::fs::rename(&candidate, &park) {
            Ok(()) => stashed.push((candidate, park)),
            Err(e) => {
                // Put back anything already parked so a partial failure
                // never strands files under the park name.
                restore_stashed_files(&stashed);
                return Err(format!(
                    "Failed to park old file {}: {e}",
                    candidate.display()
                ));
            }
        }
    }
    Ok(stashed)
}

/// Put parked files back after a failed download so the user keeps the old
/// content exactly where it was.
pub(crate) fn restore_stashed_files(stashed: &[(PathBuf, PathBuf)]) {
    for (orig, park) in stashed {
        if !park.exists() {
            continue;
        }
        // A bad replacement may already sit at the original name (the
        // zip-magic check runs after the rename into place): drop it first.
        if orig.exists() {
            let _ = std::fs::remove_file(orig);
        }
        let _ = std::fs::rename(park, orig);
    }
}

/// Delete parked files after the replacement has been verified.
pub(crate) fn discard_stashed_files(stashed: &[(PathBuf, PathBuf)]) {
    for (_, park) in stashed {
        let _ = std::fs::remove_file(park);
    }
}

/// Roll back content files left parked by an interrupted update.
///
/// A crash between "park the old file" and "download the replacement" would
/// otherwise leave the mod invisible forever (a parked name is not loaded by
/// the game and is skipped by the content listings). Called from the listings:
/// if the original name is free the park is restored, if it is taken the park
/// is stale and gets removed.
pub(crate) fn recover_stashed_content_files(content_dir: &std::path::Path) {
    if let Some(base) = content_dir.parent() {
        if crate::import::ensure_no_reparse_points(base, content_dir).is_err() {
            return;
        }
    }
    let Ok(entries) = std::fs::read_dir(content_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some(base) = file_name.strip_suffix(STASH_SUFFIX) else {
            continue;
        };
        if base.is_empty() {
            continue;
        }
        let park = entry.path();
        let orig = content_dir.join(base);
        if orig.exists() {
            if park.is_dir() {
                let _ = std::fs::remove_dir_all(&park);
            } else {
                let _ = std::fs::remove_file(&park);
            }
        } else {
            let _ = std::fs::rename(&park, &orig);
        }
    }
}

/// After a successful content update, drop the sidecar of the replaced file
/// when the new file has a different stem — otherwise it would describe a
/// file that no longer exists.
pub(crate) fn cleanup_old_sidecar_after_update(
    content_dir: &std::path::Path,
    old_filename: &str,
    new_filename: &str,
) {
    let Some(old) = std::path::Path::new(old_filename)
        .file_name()
        .and_then(|n| n.to_str())
    else {
        return;
    };
    if content_stem(old) != content_stem(new_filename) {
        remove_sidecar_meta(content_dir, old);
    }
}

/// Metadata extracted from a Prism Launcher packwiz `.pw.toml` file.
#[derive(Debug, Clone, Default)]
pub(crate) struct PackwizMeta {
    /// Filename the metadata refers to (from `filename = "..."`).
    pub filename: String,
    /// Display name (from `name = "..."`).
    pub name: String,
    /// Version (from `x-prismlauncher-version-number`, falls back to `version`).
    pub version: String,
    /// Provider: "Modrinth", "CurseForge" or "".
    pub provider: String,
    /// Project id on the provider (mod-id for Modrinth, project-id for CurseForge).
    pub project_id: String,
}

const PACKWIZ_INDEX_CACHE_TTL: Duration = Duration::from_secs(300);

struct CachedPackwizIndex {
    stamp: u64,
    index: HashMap<String, PackwizMeta>,
    cached_at: Instant,
}

static PACKWIZ_INDEX_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedPackwizIndex>>> = OnceLock::new();

/// Fingerprint of all `.pw.toml` files without parsing them. Directory entry
/// order is not stable, so the per-file hashes are XOR-ed together.
fn packwiz_index_stamp(content_dir: &Path) -> u64 {
    use std::hash::Hasher;
    let mut stamp = 0u64;
    for dir in [content_dir.join(".index"), content_dir.to_path_buf()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_pw_toml = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".pw.toml"));
            if !is_pw_toml {
                continue;
            }
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            hash_path_metadata(&mut hasher, &path);
            stamp ^= hasher.finish();
        }
    }
    stamp
}

/// Cached variant used by the content listings. `list_packs` alone calls
/// `read_pack_sidecar` once per file; without this cache every file re-read and
/// re-parsed the whole `.index/` directory (O(N²) on large Prism packs).
pub(crate) fn load_packwiz_index_cached(content_dir: &Path) -> HashMap<String, PackwizMeta> {
    let stamp = packwiz_index_stamp(content_dir);
    let cache = PACKWIZ_INDEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(entry) = guard.get(content_dir) {
            if entry.stamp == stamp && entry.cached_at.elapsed() < PACKWIZ_INDEX_CACHE_TTL {
                return entry.index.clone();
            }
        }
    }
    let index = load_packwiz_index(content_dir);
    if let Ok(mut guard) = cache.lock() {
        guard.retain(|_, entry| entry.cached_at.elapsed() < PACKWIZ_INDEX_CACHE_TTL);
        guard.insert(
            content_dir.to_path_buf(),
            CachedPackwizIndex {
                stamp,
                index: index.clone(),
                cached_at: Instant::now(),
            },
        );
    }
    index
}

/// Load all packwiz `.pw.toml` metadata from a content dir — scanning both
/// the `.index/` folder (usual Prism layout) and the content dir root
/// (Prism exports shader packs' metadata as `<slug>.pw.toml` in the root),
/// keyed by the declared `filename`.
///
/// Prism Launcher stores per-file metadata (name, version, provider, project id)
/// in `.pw.toml` files next to mods/resourcepacks/shaderpacks. Our own
/// `.index/*.voidlauncher.json` sidecars take priority everywhere, but imported
/// Prism instances only have the `.pw.toml` files, so we read them as a fallback.
pub(crate) fn load_packwiz_index(content_dir: &Path) -> HashMap<String, PackwizMeta> {
    let mut map = HashMap::new();
    let scan_dirs: Vec<PathBuf> = vec![content_dir.join(".index"), content_dir.to_path_buf()];
    for index_dir in scan_dirs {
        let Ok(entries) = std::fs::read_dir(&index_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !file_name.ends_with(".pw.toml") {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(v) = contents.parse::<toml::Table>() else {
                continue;
            };
            let Some(filename) = v.get("filename").and_then(|f| f.as_str()) else {
                continue;
            };
            let mut meta = PackwizMeta {
                filename: filename.to_string(),
                ..Default::default()
            };
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                meta.name = name.to_string();
            }
            if let Some(ver) = v
                .get("x-prismlauncher-version-number")
                .and_then(|n| n.as_str())
            {
                meta.version = ver.to_string();
            } else if let Some(ver) = v.get("version").and_then(|n| n.as_str()) {
                meta.version = ver.to_string();
            } else if let Some(ver) = v
                .get("update")
                .and_then(|u| u.get("modrinth"))
                .and_then(|m| m.get("version"))
                .and_then(|m| m.as_str())
            {
                meta.version = ver.to_string();
            }
            if let Some(pid) = v
                .get("update")
                .and_then(|u| u.get("modrinth"))
                .and_then(|m| m.get("mod-id"))
                .and_then(|m| m.as_str())
            {
                meta.provider = "Modrinth".to_string();
                meta.project_id = pid.to_string();
            } else if let Some(pid) = v
                .get("update")
                .and_then(|u| u.get("curseforge"))
                .and_then(|c| c.get("project-id"))
                .and_then(|c| c.as_integer())
            {
                meta.provider = "CurseForge".to_string();
                meta.project_id = pid.to_string();
            }
            map.insert(meta.filename.clone(), meta);
        }
    }
    map
}

/// Look up packwiz metadata for a content file.
///
/// Prism stores the metadata under the enabled file name, while a disabled
/// file on disk carries a `.disabled` suffix (`foo.jar.disabled`). Retry
/// without that suffix so disabled content keeps its name, version, provider
/// and project id — the same transfer Prism's `ResourceFolderLoadTask` does.
pub(crate) fn packwiz_meta<'a>(
    index: &'a HashMap<String, PackwizMeta>,
    filename: &str,
) -> Option<&'a PackwizMeta> {
    index.get(filename).or_else(|| {
        filename
            .strip_suffix(".disabled")
            .and_then(|enabled| index.get(enabled))
    })
}

/// Look up packwiz metadata for a single content file (if any).
pub(crate) fn read_packwiz_meta(content_dir: &Path, filename: &str) -> Option<PackwizMeta> {
    packwiz_meta(&load_packwiz_index(content_dir), filename).cloned()
}

fn read_pack_sidecar(
    pack_path: &std::path::Path,
    packwiz: &HashMap<String, PackwizMeta>,
) -> Option<(String, String, String, String)> {
    let filename = pack_path.file_name()?.to_string_lossy().to_string();
    if let Some(val) = read_sidecar_meta(pack_path.parent()?, &filename) {
        let provider = normalize_provider(val["provider"].as_str().unwrap_or(""));
        let version = val["version_number"].as_str().unwrap_or("").to_string();
        let project_id = val["project_id"].as_str().unwrap_or("").to_string();
        let project_name = val["project_name"].as_str().unwrap_or("").to_string();
        return Some((provider, version, project_id, project_name));
    }
    // Fallback: Prism packwiz metadata (.pw.toml in .index/)
    let pw = packwiz_meta(packwiz, &filename)?;
    Some((
        pw.provider.clone(),
        pw.version.clone(),
        pw.project_id.clone(),
        pw.name.clone(),
    ))
}

/// Normalize a raw provider string from sidecar/packwiz metadata to the
/// display form used in the UI (mods and packs alike). Unknown historical
/// values are loader names (`Fabric`, `Forge`, ...), never download sources;
/// they must not leak into the provider/source column.
pub(crate) fn normalize_provider(raw: &str) -> String {
    match raw.to_lowercase().as_str() {
        "modrinth" => "Modrinth".to_string(),
        "curseforge" => "CurseForge".to_string(),
        "local" => "Local".to_string(),
        "atlauncher" => "ATLauncher".to_string(),
        _ => "Local".to_string(),
    }
}

/// Strip Minecraft color/formatting codes (§a, §l, §r, etc.) and any underscores used as spaces
pub(crate) fn strip_minecraft_color_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Extract a plausible version from a file name (e.g. "Pack 1.20.1.zip"
/// → "1.20.1", "mod_v5.8.1.jar" → "5.8.1", "foo-1.20.1-4.7.jar" → "4.7",
/// "Hearths v1.0.5.mod.jar" → "1.0.5",
/// "twilightforest-1.20.1-4.3.2508-universal.jar" → "4.3.2508",
/// "curios-forge-5.14.1+1.20.1.jar" → "5.14.1+1.20.1",
/// "CataclysmCompat1.0.zip" → "1.0"). Returns "" when nothing usable.
///
/// Test/diagnostic only: displayed versions must come from real metadata
/// (JAR descriptor for mods, packwiz/sidecar for packs), never a file name.
#[cfg(test)]
pub(crate) fn extract_version_from_filename(filename: &str) -> String {
    let mut stem = filename
        .trim_end_matches(".disabled")
        .trim_end_matches(".zip")
        .trim_end_matches(".jar");
    // Strip trailing ".word" junk like ".mod" / ".jar2" that some CurseForge
    // downloads carry ("Hearths v1.0.5.mod.jar"); numeric suffixes are part of
    // the version and must stay.
    loop {
        if let Some(idx) = stem.rfind('.') {
            let suffix = &stem[idx + 1..];
            if !suffix.is_empty()
                && suffix.len() <= 8
                && suffix.chars().all(|c| c.is_ascii_alphanumeric())
                && suffix.chars().any(|c| c.is_ascii_alphabetic())
            {
                stem = &stem[..idx];
                continue;
            }
        }
        break;
    }
    // Strip trailing loader/build markers ("-forge", "-neoforge", "-universal",
    // "-all", ...) so the version right before them is exposed.
    loop {
        let lower = stem.to_lowercase();
        let mut cut = None;
        for marker in [
            "neoforge",
            "forge",
            "universal",
            "universal_jar",
            "fabric",
            "fml",
            "all",
            "mod",
            "srg",
            "dev",
            "official",
            "mapped",
        ] {
            let marker_len = marker.len();
            if lower.ends_with(&format!("-{}", marker)) || lower.ends_with(&format!("_{}", marker))
            {
                cut = Some(stem.len() - 1 - marker_len);
                break;
            }
        }
        match cut {
            Some(idx) => stem = &stem[..idx],
            None => break,
        }
    }
    let bytes = stem.as_bytes();
    let n = bytes.len();
    if n == 0 {
        return String::new();
    }

    // 1) Plain numeric tail: "Pack 1.20.1", "mod_v5.8.1", "foo-1.2.3"
    let mut end = n;
    while end > 0
        && (bytes[end - 1].is_ascii_whitespace()
            || bytes[end - 1] == b'['
            || bytes[end - 1] == b']')
    {
        end -= 1;
    }
    let mut start = end;
    let mut dots = 0;
    let mut seen_digit = false;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_digit() {
            seen_digit = true;
            start -= 1;
        } else if c == b'.' && seen_digit && dots < 3 {
            dots += 1;
            start -= 1;
        } else {
            break;
        }
    }
    if seen_digit {
        if start < n && bytes[start] == b'.' {
            start += 1;
        }
        let mut ver_end = end;
        while ver_end > start && bytes[ver_end - 1].is_ascii_alphabetic() {
            ver_end -= 1;
        }
        if ver_end > start {
            let version = String::from_utf8_lossy(&bytes[start..ver_end]).into_owned();
            if start == 0 {
                return version;
            }
            let prev = bytes[start - 1];
            if prev == b'v' || prev == b'V' || prev == b'r' || prev == b'R' {
                if start == 1
                    || bytes[start - 2].is_ascii_whitespace()
                    || bytes[start - 2] == b'-'
                    || bytes[start - 2] == b'_'
                {
                    return version;
                }
            } else if prev.is_ascii_whitespace() || prev == b'-' || prev == b'_' {
                return version;
            }
        }
    }

    // 2) Version token from the first separator that leads into a number
    //    ("name-1.11.2+1.20.1", "name-1.0.0-beta.49+1.20.1", "mod 5.14.1+1.20.1").
    //    The leftmost candidate wins (longest tail), but loader words like
    //    "forge"/"neoforge" between name and version are skipped.
    let mut best: Option<String> = None;
    for (idx, c) in stem.char_indices() {
        let is_sep = c == ' ' || c == '-' || c == '_' || c == '+';
        if !is_sep {
            continue;
        }
        let after = idx + c.len_utf8();
        let tail = &stem[after..];
        let tb = tail.as_bytes();
        if tb.is_empty() {
            continue;
        }
        let first = tb[0];
        let version_start = first.is_ascii_digit() || matches!(first, b'v' | b'V' | b'r' | b'R');
        if !version_start {
            continue;
        }
        let lower = tail.to_lowercase();
        if lower.starts_with("forge")
            || lower.starts_with("neoforge")
            || lower.starts_with("fabric")
            || lower.starts_with("universal")
        {
            continue;
        }
        if tail.contains('[') || tail.contains(']') || tail.contains('(') || tail.contains(')') {
            continue;
        }
        let has_digit = tail.chars().any(|c| c.is_ascii_digit());
        if !has_digit || (!tail.contains('.') && !tail.contains('+') && tail.len() > 6) {
            continue;
        }
        // Tail must not end on a long word ("...-noStone" → skip)
        let tail_end = tail.len();
        let mut letters = 0;
        let mut k = tail_end;
        while k > 0 && tb[k - 1].is_ascii_alphabetic() {
            letters += 1;
            k -= 1;
        }
        if letters > 4 {
            continue;
        }
        if best.is_none() {
            best = Some(tail.to_string());
        }
    }
    if let Some(v) = best {
        return v;
    }

    // 3) Glued digits at the very end: "CataclysmCompat1.0" → "1.0"
    let mut s = n;
    let mut d = 0;
    let mut gseen = false;
    while s > 0 {
        let c = bytes[s - 1];
        if c.is_ascii_digit() {
            gseen = true;
            s -= 1;
        } else if c == b'.' && gseen && d < 3 {
            d += 1;
            s -= 1;
        } else {
            break;
        }
    }
    if gseen && s == 0 {
        let v = String::from_utf8_lossy(&bytes[s..n]).into_owned();
        if !v.is_empty() && v.len() <= 12 {
            return v;
        }
    }
    if gseen
        && s > 0
        && (bytes[s - 1].is_ascii_alphabetic() || bytes[s - 1] == b'_' || bytes[s - 1] == b'-')
    {
        let v = String::from_utf8_lossy(&bytes[s..n]).into_owned();
        if !v.is_empty() && v.len() <= 12 {
            return v;
        }
    }

    String::new()
}

/// Map a pack.mcmeta `pack_format` to the Minecraft version range it targets.
/// Test/diagnostic only — `pack_format` is not the pack's own version and must
/// never be shown in the Version column.
#[cfg(test)]
pub(crate) fn pack_format_to_mc_version(pack_path: &std::path::Path) -> String {
    pack_format_to_mc_version_inner(read_pack_format(pack_path))
}

#[cfg(test)]
fn pack_format_to_mc_version_inner(pack_format: Option<u64>) -> String {
    let range = match pack_format {
        Some(1..=3) => "MC 1.6-1.12",
        Some(4) => "MC 1.13",
        Some(5) => "MC 1.14",
        Some(6) => "MC 1.15",
        Some(7) => "MC 1.16",
        Some(8) => "MC 1.17-1.18",
        Some(9) => "MC 1.18",
        Some(10) => "MC 1.19-1.19.2",
        Some(12) => "MC 1.19.3",
        Some(13) => "MC 1.19.4-1.20",
        Some(15) => "MC 1.20.1",
        Some(16) => "MC 1.20.2",
        Some(17) => "MC 1.20.3-1.20.4",
        Some(18) => "MC 1.21-1.21.1",
        Some(19) => "MC 1.21.2-1.21.3",
        Some(22) => "MC 1.21.4",
        Some(32) => "MC 1.21.5",
        Some(34) => "MC 1.21.6",
        Some(42) => "MC 1.21.8",
        _ => return String::new(),
    };
    range.to_string()
}

/// Read the `pack.pack_format` integer from a resource pack (zip or folder).
#[cfg(test)]
pub(crate) fn read_pack_format(pack_path: &std::path::Path) -> Option<u64> {
    let read_json = |contents: &str| -> Option<u64> {
        let v: serde_json::Value = serde_json::from_str(contents).ok()?;
        v["pack"]["pack_format"].as_u64()
    };
    if pack_path.is_dir() {
        let f = pack_path.join("pack.mcmeta");
        std::fs::read_to_string(f).ok().and_then(|c| read_json(&c))
    } else {
        let file = std::fs::File::open(pack_path).ok()?;
        let mut archive = zip::ZipArchive::new(file).ok()?;
        let mut f = archive.by_name("pack.mcmeta").ok()?;
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut f, &mut contents).ok()?;
        read_json(&contents)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PackEntry {
    pub filename: String,
    pub name: String,
    pub is_dir: bool,
    pub file_size: u64,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_pack_enabled")]
    pub enabled: bool,
}

fn default_pack_enabled() -> bool {
    true
}

fn read_pack_icon_from_dir(path: &std::path::Path) -> Option<String> {
    // Check root pack.png first
    let icon_path = path.join("pack.png");
    if icon_path.exists() {
        return read_image_as_base64(&icon_path);
    }

    // Recurse ONLY for files literally named pack.png in subfolders.
    // Never fall back to arbitrary images: shader-pack folders are full of
    // internal GLSL textures (noise/dither/LUT maps) that would surface as
    // garbled "static" icons.
    for entry in std::fs::read_dir(path).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(img) = read_pack_icon_from_dir(&path) {
                return Some(img);
            }
        }
    }
    None
}

fn read_pack_icon_from_zip(path: &std::path::Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader).ok()?;
    let total = archive.len();

    // Build a list of entry names first (avoids borrow conflicts)
    let entries: Vec<(String, bool)> = (0..total)
        .filter_map(|i| {
            let e = archive.by_index(i).ok()?;
            let is_dir = e.is_dir();
            let name = e.name().ok()?.into_owned();
            Some((name, is_dir))
        })
        .collect();

    let root_preferred = [
        "pack.png",
        "pack.jpg",
        "pack.jpeg",
        "preview.png",
        "thumb.png",
        "icon.png",
        "logo.png",
    ];

    // Pass 1: root-level preferred names
    for (name, is_dir) in &entries {
        if *is_dir {
            continue;
        }
        let lower = name.to_lowercase();
        if root_preferred.iter().any(|p| lower == *p) {
            if let Some(img) = try_read_zip_image(&mut archive, name) {
                return Some(img);
            }
        }
    }

    // Pass 2: a literal pack.png inside a subfolder (e.g. "<sub>/pack.png").
    // NO arbitrary-image fallback: shader archives are packed with internal
    // GLSL textures (noise, dithering, LUTs) that render as garbled icons.
    for (name, is_dir) in &entries {
        if *is_dir {
            continue;
        }
        if name.to_lowercase().ends_with("/pack.png") {
            if let Some(img) = try_read_zip_image(&mut archive, name) {
                return Some(img);
            }
        }
    }

    None
}

fn try_read_zip_image<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    const MAX_PACK_ICON_BYTES: u64 = 8 * 1024 * 1024;
    let file = archive.by_name(name).ok()?;
    if file.size() > MAX_PACK_ICON_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    use std::io::Read;
    file.take(MAX_PACK_ICON_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.is_empty() || buf.len() as u64 > MAX_PACK_ICON_BYTES {
        return None;
    }
    let lower = name.to_lowercase();
    let mime = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "image/png"
    };
    Some(format!("data:{};base64,{}", mime, base64_encode(&buf)))
}

/// Extract a clean stem name from a pack filename, properly stripping `.disabled`
/// before stripping archive extensions (`.zip` / `.jar`) and removing Minecraft color codes.
pub(crate) fn clean_pack_stem(filename: &str) -> String {
    let mut s = filename;
    if let Some(without_disabled) = s.strip_suffix(".disabled") {
        s = without_disabled;
    }
    if let Some(without_zip) = s.strip_suffix(".zip") {
        s = without_zip;
    } else if let Some(without_jar) = s.strip_suffix(".jar") {
        s = without_jar;
    }
    strip_minecraft_color_codes(s).trim().to_string()
}

/// Filename fallback for a pack title. Metadata (`pack.name` / sidecar) still
/// wins; when only the filename exists, drop an exact trailing instance
/// Minecraft-version suffix such as `"Faithful 32x - 26.3"`. Only the current
/// instance version is removed, so a genuine version in the pack name stays.
pub(crate) fn clean_pack_stem_for_mc(filename: &str, mc_version: &str) -> String {
    let stem = clean_pack_stem(filename);
    strip_mc_version_suffix(&stem, mc_version).unwrap_or(stem)
}

fn strip_mc_version_suffix(stem: &str, mc_version: &str) -> Option<String> {
    let mc_version = mc_version.trim();
    if mc_version.is_empty() {
        return None;
    }
    for suffix in [
        format!(" - {mc_version}"),
        format!(" ({mc_version})"),
        format!(" [{mc_version}]"),
        format!("_{mc_version}"),
        format!("-{mc_version}"),
    ] {
        if let Some(rest) = stem.strip_suffix(&suffix) {
            let rest = rest.trim_end_matches([' ', '-', '_']).trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Recursively extract text from Minecraft JSON text components (strings, objects with "text" / "extra", or arrays).
pub(crate) fn extract_minecraft_text(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(s) => {
            out.push_str(s);
        }
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(text)) = map.get("text") {
                out.push_str(text);
            }
            if let Some(extra) = map.get("extra") {
                extract_minecraft_text(extra, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                extract_minecraft_text(item, out);
            }
        }
        _ => {}
    }
}

/// Parse Minecraft text component into clean plain text stripped of color codes and HTML tags.
pub(crate) fn parse_minecraft_text(value: &serde_json::Value) -> Option<String> {
    let mut out = String::new();
    extract_minecraft_text(value, &mut out);
    let stripped = strip_minecraft_color_codes(&out);
    let clean = stripped
        .replace("<br>", " ")
        .replace("<br/>", " ")
        .replace("</br>", " ");
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read pack explicit name and description from pack.mcmeta JSON.
///
/// In Minecraft, `pack.description` is NOT a title/name — it is descriptive text.
/// `explicit_name` is only returned if `pack.name` is present; otherwise None.
pub(crate) fn read_pack_meta_from_json(
    json: &serde_json::Value,
) -> (Option<String>, Option<String>) {
    let pack = &json["pack"];
    let explicit_name = pack.get("name").and_then(parse_minecraft_text);
    let description = pack.get("description").and_then(parse_minecraft_text);
    (explicit_name, description)
}

fn read_pack_meta_from_dir(path: &std::path::Path) -> (Option<String>, Option<String>) {
    const MAX_MCMETA_BYTES: u64 = 1024 * 1024;
    let Ok(mut file) = std::fs::File::open(path.join("pack.mcmeta")) else {
        return (None, None);
    };
    if file.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_MCMETA_BYTES {
        return (None, None);
    }
    let mut content = String::new();
    if std::io::Read::read_to_string(&mut file, &mut content).is_err() {
        return (None, None);
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return (None, None);
    };
    read_pack_meta_from_json(&json)
}

fn read_pack_meta_from_zip(path: &std::path::Path) -> (Option<String>, Option<String>) {
    let Ok(file) = std::fs::File::open(path) else {
        return (None, None);
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return (None, None);
    };
    let Ok(mcmeta) = archive.by_name("pack.mcmeta") else {
        return (None, None);
    };
    const MAX_MCMETA_BYTES: u64 = 1024 * 1024;
    let mut bytes = Vec::new();
    let mut reader = std::io::BufReader::new(mcmeta).take(MAX_MCMETA_BYTES);
    if std::io::Read::read_to_end(&mut reader, &mut bytes).is_err() {
        return (None, None);
    }
    let Ok(content) = String::from_utf8(bytes) else {
        return (None, None);
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return (None, None);
    };
    read_pack_meta_from_json(&json)
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = std::fs::metadata(&p) {
                total += m.len();
            }
        }
    }
    total
}

fn read_image_as_base64(path: &std::path::Path) -> Option<String> {
    const MAX_ICON_BYTES: u64 = 8 * 1024 * 1024;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_ICON_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    file.take(MAX_ICON_BYTES + 1).read_to_end(&mut buf).ok()?;
    if buf.is_empty() || buf.len() as u64 > MAX_ICON_BYTES {
        return None;
    }
    Some(format!("data:image/png;base64,{}", base64_encode(&buf)))
}

pub(crate) fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Minimal base64 decode (decodes standard base64 with padding)
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const DECODE: [i8; 256] = {
        let mut table = [-1i8; 256];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < 64 {
            table[chars[i] as usize] = i as i8;
            i += 1;
        }
        table[b'=' as usize] = 0;
        table
    };

    let clean: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'\r' && b != b'\n' && b != b' ')
        .collect();
    if clean.is_empty() || !clean.len().is_multiple_of(4) {
        return None;
    }

    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for chunk in clean.chunks(4) {
        if chunk.len() != 4 {
            return None;
        }
        let mut vals = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            let v = DECODE.get(b as usize)?;
            if *v == -1 {
                return None;
            }
            vals[i] = *v as u8;
        }
        out.push((vals[0] << 2) | (vals[1] >> 4));
        if chunk[2] != b'=' {
            out.push((vals[1] << 4) | (vals[2] >> 2));
        }
        if chunk[3] != b'=' {
            out.push((vals[2] << 6) | vals[3]);
        }
    }
    Some(out)
}

/// Update instance's last played timestamp
pub fn update_last_played(instances_dir: &Path, name: &str) -> Result<()> {
    let mut instance = get_instance(instances_dir, name)?;
    instance.last_played = Some(Utc::now().to_rfc3339());
    save_instance(instances_dir, &instance, None)?;
    Ok(())
}

// ── Prism compatibility helpers ─────────────────────────────────

/// Write a Prism/MultiMC-compatible instance.cfg from our Instance
fn write_prism_cfg(instance: &Instance, instances_dir: &Path) -> Result<()> {
    let cfg_path = instance.prism_cfg_file(instances_dir);
    write_file_atomic(&cfg_path, build_prism_cfg_content(instance).as_bytes())?;
    Ok(())
}

/// Build a Prism/MultiMC-compatible `mmc-pack.json` string from our Instance.
/// This is what Prism (and MultiMC-derived launchers) read to determine the
/// Minecraft version and mod loader of an instance.
fn build_mmc_pack_json(instance: &Instance) -> Result<String> {
    let mut components = Vec::new();
    components.push(serde_json::json!({
        "uid": "net.minecraft",
        "version": instance.mc_version,
        "important": true,
    }));
    match instance.loader {
        LoaderType::Vanilla => {}
        LoaderType::Fabric => {
            components.push(serde_json::json!({
                "uid": "net.fabricmc.fabric-loader",
                "version": instance.loader_version.clone().unwrap_or_default(),
            }));
        }
        LoaderType::Forge => {
            components.push(serde_json::json!({
                "uid": "net.minecraftforge",
                "version": instance.loader_version.clone().unwrap_or_default(),
            }));
        }
        LoaderType::NeoForge => {
            components.push(serde_json::json!({
                "uid": "net.neoforged",
                "version": instance.loader_version.clone().unwrap_or_default(),
            }));
        }
    }
    let pack = serde_json::json!({
        "components": components,
        "formatVersion": 1,
    });
    Ok(serde_json::to_string_pretty(&pack)?)
}

/// Write Prism-compatible `mmc-pack.json` into the instance directory so the
/// folder is browsable/importable by Prism-derived launchers at any time.
fn write_mmc_pack_json(instance: &Instance, instances_dir: &Path) -> Result<()> {
    let path = instance.dir(instances_dir).join("mmc-pack.json");
    write_file_atomic(&path, build_mmc_pack_json(instance)?.as_bytes())?;
    Ok(())
}

/// Write instance icon as pack.png in the instance root directory
pub fn write_pack_png(instances_dir: &Path, instance_name: &str, icon_data: &str) {
    let instance = match get_instance(instances_dir, instance_name) {
        Ok(i) => i,
        Err(_) => return,
    };
    let png_path = instance.dir(instances_dir).join("pack.png");

    // Decode base64 data URL (data:image/png;base64,...)
    if let Some(b64) = icon_data.split(";base64,").nth(1) {
        if let Some(bytes) = base64_decode(b64) {
            let _ = std::fs::write(&png_path, &bytes);
        }
    }
}

/// Values read from a Prism/MultiMC `instance.cfg` that map onto our model.
struct PrismCfgFields {
    name: Option<String>,
    memory_mb: Option<u32>,
    jvm_args: Option<Vec<String>>,
}

fn parse_prism_cfg_text(content: &str) -> PrismCfgFields {
    let mut name = None;
    let mut max_mem: Option<u32> = None;
    let mut override_memory = false;
    let mut jvm_args_raw: Option<String> = None;
    let mut override_java_args = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') || line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim();
        let val = line[eq + 1..].trim();
        if key.eq_ignore_ascii_case("name") && !val.is_empty() {
            name = Some(val.to_string());
        } else if key.eq_ignore_ascii_case("MaxMemAlloc") {
            max_mem = val.parse::<u32>().ok().filter(|m| *m > 0);
        } else if key.eq_ignore_ascii_case("OverrideMemory") {
            override_memory = val.eq_ignore_ascii_case("true");
        } else if key.eq_ignore_ascii_case("JvmArgs") {
            jvm_args_raw = Some(val.to_string());
        } else if key.eq_ignore_ascii_case("OverrideJavaArgs") {
            override_java_args = val.eq_ignore_ascii_case("true");
        }
    }

    PrismCfgFields {
        name,
        memory_mb: override_memory.then_some(max_mem).flatten(),
        jvm_args: override_java_args
            .then_some(jvm_args_raw)
            .flatten()
            .filter(|raw| !raw.trim().is_empty())
            .map(|raw| raw.split_whitespace().map(str::to_string).collect()),
    }
}

/// Try to parse a Prism instance.cfg into our Instance format.
///
/// Version and loader come from the sibling `mmc-pack.json`: a folder with a
/// cfg but no usable mmc-pack is not a launchable instance and is refused
/// instead of being presented as Vanilla with an empty version.
pub fn parse_prism_cfg(cfg_path: &std::path::Path) -> Option<Instance> {
    let content = std::fs::read_to_string(cfg_path).ok()?;
    let dir = cfg_path.parent()?;
    let dir_name = dir.file_name()?.to_string_lossy().to_string();
    let fields = parse_prism_cfg_text(&content);

    let pack_content = std::fs::read_to_string(dir.join("mmc-pack.json")).ok()?;
    let (mc_version, loader, loader_version) = parse_prism_mmc_pack(&pack_content).ok()?;
    if mc_version.trim().is_empty() {
        tracing::warn!(
            target: "launcher",
            "Prism instance at {} has no Minecraft version in mmc-pack.json",
            dir.display()
        );
        return None;
    }

    let icon_data = read_image_as_base64(&dir.join("pack.png"));
    let now = chrono::Utc::now().to_rfc3339();
    Some(Instance {
        name: fields.name.unwrap_or(dir_name),
        mc_version,
        loader,
        loader_version,
        loader_profile: None,
        memory_mb: fields.memory_mb,
        jvm_args: fields.jvm_args,
        gc_preset: None,
        java_path: None,
        resolution: None,
        icon: icon_data,
        banner: None,
        created_at: now,
        last_played: None,
        play_time_seconds: 0,
        notes: String::new(),
    })
}

/// Parse Prism/MultiMC `mmc-pack.json` (components) into our metadata.
/// Returns (mc_version, loader, loader_version). Quilt packs are rejected —
/// Quilt is not supported by VoidLauncher.
fn parse_prism_mmc_pack(content: &str) -> Result<(String, LoaderType, Option<String>)> {
    let mut mc_version = String::new();
    let mut loader = LoaderType::Vanilla;
    let mut loader_version: Option<String> = None;

    let pack = serde_json::from_str::<serde_json::Value>(content)
        .map_err(|e| LauncherError::Instance(format!("Invalid Prism mmc-pack.json: {}", e)))?;
    let components = pack["components"].as_array().ok_or_else(|| {
        LauncherError::Instance("Invalid Prism mmc-pack.json: missing components array".to_string())
    })?;

    for comp in components {
        let uid = comp["uid"].as_str().unwrap_or("");
        let ver = comp["version"].as_str().unwrap_or("");
        if comp["dependencyOnly"].as_bool().unwrap_or(false) {
            continue;
        }
        match uid {
            "net.minecraft" | "org.multimc.minecraft" | "org.prismlauncher.minecraft"
                if !ver.is_empty() =>
            {
                mc_version = ver.to_string();
            }
            "net.fabricmc.fabric-loader" => {
                loader = LoaderType::Fabric;
                loader_version = Some(ver.to_string());
            }
            "org.quiltmc.quilt-loader" => {
                return Err(LauncherError::Instance(
                    "This instance requires Quilt, which is not supported by VoidLauncher."
                        .to_string(),
                ));
            }
            "net.minecraftforge" => {
                loader = LoaderType::Forge;
                loader_version = Some(ver.to_string());
            }
            "net.neoforged" => {
                loader = LoaderType::NeoForge;
                loader_version = Some(ver.to_string());
            }
            _ => {}
        }
    }

    if mc_version.trim().is_empty() {
        return Err(LauncherError::Instance(
            "Prism mmc-pack.json does not name a Minecraft version".to_string(),
        ));
    }
    if !matches!(loader, LoaderType::Vanilla)
        && loader_version.as_deref().unwrap_or("").trim().is_empty()
    {
        return Err(LauncherError::Instance(
            "Prism mmc-pack.json names a mod loader without a version".to_string(),
        ));
    }

    Ok((mc_version, loader, loader_version))
}

/// Metadata read from a Prism archive before any file is extracted.
struct PrismPackMeta {
    /// Name stored inside the pack. Only used when the caller has no name of
    /// its own (standalone `.zip` import); a wizard import always uses the
    /// name the user typed.
    pack_name: String,
    mc_version: String,
    loader: LoaderType,
    loader_version: Option<String>,
    memory_mb: Option<u32>,
    jvm_args: Option<Vec<String>>,
}

/// Read `instance.cfg` (name) + `mmc-pack.json` (version/loader) in one pass.
/// The archive stays file-backed; Prism packs can contain huge worlds.
fn read_prism_pack_meta(zip_path: &Path) -> Result<PrismPackMeta> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?)
        .map_err(|e| LauncherError::Instance(format!("Invalid ZIP: {}", e)))?;

    let mut found = false;
    let mut pack_found = false;
    let mut cfg_content = String::new();
    let mut pack_content = String::new();

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        let entry_name = entry.name()?.to_string();
        // Preflight every entry before the target directory is created: an
        // archive with a traversal path must fail cleanly.
        if crate::import::check_safe_relative(&entry_name).is_err() {
            return Err(LauncherError::Instance(format!(
                "Unsafe path in Prism pack: {}",
                entry_name
            )));
        }
        match entry_name.as_str() {
            "instance.cfg" => {
                found = true;
                entry.read_to_string(&mut cfg_content).ok();
            }
            "mmc-pack.json" => {
                pack_found = true;
                entry.read_to_string(&mut pack_content).ok();
            }
            _ => {}
        }
    }

    if !found {
        return Err(LauncherError::Instance(
            "Not a valid Prism instance pack: missing instance.cfg".to_string(),
        ));
    }

    if !pack_found {
        return Err(LauncherError::Instance(
            "Not a valid Prism instance pack: missing mmc-pack.json (cannot determine the Minecraft version)".to_string(),
        ));
    }

    let fields = parse_prism_cfg_text(&cfg_content);
    let pack_name = fields.name.unwrap_or_else(|| {
        zip_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("imported")
            .to_string()
    });
    let (mc_version, loader, loader_version) = parse_prism_mmc_pack(&pack_content)?;
    Ok(PrismPackMeta {
        pack_name,
        mc_version,
        loader,
        loader_version,
        memory_mb: fields.memory_mb,
        jvm_args: fields.jvm_args,
    })
}

fn validate_prism_instance_name(name: &str) -> Result<()> {
    let invalid =
        || LauncherError::Instance(format!("Invalid instance name in Prism pack: '{}'", name));
    if name != name.trim() || !(3..=64).contains(&name.chars().count()) {
        return Err(invalid());
    }
    validate_instance_dir_name(name, "Instance name").map_err(|_| invalid())
}

/// Import a Prism Launcher instance.zip into VoidLauncher instances dir.
///
/// Prism/MultiMC store the game directory as `minecraft/` (no dot) and keep
/// metadata in `instance.cfg` + `mmc-pack.json`. We map the game directory to
/// our canonical `.minecraft/` and translate the loader components, so mods,
/// resourcepacks, shaderpacks, configs and saves land where our launcher
/// expects them. Per-launch junk (.bobby cache, .mixin.out, crash-reports,
/// logs) and Prism metadata files are not carried over.
///
/// The pack's own `instance.cfg` name is used when no explicit name is given.
pub fn import_prism_pack(
    instances_dir: &Path,
    zip_path: &str,
    launches: &crate::launch_state::LaunchRegistry,
) -> Result<Instance> {
    import_prism_pack_as(instances_dir, zip_path, None, launches)
}

/// Name a Prism archive carries for a standalone import. Legacy IPC needs the
/// same name the old `import_prism_pack` would have chosen before it can use
/// the shared import/prepare orchestration.
pub(crate) fn prism_pack_default_name(zip_path: &str) -> Result<String> {
    Ok(read_prism_pack_meta(Path::new(zip_path))?.pack_name)
}

/// Import a Prism archive under a caller-chosen instance name. The name is
/// used for the directory, `instance.json`, `instance.cfg` and the UI, so a
/// mismatch with the name stored in the archive can never split the instance
/// between two folders.
pub fn import_prism_pack_as(
    instances_dir: &Path,
    zip_path: &str,
    instance_name: Option<&str>,
    launches: &crate::launch_state::LaunchRegistry,
) -> Result<Instance> {
    let zip = std::path::Path::new(zip_path);
    // Parse first: an invalid/corrupt archive must fail before the target
    // directory is reserved.
    let meta = read_prism_pack_meta(zip)?;
    let chosen = instance_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
        .unwrap_or_else(|| meta.pack_name.clone());
    validate_prism_instance_name(&chosen)?;

    // Atomic ownership: a concurrent import of the same name fails here
    // instead of both importers merging into one directory. The reservation
    // also blocks launches until the archive is fully imported.
    let mut reservation =
        crate::import::InstanceReservation::reserve(instances_dir, &chosen, launches, "import")?;
    let instance = import_prism_pack_into(instances_dir, zip_path, &chosen, Some(meta))?;
    reservation.set_stage(crate::instance_readiness::SetupStage::Preparing)?;
    reservation.commit()?;
    Ok(instance)
}

/// Import a Prism archive into an already reserved `instances_dir/<name>`.
/// The caller (import_with_cleanup) owns the directory and its cleanup.
pub(crate) fn import_prism_pack_reserved(
    instances_dir: &Path,
    zip_path: &str,
    instance_name: &str,
) -> Result<Instance> {
    let name = instance_name.trim();
    validate_prism_instance_name(name)?;
    import_prism_pack_into(instances_dir, zip_path, name, None)
}

fn import_prism_pack_into(
    instances_dir: &Path,
    zip_path: &str,
    instance_name: &str,
    metadata: Option<PrismPackMeta>,
) -> Result<Instance> {
    let zip_path = std::path::Path::new(zip_path);
    if !zip_path.exists() {
        return Err(LauncherError::Instance(format!(
            "ZIP file not found: {}",
            zip_path.display()
        )));
    }

    let meta = match metadata {
        Some(meta) => meta,
        None => read_prism_pack_meta(zip_path)?,
    };
    let name = instance_name.trim().to_string();
    let PrismPackMeta {
        pack_name,
        mc_version,
        loader,
        loader_version,
        memory_mb,
        jvm_args,
    } = meta;
    if name != pack_name {
        tracing::info!(
            target: "launcher",
            "Prism pack '{}' is being imported as '{}' (name chosen by the user)",
            pack_name, name
        );
    }

    // Version strings become directory names at launch: reject traversal
    // from a tampered mmc-pack.json before the instance is created.
    if !mc_version.is_empty() && crate::import::check_safe_file_name(&mc_version).is_err() {
        return Err(LauncherError::Instance(format!(
            "Invalid Minecraft version in Prism pack: '{}'",
            mc_version
        )));
    }
    if let Some(version) = loader_version.as_deref().filter(|v| !v.is_empty()) {
        if crate::import::check_safe_file_name(version).is_err() {
            return Err(LauncherError::Instance(format!(
                "Invalid loader version in Prism pack: '{}'",
                version
            )));
        }
    }

    // The directory was reserved by the caller; `create_dir_all` is a no-op
    // for an existing (empty) directory and still works for direct callers.
    let target_dir = instances_dir.join(&name);
    std::fs::create_dir_all(&target_dir)?;

    // Extract with mapping: Prism `minecraft/` (and our own `.minecraft/`
    // exports) → canonical `.minecraft/`. Skip Prism metadata and launch junk.
    {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?)
            .map_err(|e| LauncherError::Instance(format!("Invalid ZIP: {}", e)))?;
        crate::import::check_archive_entry_count(archive.len())?;
        let mut extract_budget = crate::import::ExtractBudget::new();

        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            if entry.is_dir() {
                continue;
            }

            let entry_name = entry.name()?.to_string();
            // Reject traversal, absolute/rooted paths and drive prefixes before
            // mapping the entry into the target instance.
            if crate::import::check_safe_relative(&entry_name).is_err() {
                return Err(LauncherError::Instance(format!(
                    "Unsafe path in Prism pack: {}",
                    entry_name
                )));
            }
            let normalised = entry_name.replace('\\', "/");

            // Skip per-launch junk we don't need
            fn is_junk(rel: &str) -> bool {
                rel.starts_with(".bobby/")
                    || rel.starts_with(".mixin.out/")
                    || rel.starts_with("crash-reports/")
                    || rel.starts_with("logs/")
            }

            let relative: Option<String> = if let Some(rel) = normalised.strip_prefix("minecraft/")
            {
                if is_junk(rel) {
                    None
                } else {
                    Some(format!(".minecraft/{}", rel))
                }
            } else if let Some(rel) = normalised.strip_prefix(".minecraft/") {
                if is_junk(rel) {
                    None
                } else {
                    Some(format!(".minecraft/{}", rel))
                }
            } else {
                match normalised.as_str() {
                    "instance.cfg" | "instance.json" | "mmc-pack.json" | ".packignore" => None,
                    "icon.png" | "pack.png" => Some(normalised),
                    _ if normalised.starts_with("patches/") => None,
                    _ => None,
                }
            };

            let Some(relative) = relative else { continue };
            crate::import::write_extracted_budgeted(
                &target_dir,
                &relative,
                &mut entry,
                &mut extract_budget,
            )?;
        }
    }

    // Root icon → instance icon
    let mut icon = None;
    for ic in ["icon.png", "pack.png"] {
        let p = target_dir.join(ic);
        if p.exists() {
            icon = read_image_as_base64(&p);
            let _ = std::fs::remove_file(&p);
            if icon.is_some() {
                break;
            }
        }
    }

    // Parse metadata and save as instance.json (+ Prism-compatible instance.cfg)
    let now = chrono::Utc::now().to_rfc3339();
    let instance = Instance {
        name,
        mc_version,
        loader,
        loader_version,
        loader_profile: None,
        memory_mb,
        jvm_args,
        gc_preset: None,
        java_path: None,
        resolution: None,
        icon,
        banner: None,
        created_at: now.clone(),
        last_played: None,
        play_time_seconds: 0,
        notes: String::new(),
    };
    save_instance_reserved(instances_dir, &instance, None)?;

    tracing::info!(
        target: "launcher",
        "Imported Prism pack '{}' (MC {}, loader {:?} {:?}) from {:?}",
        instance.name,
        instance.mc_version,
        instance.loader,
        instance.loader_version,
        zip_path
    );
    Ok(instance)
}

/// Export an instance as a Prism/MultiMC-compatible .zip archive.
///
/// The archive layout matches what Prism Launcher produces and expects:
/// ```text
/// MyInstance/
/// ├── instance.cfg
/// ├── mmc-pack.json
/// ├── pack.png          (when the instance has an icon)
/// └── minecraft/        (game dir, no leading dot)
///     ├── mods/
///     ├── config/
///     └── ...
/// ```
/// VoidLauncher-internal files (`instance.json`) and per-launch junk
/// (logs, crash-reports, caches) are excluded. Files are streamed into the
/// archive, so large saves do not load into memory.
pub fn export_instance(instances_dir: &Path, name: &str, output_path: &str) -> Result<()> {
    use zip::write::SimpleFileOptions;

    let _reservation = InstanceNameReservation::acquire(instances_dir, &[name])?;
    let instance = get_instance(instances_dir, name)?;
    let instance_dir = instance.dir(instances_dir);
    if !instance_dir.exists() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    if crate::instance_readiness::read_pending(&instance_dir).is_some() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' is not fully set up. Finish its setup before exporting it.",
            name
        )));
    }

    let out_path = std::path::Path::new(output_path);
    let file = std::fs::File::create(out_path)?;
    let mut zip_w = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    // Prism metadata files, generated fresh from our Instance so the archive
    // is always current even if on-disk copies are stale.
    zip_w.start_file("instance.cfg", options)?;
    let cfg_lines = build_prism_cfg_content(&instance);
    std::io::Write::write_all(&mut zip_w, cfg_lines.as_bytes())?;

    zip_w.start_file("mmc-pack.json", options)?;
    let mmc_pack = build_mmc_pack_json(&instance)?;
    std::io::Write::write_all(&mut zip_w, mmc_pack.as_bytes())?;

    // pack.png — prefer the in-memory icon, fall back to the on-disk copy
    // written by save_instance.
    let icon_bytes: Option<Vec<u8>> = instance
        .icon
        .as_ref()
        .and_then(|icon| icon.split(";base64,").nth(1))
        .and_then(base64_decode)
        .or_else(|| std::fs::read(instance_dir.join("pack.png")).ok());
    if let Some(bytes) = icon_bytes {
        zip_w.start_file("pack.png", options)?;
        std::io::Write::write_all(&mut zip_w, &bytes)?;
    }

    // Game directory: our `.minecraft/` → Prism `minecraft/`.
    let mc_dir = instance.minecraft_dir(instances_dir);
    if mc_dir.exists() {
        add_minecraft_dir_to_zip(&mut zip_w, &mc_dir, "minecraft", options)?;
    }

    zip_w.finish()?;

    tracing::info!(target: "launcher", "Exported instance '{}' to {:?}", name, out_path);
    Ok(())
}

/// Render the `[General]`/`[MultiMC]` body of a Prism instance.cfg as a
/// string (shared by on-disk save and zip export).
fn build_prism_cfg_content(instance: &Instance) -> String {
    let last_launch = instance
        .last_played
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
        .unwrap_or(0);

    let mut lines = Vec::new();
    lines.push("[General]".to_string());
    lines.push("ConfigVersion=1.3".to_string());
    lines.push("InstanceType=OneSix".to_string());
    lines.push(format!("name={}", instance.name));
    lines.push(format!("iconKey={}", instance.name));
    lines.push(format!("notes={}", instance.notes.replace('\n', "\\n")));
    lines.push(format!("lastLaunchTime={}", last_launch));
    lines.push(format!("totalTimePlayed={}", instance.play_time_seconds));
    lines.push(String::new());
    lines.push("[MultiMC]".to_string());
    lines.push("autoCloseMinecraft=false".to_string());
    lines.join("\n")
}

/// Stream a directory tree into the zip under `prefix/`, skipping launch
/// junk (logs, crash-reports, caches) and `.voidlauncher-old` stash files
/// left by transactional content updates. Files are copied in chunks, not
/// buffered whole.
fn add_minecraft_dir_to_zip(
    zip: &mut zip::ZipWriter<std::fs::File>,
    dir: &std::path::Path,
    prefix: &str,
    options: zip::write::SimpleFileOptions,
) -> Result<()> {
    fn is_junk(name: &str) -> bool {
        name == "logs"
            || name == "crash-reports"
            || name == ".bobby"
            || name == ".mixin.out"
            || name.ends_with(".voidlauncher-old")
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if is_junk(&name) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!(target: "launcher", "Skipping unreadable export entry {}: {}", path.display(), e);
                continue;
            }
        };
        if crate::import::is_unsafe_reparse_point(&metadata) {
            tracing::warn!(
                target: "launcher",
                "Skipping junction/symlink while exporting: {}",
                path.display()
            );
            continue;
        }
        let zip_path = format!("{}/{}", prefix, name);
        if metadata.is_dir() {
            zip.add_directory(&zip_path, options)?;
            add_minecraft_dir_to_zip(zip, &path, &zip_path, options)?;
        } else if metadata.is_file() {
            zip.start_file(&zip_path, options)?;
            let mut file = std::fs::File::open(&path)?;
            std::io::copy(&mut file, zip)?;
        }
    }
    Ok(())
}

/// Zip the contents of `dir` under `prefix` (no leading slash). Root files sit
/// at the archive root; subdirectories accumulate their path on recursion.
/// Entries whose file name satisfies `skip` (and their whole subtree) are left
/// out — backups use it to drop logs, screenshots, caches and themselves.
fn add_dir_contents_to_zip(
    zip: &mut zip::ZipWriter<std::fs::File>,
    dir: &std::path::Path,
    prefix: &str,
    options: zip::write::SimpleFileOptions,
    skip: &dyn Fn(&str) -> bool,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if skip(&name) {
            continue;
        }
        // Never follow a junction/symlink: in the best case the archive would
        // capture data from outside the instance, in the worst case a cyclic
        // junction makes the walk recurse forever.
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!(target: "launcher", "Skipping unreadable backup entry {}: {}", path.display(), e);
                continue;
            }
        };
        if crate::import::is_unsafe_reparse_point(&metadata) {
            tracing::warn!(
                target: "launcher",
                "Skipping junction/symlink while archiving: {}",
                path.display()
            );
            continue;
        }
        let zip_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", prefix, name)
        };
        if metadata.is_dir() {
            zip.add_directory(&zip_path, options)?;
            add_dir_contents_to_zip(zip, &path, &zip_path, options, skip)?;
        } else if metadata.is_file() {
            zip.start_file(&zip_path, options)?;
            let mut file = std::fs::File::open(&path)?;
            std::io::copy(&mut file, zip)?;
        }
    }
    Ok(())
}

// ── World operations ──────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct LevelDatData {
    pub game_type: Option<i32>,
    pub seed: Option<i64>,
    pub level_name: Option<String>,
    pub last_played: Option<i64>,
}

/// Safety bounds for untrusted `level.dat` input. A corrupt/hostile file
/// must never make the worlds tab allocate gigabytes or recurse until the
/// stack overflows.
const NBT_MAX_DEPTH: u32 = 64;
const NBT_MAX_LIST_LEN: usize = 1_000_000;
const NBT_MAX_SKIP_BYTES: u64 = 256 * 1024 * 1024;
const NBT_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

fn nbt_read_string(r: &mut impl Read) -> Result<String> {
    let mut len_buf = [0u8; 2];
    r.read_exact(&mut len_buf)
        .map_err(|e| LauncherError::Instance(e.to_string()))?;
    let len = u16::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)
        .map_err(|e| LauncherError::Instance(e.to_string()))?;
    String::from_utf8(buf)
        .map_err(|e| LauncherError::Instance(format!("Invalid UTF-8 in NBT: {e}")))
}

fn nbt_skip(r: &mut impl Read, tag: u8, depth: u32) -> Result<()> {
    if depth > NBT_MAX_DEPTH {
        return Err(LauncherError::Instance(
            "NBT nesting depth limit exceeded".into(),
        ));
    }
    match tag {
        0 => Ok(()),
        1..=6 => {
            let len = match tag {
                1 => 1,
                2 => 2,
                3 => 4,
                4 => 8,
                5 => 4,
                6 => 8,
                _ => unreachable!(),
            };
            let mut b = [0u8; 8];
            r.read_exact(&mut b[..len])
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            Ok(())
        }
        7 | 11 | 12 => {
            let mut len_buf = [0u8; 4];
            r.read_exact(&mut len_buf)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            let elem_size: u64 = match tag {
                7 => 1,
                11 => 4,
                12 => 8,
                _ => 1,
            };
            let len = u32::from_be_bytes(len_buf) as u64;
            let total = len * elem_size;
            if total > NBT_MAX_SKIP_BYTES {
                return Err(LauncherError::Instance(format!(
                    "NBT array too large: {} bytes",
                    total
                )));
            }
            // Stream-skip instead of allocating `total` bytes: a corrupt
            // length must not allocate gigabytes up front.
            let mut limited = r.by_ref().take(total);
            let copied = std::io::copy(&mut limited, &mut std::io::sink())
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            if copied != total {
                return Err(LauncherError::Instance(
                    "Truncated NBT array payload".into(),
                ));
            }
            Ok(())
        }
        8 => {
            nbt_read_string(r)?;
            Ok(())
        }
        9 => {
            let mut elem_type = [0u8; 1];
            r.read_exact(&mut elem_type)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            let mut len_buf = [0u8; 4];
            r.read_exact(&mut len_buf)
                .map_err(|e| LauncherError::Instance(e.to_string()))?;
            let len = u32::from_be_bytes(len_buf) as usize;
            if len > NBT_MAX_LIST_LEN {
                return Err(LauncherError::Instance(format!(
                    "NBT list too long: {} entries",
                    len
                )));
            }
            if elem_type[0] == 0 && len > 0 {
                return Err(LauncherError::Instance(
                    "NBT list uses TAG_End as its element type".into(),
                ));
            }
            for _ in 0..len {
                nbt_skip(r, elem_type[0], depth + 1)?;
            }
            Ok(())
        }
        10 => {
            loop {
                let mut t = [0u8; 1];
                r.read_exact(&mut t)
                    .map_err(|e| LauncherError::Instance(e.to_string()))?;
                if t[0] == 0 {
                    break;
                }
                nbt_read_string(r)?;
                nbt_skip(r, t[0], depth + 1)?;
            }
            Ok(())
        }
        _ => Err(LauncherError::Instance(format!(
            "Unknown NBT tag type: {tag}"
        ))),
    }
}

fn nbt_parse_compound_fields(r: &mut impl Read, data: &mut LevelDatData, depth: u32) -> Result<()> {
    if depth > NBT_MAX_DEPTH {
        return Err(LauncherError::Instance(
            "NBT nesting depth limit exceeded".into(),
        ));
    }
    loop {
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if tag[0] == 0 {
            break;
        }
        let name = nbt_read_string(r)?;
        match tag[0] {
            3 => {
                let mut buf = [0u8; 4];
                r.read_exact(&mut buf)
                    .map_err(|e| LauncherError::Instance(e.to_string()))?;
                let val = i32::from_be_bytes(buf);
                if name == "GameType" {
                    data.game_type = Some(val);
                }
            }
            4 => {
                let mut buf = [0u8; 8];
                r.read_exact(&mut buf)
                    .map_err(|e| LauncherError::Instance(e.to_string()))?;
                let val = i64::from_be_bytes(buf);
                match name.as_str() {
                    "Seed" => data.seed = Some(val),
                    "LastPlayed" => data.last_played = Some(val),
                    _ => {}
                }
            }
            8 => {
                let val = nbt_read_string(r)?;
                if name == "LevelName" {
                    data.level_name = Some(val);
                }
            }
            10 => {
                if name == "Data" {
                    nbt_parse_compound_fields(r, data, depth + 1)?;
                } else {
                    nbt_skip(r, 10, depth + 1)?;
                }
            }
            _ => {
                nbt_skip(r, tag[0], depth + 1)?;
            }
        }
    }
    Ok(())
}

/// Parse a GZip-compressed Minecraft level.dat and extract key fields.
pub fn parse_level_dat(path: &std::path::Path) -> LevelDatData {
    let Ok(file) = std::fs::File::open(path) else {
        return LevelDatData::default();
    };
    // Bound the decompressed stream as well: a gzip bomb in a downloaded
    // world must not keep the parser reading forever.
    let mut decoder = GzDecoder::new(file).take(NBT_MAX_FILE_BYTES);
    let mut tag = [0u8; 1];
    if decoder.read_exact(&mut tag).is_err() || tag[0] != 10 {
        return LevelDatData::default();
    }
    let _ = nbt_read_string(&mut decoder);
    let mut data = LevelDatData::default();
    let _ = nbt_parse_compound_fields(&mut decoder, &mut data, 0);
    data
}

pub fn rename_world(
    instances_dir: &Path,
    instance_name: &str,
    old_name: &str,
    new_name: &str,
) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let from = saves_dir.join(old_name);
    let to = saves_dir.join(new_name);
    if !from.exists() {
        return Err(LauncherError::Instance(format!(
            "World '{}' not found",
            old_name
        )));
    }
    if to.exists() {
        return Err(LauncherError::Instance(format!(
            "A world named '{}' already exists",
            new_name
        )));
    }
    std::fs::rename(&from, &to).map_err(|e| LauncherError::Instance(e.to_string()))
}

pub fn copy_world(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
    new_name: &str,
) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let src = saves_dir.join(world_name);
    let dst = saves_dir.join(new_name);
    if !src.exists() {
        return Err(LauncherError::Instance(format!(
            "World '{}' not found",
            world_name
        )));
    }
    if dst.exists() {
        return Err(LauncherError::Instance(format!(
            "A world named '{}' already exists",
            new_name
        )));
    }
    copy_dir_recursive(&src, &dst)
}

pub fn delete_world(instances_dir: &Path, instance_name: &str, world_name: &str) -> Result<()> {
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let world_dir = saves_dir.join(world_name);
    if !world_dir.exists() {
        return Err(LauncherError::Instance(format!(
            "World '{}' not found",
            world_name
        )));
    }
    std::fs::remove_dir_all(&world_dir).map_err(|e| LauncherError::Instance(e.to_string()))
}

pub fn read_world_icon(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
) -> Option<String> {
    let instance = get_instance(instances_dir, instance_name).ok()?;
    let icon_path = instance
        .minecraft_dir(instances_dir)
        .join("saves")
        .join(world_name)
        .join("icon.png");
    if !icon_path.exists() {
        return None;
    }
    read_image_as_base64(&icon_path)
}

/// Metadata describing one stored world backup archive.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BackupInfo {
    pub filename: String,
    pub size_bytes: u64,
    pub created_at: i64,
}

/// Where backups of a world's saves are stored: `<instance>/.backups/worlds/<world>/`.
fn world_backups_dir(instances_dir: &Path, instance_name: &str, world_name: &str) -> PathBuf {
    instances_dir
        .join(instance_name)
        .join(".backups")
        .join("worlds")
        .join(world_name)
}

/// Reject any backup filename that is not a plain `.zip` file name inside the
/// backups directory (no traversal, separators, hidden files, or Windows ADS).
fn validate_backup_filename(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 200
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('\0')
        || name.contains("..")
        || name.starts_with('.')
        || !name.to_ascii_lowercase().ends_with(".zip")
    {
        return Err(LauncherError::Instance(format!(
            "Invalid backup filename: '{}'",
            name
        )));
    }
    Ok(())
}

/// Sibling `.part` path used while building a backup archive, so an
/// interrupted backup never appears in the backup list as a valid `.zip`.
fn backup_part_path(path: &std::path::Path) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(".part");
    PathBuf::from(p)
}

/// Zip a world save into `<instance>/.backups/worlds/<world>/<world>__<ts>.zip`
/// and return the resulting archive's metadata.
pub fn backup_world(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
) -> Result<BackupInfo> {
    use zip::write::SimpleFileOptions;
    let instance = get_instance(instances_dir, instance_name)?;
    let world_dir = instance
        .minecraft_dir(instances_dir)
        .join("saves")
        .join(world_name);
    if !world_dir.is_dir() {
        return Err(LauncherError::Instance(format!(
            "World '{}' not found",
            world_name
        )));
    }
    let backups_dir = world_backups_dir(instances_dir, instance_name, world_name);
    std::fs::create_dir_all(&backups_dir)?;
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut out_path = backups_dir.join(format!("{}__{}.zip", world_name, ts));
    // Two backups in the same second must not clobber each other.
    if out_path.exists() {
        out_path = backups_dir.join(format!("{}__{}_{}.zip", world_name, ts, std::process::id()));
    }
    let filename = out_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let part_path = backup_part_path(&out_path);
    let _ = std::fs::remove_file(&part_path);
    let built = (|| -> Result<()> {
        let file = std::fs::File::create(&part_path)?;
        let mut zip_w = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        add_dir_contents_to_zip(&mut zip_w, &world_dir, "", options, &|_| false)?;
        let file = zip_w.finish()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = built {
        let _ = std::fs::remove_file(&part_path);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&part_path, &out_path) {
        let _ = std::fs::remove_file(&part_path);
        return Err(e.into());
    }
    let size_bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    tracing::info!(target: "launcher", "Backed up world '{}' of '{}' to {:?}", world_name, instance_name, out_path);
    Ok(BackupInfo {
        filename,
        size_bytes,
        created_at: chrono::Local::now().timestamp(),
    })
}

/// List `*.zip` archives in a backup directory, newest first. A missing
/// directory yields an empty list. Shared by world and instance backups.
fn list_backup_dir(dir: &std::path::Path) -> Result<Vec<BackupInfo>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.to_ascii_lowercase().ends_with(".zip") {
            continue;
        }
        let meta = entry.metadata()?;
        let created_at = meta
            .created()
            .or_else(|_| meta.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(BackupInfo {
            filename: fname,
            size_bytes: meta.len(),
            created_at,
        });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    Ok(out)
}

/// List a world's backups, newest first. Missing directory yields an empty list.
pub fn list_world_backups(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
) -> Result<Vec<BackupInfo>> {
    list_backup_dir(&world_backups_dir(instances_dir, instance_name, world_name))
}

/// Restore a world from a backup archive. `mode` is `"replace"` (overwrite the
/// existing world) or `"as_new"` (extract into a new world named `new_name`).
pub fn restore_world(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
    backup_filename: &str,
    mode: &str,
    new_name: Option<&str>,
) -> Result<()> {
    validate_backup_filename(backup_filename)?;
    let instance = get_instance(instances_dir, instance_name)?;
    let saves_dir = instance.minecraft_dir(instances_dir).join("saves");
    let backup_path =
        world_backups_dir(instances_dir, instance_name, world_name).join(backup_filename);
    if !backup_path.is_file() {
        return Err(LauncherError::Instance(format!(
            "Backup '{}' not found",
            backup_filename
        )));
    }

    let target_world = match mode {
        "replace" => world_name.to_string(),
        "as_new" => {
            let n = new_name.unwrap_or("").trim();
            if n.is_empty()
                || n.len() > 64
                || n.contains('/')
                || n.contains('\\')
                || n.contains(':')
                || n.contains('\0')
                || n.contains("..")
            {
                return Err(LauncherError::Instance(format!(
                    "Invalid world name: '{}'",
                    n
                )));
            }
            n.to_string()
        }
        _ => {
            return Err(LauncherError::Instance(format!(
                "Unknown restore mode: '{}'",
                mode
            )))
        }
    };

    let target_dir = saves_dir.join(&target_world);
    let mc_dir = instance.minecraft_dir(instances_dir);
    crate::import::ensure_no_reparse_points(&mc_dir, &target_dir).map_err(|e| {
        LauncherError::Instance(format!(
            "Refusing to restore a world through a junction/symlink: {}",
            e
        ))
    })?;
    if mode != "replace" && target_dir.exists() {
        return Err(LauncherError::Instance(format!(
            "A world named '{}' already exists",
            target_world
        )));
    }

    // Extract into a temp directory FIRST and only swap it in when the whole
    // archive has extracted. Replacing the live world before extraction would
    // destroy it if the backup turns out to be corrupt (extract fails
    // midway). The temp dir lives under `.backups/` so a crash cannot leave
    // it looking like a playable world in the saves list.
    // Unique per operation: two concurrent restores (or a leftover from a
    // crashed run) must never share the same temp/old directories.
    let restore_token = uuid::Uuid::new_v4().simple().to_string();
    let tmp_dir = instances_dir
        .join(instance_name)
        .join(".backups")
        .join(format!(".restore-tmp-{}", restore_token));
    let instance_dir = instance.dir(instances_dir);
    crate::import::ensure_no_reparse_points(&instance_dir, &tmp_dir).map_err(|e| {
        LauncherError::Instance(format!(
            "Refusing to extract a world backup through a junction/symlink: {}",
            e
        ))
    })?;
    if tmp_dir.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    if let Err(e) = extract_zip_to_dir(&backup_path, &tmp_dir) {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    if target_dir.exists() {
        // Replace: park the old world next to the temp dir, move the new one
        // in, then drop the parked copy. A failed swap restores the original.
        let old_dir = instances_dir
            .join(instance_name)
            .join(".backups")
            .join(format!(".restore-old-{}", restore_token));
        if old_dir.exists() {
            let _ = std::fs::remove_dir_all(&old_dir);
        }
        std::fs::rename(&target_dir, &old_dir).map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            LauncherError::Instance(format!("Failed to replace world '{}': {}", target_world, e))
        })?;
        if let Err(e) = std::fs::rename(&tmp_dir, &target_dir) {
            let _ = std::fs::rename(&old_dir, &target_dir);
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(LauncherError::Instance(format!(
                "Failed to restore world '{}': {}",
                target_world, e
            )));
        }
        let _ = std::fs::remove_dir_all(&old_dir);
    } else if let Err(e) = std::fs::rename(&tmp_dir, &target_dir) {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(LauncherError::Instance(format!(
            "Failed to restore world '{}': {}",
            target_world, e
        )));
    }
    tracing::info!(
        target: "launcher",
        "Restored world '{}' of '{}' from '{}' (mode {}) as '{}'",
        world_name, instance_name, backup_filename, mode, target_world
    );
    Ok(())
}

/// Delete one stored world backup archive.
pub fn delete_world_backup(
    instances_dir: &Path,
    instance_name: &str,
    world_name: &str,
    backup_filename: &str,
) -> Result<()> {
    validate_backup_filename(backup_filename)?;
    let path = world_backups_dir(instances_dir, instance_name, world_name).join(backup_filename);
    if !path.is_file() {
        return Err(LauncherError::Instance(format!(
            "Backup '{}' not found",
            backup_filename
        )));
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

// ── Instance backup / restore ────────────────────────────────────

/// Where per-instance backup archives live: `<instance>/.backups/instances/`.
/// Inside the instance so backups travel with it, but never zipped into a
/// backup of that same instance (see `instance_backup_skip`).
fn instance_backups_dir(instances_dir: &Path, instance_name: &str) -> PathBuf {
    instances_dir
        .join(instance_name)
        .join(".backups")
        .join("instances")
}

/// What a whole-instance backup leaves out: per-launch logs, crash reports,
/// screenshots and the backups themselves (a backup must not contain itself).
/// Keeps archives small and avoids recursion into `.backups`.
fn instance_backup_skip(name: &str) -> bool {
    name == "logs"
        || name == "crash-reports"
        || name == "screenshots"
        || name == ".backups"
        || name.ends_with(".voidlauncher-old")
}

/// Extract a zip archive into `dest` with zip-slip protection: entries with
/// NUL, `:` (drive letters / NTFS streams), leading `/` or `..` segments are
/// rejected, and the final join must stay inside `dest`. A rejected entry
/// fails the whole restore so a corrupt archive is never applied partially.
fn extract_zip_to_dir(zip_path: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| LauncherError::Instance(format!("Invalid backup archive: {}", e)))?;
    std::fs::create_dir_all(dest)?;
    let mut extract_budget = crate::import::ExtractBudget::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| LauncherError::Instance(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name()?.replace('\\', "/");
        if name.is_empty()
            || name.contains('\0')
            || name.contains(':')
            || name.starts_with('/')
            || name.split('/').any(|c| c == "..")
        {
            return Err(LauncherError::Instance(format!(
                "Unsafe path in backup archive: {}",
                name
            )));
        }
        crate::import::write_extracted_budgeted(dest, &name, &mut entry, &mut extract_budget)?;
    }
    Ok(())
}

/// Zip an entire instance (metadata + `.minecraft`) into
/// `<instance>/.backups/instances/<name>__<ts>.zip`. Logs, crash reports,
/// screenshots and the `.backups` directory are excluded.
pub fn backup_instance(instances_dir: &Path, name: &str) -> Result<BackupInfo> {
    use zip::write::SimpleFileOptions;
    let _reservation = InstanceNameReservation::acquire(instances_dir, &[name])?;
    let instance = get_instance(instances_dir, name)?;
    let inst_dir = instance.dir(instances_dir);
    if !inst_dir.is_dir() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    let backups_dir = instance_backups_dir(instances_dir, name);
    std::fs::create_dir_all(&backups_dir)?;
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut out_path = backups_dir.join(format!("{}__{}.zip", name, ts));
    // Two backups in the same second must not clobber each other.
    if out_path.exists() {
        out_path = backups_dir.join(format!("{}__{}_{}.zip", name, ts, std::process::id()));
    }
    let filename = out_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let part_path = backup_part_path(&out_path);
    let _ = std::fs::remove_file(&part_path);
    let built = (|| -> Result<()> {
        let file = std::fs::File::create(&part_path)?;
        let mut zip_w = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        add_dir_contents_to_zip(&mut zip_w, &inst_dir, "", options, &instance_backup_skip)?;
        let file = zip_w.finish()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = built {
        let _ = std::fs::remove_file(&part_path);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&part_path, &out_path) {
        let _ = std::fs::remove_file(&part_path);
        return Err(e.into());
    }
    let size_bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    tracing::info!(target: "launcher", "Backed up instance '{}' to {:?}", name, out_path);
    Ok(BackupInfo {
        filename,
        size_bytes,
        created_at: chrono::Local::now().timestamp(),
    })
}

/// List an instance's backups, newest first. Missing directory yields empty.
pub fn list_instance_backups(instances_dir: &Path, name: &str) -> Result<Vec<BackupInfo>> {
    list_backup_dir(&instance_backups_dir(instances_dir, name))
}

/// Hidden directories used while a restore is in progress. A random UUID token
/// makes every operation's paths unique even when two restores start in the
/// same second (the old PID+timestamp scheme collided).
struct RestoreStaging {
    tmp: PathBuf,
    old: PathBuf,
}

fn new_restore_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn restore_staging_paths(instances_dir: &Path, token: &str) -> RestoreStaging {
    RestoreStaging {
        tmp: instances_dir.join(format!(".void-restore-tmp-{token}")),
        old: instances_dir.join(format!(".void-old-{token}")),
    }
}

/// Write the metadata files that live in an instance root into `dir`. Unlike
/// `save_instance`, `dir` may be a staging directory that is not named after
/// the instance yet.
fn write_instance_metadata_in_dir(dir: &Path, instance: &Instance) -> Result<()> {
    if !dir.is_dir() {
        std::fs::create_dir_all(dir).map_err(|e| {
            LauncherError::Instance(format!(
                "Could not create instance directory '{}': {e}",
                dir.display()
            ))
        })?;
    }
    let json = serde_json::to_string_pretty(instance)?;
    let write = |name: &str, contents: &[u8]| {
        write_file_atomic(&dir.join(name), contents).map_err(|e| {
            LauncherError::Instance(format!(
                "Could not write {name} into '{}': {e}",
                dir.display()
            ))
        })
    };
    write("instance.json", json.as_bytes())?;
    write("instance.cfg", build_prism_cfg_content(instance).as_bytes())?;
    write("mmc-pack.json", build_mmc_pack_json(instance)?.as_bytes())?;
    if let Some(icon) = instance.icon.as_deref() {
        if let Some(bytes) = icon.split(";base64,").nth(1).and_then(base64_decode) {
            let _ = std::fs::write(dir.join("pack.png"), bytes);
        }
    }
    Ok(())
}

/// Align the restored instance metadata with the directory it will live in.
/// A backup taken before a rename still contains the old name; leaving it
/// would make `Instance.dir()` point at a folder that no longer exists.
///
/// This runs on the extracted staging directory, BEFORE the swap, so a
/// metadata error can no longer destroy the live instance (the old code
/// swapped first and then tried to roll back, which itself could fail).
fn sync_restored_instance_name(dir: &Path, target_name: &str) -> Result<()> {
    let json_path = dir.join("instance.json");
    let cfg_path = dir.join("instance.cfg");

    let mut instance = if json_path.exists() {
        let contents = std::fs::read_to_string(&json_path)?;
        serde_json::from_str::<Instance>(&contents).map_err(|e| {
            LauncherError::Instance(format!(
                "Restored metadata for '{}' is invalid: {}",
                target_name, e
            ))
        })?
    } else if cfg_path.exists() {
        parse_prism_cfg(&cfg_path).ok_or_else(|| {
            LauncherError::Instance(format!(
                "Restored metadata for '{}' is invalid: cannot parse instance.cfg",
                target_name
            ))
        })?
    } else {
        return Err(LauncherError::Instance(format!(
            "Restored instance '{}' has no instance.json or instance.cfg",
            target_name
        )));
    };

    instance.name = target_name.to_string();
    write_instance_metadata_in_dir(dir, &instance)
}

/// Indirection over `std::fs::rename` so the restore swap can be tested with
/// the exact Windows failure users hit (`os error 5`, Access Denied).
/// Production always calls the OS; tests inject a wrapper that fails at one
/// specific stage.
trait RenameOp {
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
}

struct OsRename;

impl RenameOp for OsRename {
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }
}

/// Rename attempts during a restore swap. Windows refuses to rename a
/// directory while any child file is held without delete sharing — antivirus
/// scans, the search indexer, Explorer previews and the game itself all do
/// that. A bounded wait lets a transient lock clear instead of aborting a
/// restore that would succeed a moment later.
const RENAME_ATTEMPTS: u32 = 10;
const RENAME_BASE_DELAY_MS: u64 = 100;
const RENAME_MAX_DELAY_MS: u64 = 1200;

fn rename_with_retry(op: &dyn RenameOp, from: &Path, to: &Path, context: &str) -> Result<()> {
    rename_with_retry_policy(
        op,
        from,
        to,
        context,
        RENAME_ATTEMPTS,
        RENAME_BASE_DELAY_MS,
        RENAME_MAX_DELAY_MS,
    )
}

fn rename_with_retry_policy(
    op: &dyn RenameOp,
    from: &Path,
    to: &Path,
    context: &str,
    attempts: u32,
    base_delay_ms: u64,
    max_delay_ms: u64,
) -> Result<()> {
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..attempts {
        match op.rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let retryable = matches!(e.kind(), std::io::ErrorKind::PermissionDenied)
                    || e.raw_os_error() == Some(32);
                let detail = e.to_string();
                last = Some(e);
                if !retryable || attempt + 1 == attempts {
                    break;
                }
                let delay = base_delay_ms
                    .saturating_mul(1u64 << attempt.min(6))
                    .min(max_delay_ms);
                tracing::debug!(
                    target: "launcher",
                    "{} attempt {}/{} failed for '{}': {}; retrying in {} ms",
                    context,
                    attempt + 1,
                    attempts,
                    from.display(),
                    detail,
                    delay
                );
                std::thread::sleep(Duration::from_millis(delay));
            }
        }
    }
    let detail = last
        .map(|e| e.to_string())
        .unwrap_or_else(|| "unknown error".to_string());
    Err(LauncherError::Instance(format!(
        "{context} failed for '{}' (after {attempts} attempts): {detail}",
        from.display()
    )))
}

/// `remove_dir_all` that first clears Windows read-only attributes when the
/// direct attempt fails. Files restored from an archive can carry the
/// read-only flag, which made cleanup fail with Access Denied and left the
/// restore looking broken after it had already succeeded.
fn remove_dir_all_force(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(first) => {
            clear_readonly_recursive(path);
            std::fs::remove_dir_all(path).map_err(|_| first)
        }
    }
}

fn clear_readonly_recursive(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.permissions().readonly() {
        let mut perms = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(path, perms);
    }
    if meta.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                clear_readonly_recursive(&entry.path());
            }
        }
    }
}

/// A directory that can be listed and launched by the instance manager: it
/// must exist and contain one of the metadata formats the launcher reads.
fn has_instance_metadata(dir: &Path) -> bool {
    dir.is_dir() && (dir.join("instance.json").is_file() || dir.join("instance.cfg").is_file())
}

/// Restore an instance from a backup archive. `mode` is `"replace"` (rebuild
/// the instance from the archive, preserving its `.backups` history) or
/// `"as_new"` (extract into a new instance named `new_name`; the original is
/// untouched). The archive is fully extracted to a temp directory first — only
/// a valid instance archive (has `instance.json` or `instance.cfg`) swaps in,
/// so a corrupt backup never destroys the existing instance.
pub fn restore_instance(
    instances_dir: &Path,
    name: &str,
    backup_filename: &str,
    mode: &str,
    new_name: Option<&str>,
) -> Result<()> {
    restore_instance_with(
        instances_dir,
        name,
        backup_filename,
        mode,
        new_name,
        &OsRename,
    )
}

fn restore_instance_with(
    instances_dir: &Path,
    name: &str,
    backup_filename: &str,
    mode: &str,
    new_name: Option<&str>,
    rename: &dyn RenameOp,
) -> Result<()> {
    validate_instance_dir_name(name, "Instance name")?;
    validate_backup_filename(backup_filename)?;
    // No `get_instance` here on purpose: restore must work even when the
    // live instance's metadata is what got damaged.
    let inst_dir = instances_dir.join(name);
    let backup_path = instance_backups_dir(instances_dir, name).join(backup_filename);
    if !inst_dir.is_dir() {
        return Err(LauncherError::Instance(format!(
            "Instance '{}' not found",
            name
        )));
    }
    if !backup_path.is_file() {
        return Err(LauncherError::Instance(format!(
            "Backup '{}' not found",
            backup_filename
        )));
    }

    let target_name = match mode {
        "replace" => name.to_string(),
        "as_new" => {
            let n = new_name.unwrap_or("");
            if n.chars().count() > 64 {
                return Err(LauncherError::Instance(format!(
                    "Invalid instance name: '{}'",
                    n
                )));
            }
            validate_instance_dir_name(n, "Instance name")?;
            n.to_string()
        }
        _ => {
            return Err(LauncherError::Instance(format!(
                "Unknown restore mode: '{}'",
                mode
            )))
        }
    };
    // Reserve the source and target before checking the target. This closes
    // the gap between "does not exist" and the directory swap.
    let _reservation = InstanceNameReservation::acquire(instances_dir, &[name, &target_name])?;
    let target_dir = instances_dir.join(&target_name);
    if mode == "as_new" && target_dir.exists() {
        return Err(LauncherError::Instance(format!(
            "An instance named '{}' already exists",
            target_name
        )));
    }

    // 1. Extract fully to a hidden sibling temp dir — failures here leave the
    //    existing instance untouched. Dot-prefixed and lacking instance.json,
    //    so `list_instances` ignores it even if a crash leaves it behind.
    //    A fresh UUID per operation prevents two restores from sharing (and
    //    then deleting) each other's staging paths.
    let staging = restore_staging_paths(instances_dir, &new_restore_token());
    let tmp = &staging.tmp;
    if tmp.exists() {
        let _ = remove_dir_all_force(tmp);
    }
    if let Err(e) = extract_zip_to_dir(&backup_path, tmp) {
        let _ = remove_dir_all_force(tmp);
        return Err(LauncherError::Instance(format!(
            "Could not extract backup '{}': {e}",
            backup_filename
        )));
    }
    if !tmp.join("instance.json").exists() && !tmp.join("instance.cfg").exists() {
        let _ = remove_dir_all_force(tmp);
        return Err(LauncherError::Instance(
            "Archive does not look like a VoidLauncher or Prism instance".into(),
        ));
    }

    // 2. Re-label the extracted metadata while it is still in staging. The
    //    archive may predate a rename, and `Instance.dir()` derives the folder
    //    from this field. Doing it before the swap means a metadata failure
    //    simply discards the staging copy and leaves the live instance intact.
    if let Err(e) = sync_restored_instance_name(tmp, &target_name) {
        let _ = remove_dir_all_force(tmp);
        return Err(e);
    }

    // 3. Swap it in.
    if mode == "replace" {
        let old_dir = &staging.old;
        if old_dir.exists() {
            let _ = remove_dir_all_force(old_dir);
        }
        if let Err(e) = rename_with_retry(
            rename,
            &inst_dir,
            old_dir,
            "Moving the current instance aside",
        ) {
            let _ = remove_dir_all_force(tmp);
            return Err(LauncherError::Instance(format!(
                "{e}. Nothing was changed - close the game, Explorer windows and any antivirus scan using '{}', then retry.",
                inst_dir.display()
            )));
        }
        if let Err(e) =
            rename_with_retry(rename, tmp, &target_dir, "Installing the restored instance")
        {
            // Roll back to the original instance. If even that fails, keep
            // the original directory for manual recovery; the staging copy is
            // never installed and is safe to discard.
            match rename_with_retry(
                rename,
                old_dir,
                &inst_dir,
                "Restoring the original instance",
            ) {
                Ok(()) => {
                    let _ = remove_dir_all_force(tmp);
                }
                Err(rollback) => {
                    tracing::error!(
                        target: "launcher",
                        "Restore swap failed ({}), rollback failed ({}); original kept at {}",
                        e, rollback, old_dir.display()
                    );
                    // The original is safe in `old_dir`; the staging copy was
                    // never installed, so removing it cannot lose user data.
                    let _ = remove_dir_all_force(tmp);
                    return Err(LauncherError::Instance(format!(
                        "{e}; the original instance is preserved at '{}'",
                        old_dir.display()
                    )));
                }
            }
            return Err(e);
        }

        // The restored directory must be a usable instance before the old
        // copy is touched, let alone deleted.
        let restored_ok = has_instance_metadata(&target_dir);
        if !restored_ok {
            let bad = LauncherError::Instance(format!(
                "Restored instance '{}' is incomplete after the swap",
                target_name
            ));
            if remove_dir_all_force(&target_dir).is_err() {
                tracing::error!(
                    target: "launcher",
                    "Could not remove the incomplete restored instance {}; previous instance kept at {}",
                    target_dir.display(), old_dir.display()
                );
                return Err(bad);
            }
            if let Err(rollback) = rename_with_retry(
                rename,
                old_dir,
                &inst_dir,
                "Restoring the original instance",
            ) {
                tracing::error!(
                    target: "launcher",
                    "Restore verification failed ({}), rollback failed ({}); original kept at {}",
                    bad, rollback, old_dir.display()
                );
                return Err(LauncherError::Instance(format!(
                    "{}; the original instance is preserved at '{}'",
                    bad,
                    old_dir.display()
                )));
            }
            return Err(bad);
        }

        // 4. Move the preserved backup history out of the old directory. It
        //    contains the archive being restored, so this must happen after
        //    the swap: a failure can then never leave it in a directory that
        //    no longer exists.
        let old_backups = old_dir.join(".backups");
        let new_backups = target_dir.join(".backups");
        let mut backup_history_moved = true;
        if old_backups.is_dir() {
            if !new_backups.exists() {
                if let Err(e) =
                    rename_with_retry(rename, &old_backups, &new_backups, "Moving backup history")
                {
                    tracing::error!(
                        target: "launcher",
                        "Restore succeeded but backup history could not be moved; it is kept at {}: {}",
                        old_backups.display(), e
                    );
                    backup_history_moved = false;
                }
            } else {
                // Extremely unlikely (a backup containing `.backups`), but
                // keep both histories instead of losing one.
                match std::fs::read_dir(&old_backups) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            let dest = new_backups.join(entry.file_name());
                            if let Err(e) = rename_with_retry(
                                rename,
                                &entry.path(),
                                &dest,
                                "Merging backup history",
                            ) {
                                tracing::warn!(
                                    target: "launcher",
                                    "Could not merge preserved backup {:?}: {}",
                                    entry.path(), e
                                );
                                backup_history_moved = false;
                            }
                        }
                        if backup_history_moved {
                            let _ = remove_dir_all_force(&old_backups);
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            target: "launcher",
                            "Backup history kept at {} (could not read it): {}",
                            old_backups.display(), e
                        );
                        backup_history_moved = false;
                    }
                }
            }
        }

        // Only delete the old copy once the restored instance (and, when
        // possible, its backup history) is safely in place. If cleanup is
        // blocked, the hidden directory stays — a later launch ignores it and
        // nothing is lost.
        if backup_history_moved {
            if let Err(e) = remove_dir_all_force(old_dir) {
                tracing::warn!(
                    target: "launcher",
                    "Restored instance is in place, but the previous copy could not be fully removed ({}); it remains hidden at {}",
                    e, old_dir.display()
                );
            }
        }
    } else if let Err(e) =
        rename_with_retry(rename, tmp, &target_dir, "Installing the restored instance")
    {
        let _ = remove_dir_all_force(tmp);
        return Err(e);
    } else if !has_instance_metadata(&target_dir) {
        // Same guarantee as replace: never report success for a directory
        // the instance manager cannot load.
        let _ = remove_dir_all_force(&target_dir);
        return Err(LauncherError::Instance(format!(
            "Restored instance '{}' is incomplete after the swap",
            target_name
        )));
    }

    tracing::info!(
        target: "launcher",
        "Restored instance '{}' from '{}' (mode {}) as '{}'",
        name, backup_filename, mode, target_name
    );
    Ok(())
}

/// Delete one stored instance backup archive.
pub fn delete_instance_backup(
    instances_dir: &Path,
    name: &str,
    backup_filename: &str,
) -> Result<()> {
    validate_backup_filename(backup_filename)?;
    let path = instance_backups_dir(instances_dir, name).join(backup_filename);
    if !path.is_file() {
        return Err(LauncherError::Instance(format!(
            "Backup '{}' not found",
            backup_filename
        )));
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn new_instance_inherits_global_memory_and_gc() {
        let instance = Instance::new("Inherited", "1.20.1", LoaderType::Vanilla, None);
        assert_eq!(instance.memory_mb, None, "must inherit global memory");
        assert_eq!(instance.gc_preset, None, "must inherit global GC");
    }

    #[test]
    fn effective_memory_and_gc_inherit_independently() {
        let mut instance = Instance::new("Inherited", "1.20.1", LoaderType::Vanilla, None);
        assert_eq!(instance.effective_memory_mb(4096, 8192), 4096);
        assert_eq!(instance.effective_gc_preset("g1gc"), "g1gc");

        instance.memory_mb = Some(6144);
        assert_eq!(instance.effective_memory_mb(4096, 8192), 6144);
        assert_eq!(
            instance.effective_gc_preset("g1gc"),
            "g1gc",
            "memory override must not pin the GC default"
        );

        instance.gc_preset = Some("zgc".into());
        assert_eq!(instance.effective_memory_mb(4096, 8192), 6144);
        assert_eq!(instance.effective_gc_preset("g1gc"), "zgc");

        instance.memory_mb = None;
        assert_eq!(instance.effective_memory_mb(8192, 8192), 8192);
    }

    #[test]
    fn per_instance_memory_is_clamped_to_safe_bounds_and_global_limit() {
        let mut instance = Instance::new("Bounded", "1.20.1", LoaderType::Vanilla, None);

        // 0 and absurd values must never reach -Xmx.
        instance.memory_mb = Some(0);
        assert_eq!(instance.effective_memory_mb(4096, 8192), 2048);
        instance.memory_mb = Some(1_000_000);
        assert_eq!(instance.effective_memory_mb(4096, 8192), 8192);
        instance.memory_mb = Some(16_384);
        assert_eq!(instance.effective_memory_mb(4096, 8192), 8192);

        // A global default below the floor is also clamped.
        instance.memory_mb = None;
        assert_eq!(instance.effective_memory_mb(0, 8192), 2048);

        // Persisted values are normalized to the same absolute bounds.
        instance.memory_mb = Some(16_384);
        assert_eq!(instance.normalized_memory_mb(8192), Some(8192));
        assert_eq!(instance.normalized_memory_mb(32768), Some(16_384));
        instance.memory_mb = None;
        assert_eq!(instance.normalized_memory_mb(8192), None);
    }

    #[test]
    fn legacy_or_corrupt_memory_json_does_not_break_instance_loading() {
        // Negative / zero / absurd / null values must be tolerated instead of
        // failing the whole instance.json deserialize.
        for (raw, expected) in [
            ("-4096", None),
            ("0", None),
            ("null", None),
            ("999999999999", Some(crate::config::MAX_MEMORY_MB)),
            ("6144", Some(6144)),
        ] {
            let json = format!(
                r#"{{"name":"Old","mc_version":"1.20.1","loader":"Vanilla","loader_version":null,
                   "loader_profile":null,"memory_mb":{raw},"jvm_args":null,"java_path":null,
                   "resolution":null,"icon":null,"created_at":"2024-01-01T00:00:00Z",
                   "last_played":null,"play_time_seconds":0,"notes":""}}"#
            );
            let loaded: Instance =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("raw {raw}: {e}"));
            assert_eq!(loaded.memory_mb, expected, "raw value {raw}");
            assert!(loaded.effective_memory_mb(4096, 8192) >= 2048);
        }
    }

    #[test]
    fn explicit_instance_memory_and_gc_survive_roundtrip() {
        let mut instance = Instance::new("Custom", "1.20.1", LoaderType::Vanilla, None);
        instance.memory_mb = Some(6144);
        instance.gc_preset = Some("zgc".into());

        let json = serde_json::to_string(&instance).unwrap();
        let loaded: Instance = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.memory_mb, Some(6144));
        assert_eq!(loaded.gc_preset.as_deref(), Some("zgc"));
    }

    #[test]
    fn legacy_instance_without_memory_or_gc_fields_inherits() {
        let json = r#"{
            "name": "Legacy",
            "mc_version": "1.20.1",
            "loader": "Vanilla",
            "loader_version": null,
            "loader_profile": null,
            "jvm_args": null,
            "java_path": null,
            "resolution": null,
            "icon": null,
            "created_at": "2026-01-01T00:00:00Z",
            "last_played": null,
            "play_time_seconds": 0,
            "notes": ""
        }"#;
        let loaded: Instance = serde_json::from_str(json).unwrap();
        assert_eq!(loaded.memory_mb, None);
        assert_eq!(loaded.gc_preset, None);
    }

    fn build_prism_zip(path: &Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let cfg = "[General]\nConfigVersion=1.3\nInstanceType=OneSix\nname=Test Pack\n";
        zip_w.start_file("instance.cfg", opts).unwrap();
        zip_w.write_all(cfg.as_bytes()).unwrap();

        let pack = r#"{
            "components": [
                {"uid": "org.lwjgl3", "version": "3.4.1", "dependencyOnly": true},
                {"uid": "net.minecraft", "version": "1.20.1", "important": true},
                {"uid": "net.fabricmc.intermediary", "version": "1.20.1", "dependencyOnly": true},
                {"uid": "net.fabricmc.fabric-loader", "version": "0.15.11"}
            ],
            "formatVersion": 1
        }"#;
        zip_w.start_file("mmc-pack.json", opts).unwrap();
        zip_w.write_all(pack.as_bytes()).unwrap();

        for (name, bytes) in [
            ("minecraft/mods/testmod.jar", b"PK\x03\x04fakejar".to_vec()),
            (
                "minecraft/mods/DisabledMod-1.2.3.jar.disabled",
                b"PK\x03\x04fakedisabled".to_vec(),
            ),
            (
                "minecraft/mods/.index/disabledmod.pw.toml",
                br#"filename = 'DisabledMod-1.2.3.jar'
name = 'Disabled Mod'
side = 'both'
x-prismlauncher-version-number = '1.2.3'

[download]
hash = 'abc'
hash-format = 'sha1'
mode = 'metadata:modrinth'

[update.modrinth]
mod-id = 'disabledmod'
version = '1.2.3'
"#
                .to_vec(),
            ),
            (
                "minecraft/resourcepacks/rp.zip",
                b"PK\x03\x04fakerp".to_vec(),
            ),
            ("minecraft/shaderpacks/sh.zip", b"PK\x03\x04fakesh".to_vec()),
            ("minecraft/config/x.yml", b"key: value".to_vec()),
            ("minecraft/.bobby/junk.mca", b"junk".to_vec()),
            ("minecraft/.mixin.out/junk.class", b"junk".to_vec()),
            ("minecraft/crash-reports/crash.txt", b"junk".to_vec()),
            ("patches/forge.json", b"{}".to_vec()),
            ("icon.png", b"\x89PNG\r\n\x1a\nfakepng".to_vec()),
        ] {
            zip_w.start_file(name, opts).unwrap();
            zip_w.write_all(&bytes).unwrap();
        }

        zip_w.finish().unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_prism_import_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn vanilla_instance(name: &str) -> Instance {
        Instance {
            name: name.into(),
            mc_version: "1.20.1".into(),
            loader: LoaderType::Vanilla,
            loader_version: None,
            loader_profile: None,
            memory_mb: None,
            jvm_args: None,
            gc_preset: None,
            java_path: None,
            resolution: None,
            icon: None,
            banner: None,
            created_at: Utc::now().to_rfc3339(),
            last_played: None,
            play_time_seconds: 0,
            notes: String::new(),
        }
    }

    #[test]
    fn delete_screenshot_rejects_path_traversal() {
        let dir = temp_dir("shots");
        let instances_dir = dir.join("instances");
        let instance = Instance {
            name: "Test".into(),
            mc_version: "1.20.1".into(),
            loader: LoaderType::Vanilla,
            loader_version: None,
            loader_profile: None,
            memory_mb: None,
            jvm_args: None,
            gc_preset: None,
            java_path: None,
            resolution: None,
            icon: None,
            banner: None,
            created_at: Utc::now().to_rfc3339(),
            last_played: None,
            play_time_seconds: 0,
            notes: String::new(),
        };
        save_instance(&instances_dir, &instance, None).unwrap();

        let screenshots_dir = instance.minecraft_dir(&instances_dir).join("screenshots");
        std::fs::create_dir_all(&screenshots_dir).unwrap();
        std::fs::write(screenshots_dir.join("shot.png"), b"png").unwrap();
        // Decoy files that must NOT be deletable by a crafted screenshot name.
        let decoy = instance.dir(&instances_dir).join("decoy.txt");
        std::fs::write(&decoy, b"keep").unwrap();
        let cfg_decoy = instance.config_file(&instances_dir);
        let cfg_ok = std::fs::read(&cfg_decoy).unwrap();

        for evil in [
            "../decoy.txt",
            "..\\decoy.txt",
            "C:/Windows/decoy.txt",
            "C:\\Windows\\decoy.txt",
            "/decoy.txt",
            "\\decoy.txt",
            "../instance.json",
            "..\\instance.json",
            "..",
            ".",
            "",
        ] {
            assert!(
                delete_screenshot(&instances_dir, "Test", evil).is_err(),
                "name {:?} must be rejected",
                evil
            );
        }
        assert!(decoy.exists(), "decoy must survive");
        assert_eq!(
            std::fs::read(&cfg_decoy).unwrap(),
            cfg_ok,
            "instance.json must survive"
        );

        delete_screenshot(&instances_dir, "Test", "shot.png").unwrap();
        assert!(!screenshots_dir.join("shot.png").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prism_import_maps_game_dir_and_loader() {
        let dir = temp_dir("map");
        let zip_path = dir.join("pack.zip");
        build_prism_zip(&zip_path);

        let instances_dir = dir.join("instances");
        let instance = import_prism_pack(
            &instances_dir,
            zip_path.to_str().unwrap(),
            &crate::launch_state::LaunchRegistry::new(),
        )
        .unwrap();

        assert_eq!(instance.name, "Test Pack");
        assert_eq!(instance.mc_version, "1.20.1");
        assert_eq!(instance.loader, LoaderType::Fabric);
        assert_eq!(instance.loader_version.as_deref(), Some("0.15.11"));
        assert!(instance.icon.is_some(), "root icon should be imported");

        let mc_dir = instances_dir.join("Test Pack").join(".minecraft");
        assert!(
            mc_dir.join("mods/testmod.jar").exists(),
            "mods mapped to .minecraft/mods"
        );
        let disabled = mc_dir.join("mods/DisabledMod-1.2.3.jar.disabled");
        assert!(disabled.exists(), "disabled state preserved on import");
        assert!(
            !mc_dir.join("mods/DisabledMod-1.2.3.jar").exists(),
            "import must not silently enable a disabled mod"
        );
        let index = load_packwiz_index(&mc_dir.join("mods"));
        let disabled_meta = packwiz_meta(&index, "DisabledMod-1.2.3.jar.disabled")
            .expect("disabled file must reuse the enabled file's metadata");
        assert_eq!(disabled_meta.name, "Disabled Mod");
        assert_eq!(disabled_meta.version, "1.2.3");
        assert_eq!(disabled_meta.provider, "Modrinth");
        assert!(mc_dir.join("mods/.index/disabledmod.pw.toml").exists());
        assert!(mc_dir.join("resourcepacks/rp.zip").exists());
        assert!(mc_dir.join("shaderpacks/sh.zip").exists());
        assert!(mc_dir.join("config/x.yml").exists());
        assert!(!mc_dir.join(".bobby").exists(), "launch junk skipped");
        assert!(!mc_dir.join(".mixin.out").exists(), "mixin dump skipped");
        assert!(
            !mc_dir.join("crash-reports").exists(),
            "crash reports skipped"
        );
        assert!(
            !instances_dir.join("Test Pack").join("patches").exists(),
            "patches skipped"
        );
        assert!(
            instances_dir
                .join("Test Pack")
                .join("instance.json")
                .exists(),
            "instance.json written"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn packwiz_index_parses_modrinth_curseforge_and_local_meta() {
        let dir = temp_dir("pw");
        let index_dir = dir.join(".index");
        std::fs::create_dir_all(&index_dir).unwrap();

        std::fs::write(
            index_dir.join("emf.pw.toml"),
            r#"filename = "entity_model_features-3.2.6-26.2-fabric.jar"
name = "Entity Model Features"
side = "both"
x-prismlauncher-loaders = ["fabric"]
x-prismlauncher-mc-versions = ["26.2"]
x-prismlauncher-version-number = "3.2.6"
x-prismlauncher-release-type = "release"

[download]
hash = "abc"
hash-format = "sha1"
mode = "metadata:modrinth"
url = "https://cdn.modrinth.com/data/P7dR8mSH/versions/0/emf.jar"

[update.modrinth]
mod-id = "P7dR8mSH"
version = "3.2.6"
"#,
        )
        .unwrap();

        std::fs::write(
            index_dir.join("cf.pw.toml"),
            r#"filename = "sodium-0.6.0.jar"
name = "Sodium"
side = "both"

[download]
hash = "def"
hash-format = "sha512"
mode = "metadata:curseforge"
url = "https://www.curseforge.com/minecraft/mc-mods/sodium/download/5123456"

[update.curseforge]
file-id = 5123456
project-id = 394468
"#,
        )
        .unwrap();

        std::fs::write(
            index_dir.join("local.pw.toml"),
            r#"filename = "mymod.jar"
name = "My Mod"
side = "both"

[download]
hash = "123"
hash-format = "sha1"
mode = "url"
url = "https://example.com/mymod.jar"
"#,
        )
        .unwrap();

        // Prism exports shader packs' metadata in the content dir ROOT (not .index/)
        std::fs::write(
            dir.join("bsl-shaders.pw.toml"),
            r#"filename = 'BSL_v10.1.3.zip'
name = 'BSL Shaders'
side = 'client'
x-prismlauncher-version-number = '10.1.3'
x-prismlauncher-release-type = 'release'

[download]
hash = 'abc'
hash-format = 'sha512'
mode = 'url'
url = 'https://cdn.modrinth.com/data/Q1vvjJYV/versions/hIibTfxn/BSL_v10.1.3.zip'

[update.modrinth]
mod-id = 'Q1vvjJYV'
version = 'hIibTfxn'
"#,
        )
        .unwrap();

        let index = load_packwiz_index(&dir);
        assert_eq!(index.len(), 4);

        let bsl = index.get("BSL_v10.1.3.zip").unwrap();
        assert_eq!(bsl.name, "BSL Shaders");
        assert_eq!(bsl.version, "10.1.3");
        assert_eq!(bsl.provider, "Modrinth");
        assert_eq!(bsl.project_id, "Q1vvjJYV");

        let emf = index
            .get("entity_model_features-3.2.6-26.2-fabric.jar")
            .unwrap();
        assert_eq!(emf.name, "Entity Model Features");
        assert_eq!(emf.version, "3.2.6");
        assert_eq!(emf.provider, "Modrinth");
        assert_eq!(emf.project_id, "P7dR8mSH");

        let cf = index.get("sodium-0.6.0.jar").unwrap();
        assert_eq!(cf.name, "Sodium");
        assert_eq!(cf.provider, "CurseForge");
        assert_eq!(cf.project_id, "394468");

        let local = index.get("mymod.jar").unwrap();
        assert_eq!(local.provider, "");
        assert_eq!(local.project_id, "");
        assert_eq!(local.version, "");

        // Our own .voidlauncher.json sidecars in the same folder are ignored
        std::fs::write(index_dir.join("sodium-0.6.0.voidlauncher.json"), "{}").unwrap();
        let index2 = load_packwiz_index(&dir);
        assert_eq!(index2.len(), 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn packwiz_index_cache_invalidates_when_metadata_changes() {
        let dir = temp_dir("pw_cache");
        let index_dir = dir.join(".index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let meta_path = index_dir.join("demo.pw.toml");
        std::fs::write(
            &meta_path,
            "filename = 'demo.jar'\nname = 'Old Name'\n[update.modrinth]\nmod-id = 'abc'\n",
        )
        .unwrap();

        let first = load_packwiz_index_cached(&dir);
        assert_eq!(first.get("demo.jar").unwrap().name, "Old Name");

        std::fs::write(
            &meta_path,
            "filename = 'demo.jar'\nname = 'New Name With More Detail'\n[update.modrinth]\nmod-id = 'abc'\n",
        )
        .unwrap();
        let second = load_packwiz_index_cached(&dir);
        assert_eq!(
            second.get("demo.jar").unwrap().name,
            "New Name With More Detail"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn world_size_cache_invalidates_when_world_metadata_changes() {
        let dir = temp_dir("world_size_cache");
        let world = dir.join("World");
        std::fs::create_dir_all(world.join("region")).unwrap();
        std::fs::write(world.join("level.dat"), b"1234").unwrap();
        std::fs::write(world.join("region").join("r.0.0.mca"), b"abcd").unwrap();

        assert_eq!(cached_world_size(&world, None), 8);
        assert_eq!(cached_world_size(&world, None), 8);

        std::fs::write(world.join("level.dat"), b"12345678").unwrap();
        assert_eq!(cached_world_size(&world, None), 12);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_default_name_matches_the_archive_cfg() {
        let dir = temp_dir("prism_default_name");
        let zip_path = dir.join("pack.zip");
        build_prism_zip(&zip_path);
        assert_eq!(
            prism_pack_default_name(zip_path.to_str().unwrap()).unwrap(),
            "Test Pack"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_import_rejects_packs_without_mmc_pack_json() {
        let dir = temp_dir("nocfg");
        let zip_path = dir.join("pack.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip_w.start_file("instance.cfg", opts).unwrap();
        zip_w.write_all(b"[General]\nname=Old Pack\n").unwrap();
        zip_w.start_file("minecraft/mods/old.jar", opts).unwrap();
        zip_w.write_all(b"PK\x03\x04old").unwrap();
        zip_w.finish().unwrap();

        let instances_dir = dir.join("instances");
        let err = import_prism_pack(
            &instances_dir,
            zip_path.to_str().unwrap(),
            &crate::launch_state::LaunchRegistry::new(),
        )
        .expect_err("a Prism pack without version metadata must not become Vanilla");
        assert!(
            err.to_string().contains("mmc-pack.json"),
            "error must name the missing metadata: {err}"
        );
        assert!(
            !instances_dir.join("Old Pack").exists(),
            "the failed import must not leave an empty-version instance"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_folder_without_version_metadata_is_not_presented_as_vanilla() {
        let dir = temp_dir("prism_folder_no_pack");
        let inst = dir.join("Broken");
        std::fs::create_dir_all(&inst).unwrap();
        std::fs::write(inst.join("instance.cfg"), b"[General]\nname=Broken\n").unwrap();
        assert!(
            parse_prism_cfg(&inst.join("instance.cfg")).is_none(),
            "a cfg without mmc-pack.json cannot yield a launchable version"
        );

        std::fs::write(
            inst.join("mmc-pack.json"),
            br#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.fabricmc.fabric-loader","version":"0.15.11"}],"formatVersion":1}"#,
        )
        .unwrap();
        let parsed = parse_prism_cfg(&inst.join("instance.cfg")).expect("valid Prism folder");
        assert_eq!(parsed.mc_version, "1.20.1");
        assert_eq!(parsed.loader, LoaderType::Fabric);
        assert_eq!(parsed.loader_version.as_deref(), Some("0.15.11"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_import_reads_version_loader_memory_and_jvm_args() {
        let dir = temp_dir("prism_settings");
        let zip_path = dir.join("pack.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip_w.start_file("instance.cfg", opts).unwrap();
        zip_w.write_all(
            b"[General]\nname=Settings Pack\nOverrideMemory=true\nMaxMemAlloc=6144\nOverrideJavaArgs=true\nJvmArgs=-XX:+UseG1GC -Dfoo=bar\n",
        )
        .unwrap();
        zip_w.start_file("mmc-pack.json", opts).unwrap();
        zip_w
            .write_all(
                br#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.fabricmc.fabric-loader","version":"0.15.11"}],"formatVersion":1}"#,
            )
            .unwrap();
        zip_w.finish().unwrap();

        let instances_dir = dir.join("instances");
        let instance = import_prism_pack(
            &instances_dir,
            zip_path.to_str().unwrap(),
            &crate::launch_state::LaunchRegistry::new(),
        )
        .unwrap();
        assert_eq!(instance.mc_version, "1.20.1");
        assert_eq!(instance.loader, LoaderType::Fabric);
        assert_eq!(instance.memory_mb, Some(6144));
        assert_eq!(
            instance.jvm_args.as_deref(),
            Some(["-XX:+UseG1GC".to_string(), "-Dfoo=bar".to_string()].as_slice())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_instance(name: &str, mc: &str, loader: LoaderType, lv: Option<&str>) -> Instance {
        Instance {
            name: name.into(),
            mc_version: mc.into(),
            loader,
            loader_version: lv.map(String::from),
            loader_profile: None,
            memory_mb: None,
            jvm_args: None,
            gc_preset: None,
            java_path: None,
            resolution: None,
            icon: None,
            banner: None,
            created_at: Utc::now().to_rfc3339(),
            last_played: None,
            play_time_seconds: 0,
            notes: String::new(),
        }
    }

    #[test]
    fn save_instance_writes_prism_metadata_files() {
        let dir = temp_dir("save_meta");
        let instances_dir = dir.join("instances");
        let instance = test_instance("MetaPack", "1.21.1", LoaderType::NeoForge, Some("21.1.135"));
        save_instance(&instances_dir, &instance, None).unwrap();

        let root = instances_dir.join("MetaPack");
        assert!(root.join("instance.cfg").exists());
        assert!(root.join("mmc-pack.json").exists());

        let pack: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("mmc-pack.json")).unwrap())
                .unwrap();
        let comps = pack["components"].as_array().unwrap();
        assert_eq!(comps[0]["uid"], "net.minecraft");
        assert_eq!(comps[0]["version"], "1.21.1");
        assert_eq!(comps[1]["uid"], "net.neoforged");
        assert_eq!(comps[1]["version"], "21.1.135");

        let cfg = std::fs::read_to_string(root.join("instance.cfg")).unwrap();
        assert!(
            cfg.contains("InstanceType=OneSix"),
            "cfg must mark modern instance: {}",
            cfg
        );
        assert!(cfg.contains("name=MetaPack"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_export_writes_prism_layout() {
        let dir = temp_dir("export_layout");
        let instances_dir = dir.join("instances");
        let instance = test_instance("ExportPack", "1.20.1", LoaderType::Fabric, Some("0.15.11"));
        save_instance(&instances_dir, &instance, None).unwrap();

        let mc_dir = instance.minecraft_dir(&instances_dir);
        std::fs::create_dir_all(mc_dir.join("mods")).unwrap();
        std::fs::write(mc_dir.join("mods/testmod.jar"), b"PK\x03\x04jar").unwrap();
        std::fs::create_dir_all(mc_dir.join("logs")).unwrap();
        std::fs::write(mc_dir.join("logs/latest.log"), b"junk").unwrap();
        std::fs::create_dir_all(mc_dir.join("crash-reports")).unwrap();
        std::fs::write(mc_dir.join("crash-reports/crash.txt"), b"junk").unwrap();

        let out = dir.join("export.zip");
        export_instance(&instances_dir, "ExportPack", out.to_str().unwrap()).unwrap();

        let file = std::fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| {
                archive
                    .by_index(i)
                    .unwrap()
                    .name()
                    .ok()
                    .map(|name| name.into_owned())
                    .unwrap_or_default()
            })
            .collect();

        assert!(names.contains(&"instance.cfg".to_string()), "{:?}", names);
        assert!(names.contains(&"mmc-pack.json".to_string()), "{:?}", names);
        assert!(
            names.contains(&"minecraft/mods/testmod.jar".to_string()),
            "game dir must be minecraft/ (no dot): {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.starts_with(".minecraft")),
            "no dotted dir: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("instance.json")),
            "internal metadata excluded: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("logs")),
            "logs excluded: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("crash-reports")),
            "crash reports excluded: {:?}",
            names
        );

        let mut mmc = String::new();
        archive
            .by_name("mmc-pack.json")
            .unwrap()
            .read_to_string(&mut mmc)
            .unwrap();
        let pack: serde_json::Value = serde_json::from_str(&mmc).unwrap();
        assert_eq!(pack["formatVersion"], 1);
        let comps = pack["components"].as_array().unwrap();
        assert_eq!(comps[0]["uid"], "net.minecraft");
        assert_eq!(comps[0]["important"], true);
        assert_eq!(comps[1]["uid"], "net.fabricmc.fabric-loader");
        assert_eq!(comps[1]["version"], "0.15.11");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_refuses_an_instance_that_is_not_ready() {
        let dir = temp_dir("export_pending");
        let instances_dir = dir.join("instances");
        let instance = test_instance("PendingExport", "1.20.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        crate::instance_readiness::write_pending(
            &instances_dir.join("PendingExport"),
            &crate::instance_readiness::PendingSetup {
                stage: crate::instance_readiness::SetupStage::Preparing,
                operation: "import".to_string(),
                started_at: "2026-11-14T00:00:00Z".to_string(),
                token: "export-owner".to_string(),
            },
        )
        .unwrap();

        let out = dir.join("export.zip");
        let err = export_instance(&instances_dir, "PendingExport", out.to_str().unwrap())
            .expect_err("a partially prepared instance must not be exported as ready");
        assert!(err.to_string().contains("not fully set up"), "got: {err}");
        assert!(!out.exists(), "no archive may be produced");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_export_round_trip_preserves_instance() {
        let dir = temp_dir("round_trip");
        let src_dir = dir.join("src_instances");
        let instance = test_instance("RoundTrip", "1.20.1", LoaderType::Forge, Some("47.2.0"));
        save_instance(&src_dir, &instance, None).unwrap();
        let mc_dir = instance.minecraft_dir(&src_dir);
        std::fs::create_dir_all(mc_dir.join("mods")).unwrap();
        std::fs::write(mc_dir.join("mods/sodium.jar"), b"PK\x03\x04jar").unwrap();

        let out = dir.join("round.zip");
        export_instance(&src_dir, "RoundTrip", out.to_str().unwrap()).unwrap();

        let dst_dir = dir.join("dst_instances");
        let imported = import_prism_pack(
            &dst_dir,
            out.to_str().unwrap(),
            &crate::launch_state::LaunchRegistry::new(),
        )
        .unwrap();
        assert_eq!(imported.name, "RoundTrip");
        assert_eq!(imported.mc_version, "1.20.1");
        assert_eq!(imported.loader, LoaderType::Forge);
        assert_eq!(imported.loader_version.as_deref(), Some("47.2.0"));
        assert!(dst_dir
            .join("RoundTrip/.minecraft/mods/sodium.jar")
            .exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_import_rejects_quilt_packs() {
        let dir = temp_dir("quilt_reject");
        let zip_path = dir.join("quilt.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip_w.start_file("instance.cfg", opts).unwrap();
        zip_w.write_all(b"[General]\nname=Quilt Pack\n").unwrap();
        zip_w.start_file("mmc-pack.json", opts).unwrap();
        zip_w.write_all(
            br#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"org.quiltmc.quilt-loader","version":"0.20.2"}],"formatVersion":1}"#,
        )
        .unwrap();
        zip_w.finish().unwrap();

        let instances_dir = dir.join("instances");
        let err = import_prism_pack(
            &instances_dir,
            zip_path.to_str().unwrap(),
            &crate::launch_state::LaunchRegistry::new(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("Quilt"),
            "error must name Quilt: {}",
            err
        );
        assert!(
            !instances_dir.join("Quilt Pack").exists(),
            "no instance written on rejection"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prism_import_rejects_traversal_entries() {
        let dir = temp_dir("prism_traversal");
        let zip_path = dir.join("evil.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip_w.start_file("instance.cfg", opts).unwrap();
        zip_w.write_all(b"[General]\nname=Evil Pack\n").unwrap();
        zip_w.start_file("mmc-pack.json", opts).unwrap();
        zip_w
            .write_all(br#"{"components":[{"uid":"net.minecraft","version":"1.20.1"}]}"#)
            .unwrap();
        zip_w
            .start_file("minecraft/../../escape.txt", opts)
            .unwrap();
        zip_w.write_all(b"escaped").unwrap();
        zip_w.finish().unwrap();

        let instances_dir = dir.join("instances");
        let err = import_prism_pack(
            &instances_dir,
            zip_path.to_str().unwrap(),
            &crate::launch_state::LaunchRegistry::new(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("Unsafe path"),
            "error must name the unsafe path: {}",
            err
        );
        assert!(
            !instances_dir.join("Evil Pack").exists(),
            "no instance directory may survive a rejected import"
        );
        assert!(
            !dir.join("escape.txt").exists(),
            "traversal entry must not be written outside the instance"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_loader_names_are_not_shown_as_download_providers() {
        assert_eq!(normalize_provider("Fabric"), "Local");
        assert_eq!(normalize_provider("forge"), "Local");
        assert_eq!(normalize_provider("NeoForge"), "Local");
        assert_eq!(normalize_provider("modrinth"), "Modrinth");
        assert_eq!(normalize_provider("curseforge"), "CurseForge");
        assert_eq!(normalize_provider("atlauncher"), "ATLauncher");
    }

    #[test]
    fn sidecar_meta_uses_index_folder_with_legacy_fallback() {
        let dir = temp_dir("sidecar");
        let mods = dir.join("mods");
        std::fs::create_dir_all(&mods).unwrap();

        // New layout: hidden .index/ folder next to the content dir
        std::fs::create_dir_all(mods.join(".index")).unwrap();
        std::fs::write(
            mods.join(".index").join("foo.voidlauncher.json"),
            r#"{"provider":"modrinth","project_id":"abc"}"#,
        )
        .unwrap();
        let meta = read_sidecar_meta(&mods, "foo.jar").unwrap();
        assert_eq!(meta["project_id"], "abc");
        assert_eq!(
            sidecar_meta_path(&mods, "foo.jar"),
            mods.join(".index/foo.voidlauncher.json")
        );

        // .disabled file resolves to the same stem
        let meta = read_sidecar_meta(&mods, "foo.jar.disabled").unwrap();
        assert_eq!(meta["project_id"], "abc");

        // Legacy layout (file next to the content) still works as fallback
        std::fs::write(
            mods.join("bar.voidlauncher.json"),
            r#"{"provider":"curseforge"}"#,
        )
        .unwrap();
        let meta = read_sidecar_meta(&mods, "bar.zip").unwrap();
        assert_eq!(meta["provider"], "curseforge");

        // Removal cleans both layouts
        std::fs::write(mods.join(".index").join("baz.voidlauncher.json"), "{}").unwrap();
        std::fs::write(mods.join("baz.voidlauncher.json"), "{}").unwrap();
        remove_sidecar_meta(&mods, "baz.jar");
        assert!(!mods.join(".index").join("baz.voidlauncher.json").exists());
        assert!(!mods.join("baz.voidlauncher.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_packs_hides_prism_metadata_and_index_folder() {
        let dir = temp_dir("packs");
        let instances_dir = dir.join("instances");
        let now = chrono::Utc::now().to_rfc3339();
        let instance = Instance {
            name: "P".to_string(),
            mc_version: "1.20.1".to_string(),
            loader: LoaderType::Vanilla,
            loader_version: None,
            loader_profile: None,
            memory_mb: None,
            jvm_args: None,
            gc_preset: None,
            java_path: None,
            resolution: None,
            icon: None,
            banner: None,
            created_at: now.clone(),
            last_played: None,
            play_time_seconds: 0,
            notes: String::new(),
        };
        save_instance(&instances_dir, &instance, None).unwrap();

        let shaders = instances_dir
            .join("P")
            .join(".minecraft")
            .join("shaderpacks");
        std::fs::create_dir_all(&shaders).unwrap();
        std::fs::write(shaders.join("BSL_v10.zip"), b"PK\x03\x04x").unwrap();
        std::fs::write(shaders.join("bsl-shaders.pw.toml"), b"meta").unwrap();
        std::fs::create_dir_all(shaders.join(".index")).unwrap();
        std::fs::write(shaders.join(".index").join("bsl.voidlauncher.json"), b"{}").unwrap();

        let packs = list_packs(&instances_dir, "P", "shaderpacks").unwrap();
        assert_eq!(packs.len(), 1, "Prism .pw.toml and .index must be hidden");
        assert_eq!(packs[0].filename, "BSL_v10.zip");
        assert_eq!(
            packs[0].version, "",
            "a version-looking file name must not become the pack version"
        );

        // A disabled pack keeps the metadata of its enabled counterpart
        // (Prism stores the .pw.toml under the un-suffixed file name).
        let packs_dir = instances_dir
            .join("P")
            .join(".minecraft")
            .join("resourcepacks");
        std::fs::create_dir_all(packs_dir.join(".index")).unwrap();
        std::fs::write(
            packs_dir.join("Faithful 32x - 26.3.zip.disabled"),
            b"PK\x03\x04faithful",
        )
        .unwrap();
        std::fs::write(
            packs_dir.join(".index").join("faithful.pw.toml"),
            "filename = 'Faithful 32x - 26.3.zip'\nname = 'Faithful 32x'\nx-prismlauncher-version-number = '26.3'\n[update.modrinth]\nmod-id = 'faithful'\n",
        )
        .unwrap();

        let packs = list_packs(&instances_dir, "P", "resourcepacks").unwrap();
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].filename, "Faithful 32x - 26.3.zip");
        assert!(!packs[0].enabled);
        assert_eq!(packs[0].name, "Faithful 32x");
        assert_eq!(packs[0].version, "26.3");
        assert_eq!(packs[0].provider, "Modrinth");
        assert_eq!(packs[0].project_id, "faithful");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_version_from_filename_handles_common_shapes() {
        assert_eq!(
            extract_version_from_filename("SE Vanilla Consistency 1.20.1.zip"),
            "1.20.1"
        );
        assert_eq!(
            extract_version_from_filename("Aether Regenerated v1.3.1.zip"),
            "1.3.1"
        );
        assert_eq!(
            extract_version_from_filename("recipeessentials-1.20.1-4.7.jar"),
            "4.7"
        );
        assert_eq!(
            extract_version_from_filename("Ping-Wheel-1.12.1-forge-1.20.1.jar"),
            "1.20.1"
        );
        assert_eq!(extract_version_from_filename("mod_v5.8.1.jar"), "5.8.1");
        assert_eq!(
            extract_version_from_filename("Hearths v1.0.5.mod.jar"),
            "1.0.5"
        );
        assert_eq!(
            extract_version_from_filename("twilightforest-1.20.1-4.3.2508-universal.jar"),
            "4.3.2508"
        );
        assert_eq!(
            extract_version_from_filename("aether-1.20.1-1.5.2-neoforge.jar"),
            "1.5.2"
        );
        assert_eq!(
            extract_version_from_filename("Patchouli-1.20.1-85-FORGE.jar"),
            "85"
        );
        assert_eq!(
            extract_version_from_filename("TerraBlender-forge-1.20.1-3.0.1.10.jar"),
            "3.0.1.10"
        );
        assert_eq!(
            extract_version_from_filename("kotlinforforge-4.12.0-all.jar"),
            "4.12.0"
        );
        assert_eq!(
            extract_version_from_filename("ConnectorExtras-1.11.2+1.20.1.jar"),
            "1.11.2+1.20.1"
        );
        assert_eq!(
            extract_version_from_filename("Connector-1.0.0-beta.49+1.20.1.jar"),
            "1.0.0-beta.49+1.20.1"
        );
        assert_eq!(
            extract_version_from_filename("curios-forge-5.14.1+1.20.1.jar"),
            "5.14.1+1.20.1"
        );
        assert_eq!(
            extract_version_from_filename("CataclysmCompat1.0.zip"),
            "1.0"
        );
        assert_eq!(
            extract_version_from_filename("BoP x FD Bark Cutting Compat.zip"),
            ""
        );
        assert_eq!(extract_version_from_filename("Better_Modded_GUI.zip"), "");
        assert_eq!(extract_version_from_filename("NoBushyLeaves.zip"), "");
        assert_eq!(extract_version_from_filename("fresh_waystones.zip"), "");
        assert_eq!(
            extract_version_from_filename("BetterBetterX-v1.1-noStone.zip"),
            ""
        );
        assert_eq!(
            extract_version_from_filename("Geophilic v3.6.mod.jar"),
            "3.6"
        );
        assert_eq!(
            extract_version_from_filename("LessStructures-SpacingTweaks-1.20.1-2.1.56.zip"),
            "2.1.56"
        );
        assert_eq!(
            extract_version_from_filename("Modded Omelet [120] [162].zip"),
            ""
        );
    }

    // --- Transactional content update (park → download → swap / rollback) ---

    #[test]
    fn stash_parks_old_jar_and_restore_puts_it_back() {
        let dir = temp_dir("stash_restore");
        std::fs::write(dir.join("sodium-0.5.8.jar"), b"old-bytes").unwrap();

        let stashed = stash_old_content_files(&dir, "sodium-0.5.8.jar").unwrap();
        assert_eq!(stashed.len(), 1);
        assert!(
            !dir.join("sodium-0.5.8.jar").exists(),
            "old file must be parked, not live"
        );
        assert!(dir.join("sodium-0.5.8.jar.voidlauncher-old").exists());

        // Simulated failed download: no new file, rollback the park.
        restore_stashed_files(&stashed);
        assert!(
            dir.join("sodium-0.5.8.jar").exists(),
            "old file must survive a failed download"
        );
        assert_eq!(
            std::fs::read(dir.join("sodium-0.5.8.jar")).unwrap(),
            b"old-bytes"
        );
        assert!(!dir.join("sodium-0.5.8.jar.voidlauncher-old").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacing_a_jar_never_deletes_a_same_stem_zip_sibling() {
        let dir = temp_dir("stash_sibling_extension");
        std::fs::write(dir.join("Foo.jar"), b"old-jar").unwrap();
        std::fs::write(dir.join("Foo.zip"), b"unrelated-zip").unwrap();

        let stashed = stash_old_content_files(&dir, "Foo.jar").unwrap();
        assert_eq!(stashed.len(), 1, "only the replaced jar is parked");
        assert!(dir.join("Foo.zip").exists());

        std::fs::write(dir.join("Foo.jar"), b"new-jar").unwrap();
        discard_stashed_files(&stashed);

        assert_eq!(std::fs::read(dir.join("Foo.jar")).unwrap(), b"new-jar");
        assert_eq!(
            std::fs::read(dir.join("Foo.zip")).unwrap(),
            b"unrelated-zip",
            "a different content item with the same stem must survive"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stash_parks_disabled_and_enabled_twins() {
        let dir = temp_dir("stash_twins");
        std::fs::write(dir.join("sodium.jar.disabled"), b"disabled").unwrap();
        std::fs::write(dir.join("sodium.jar"), b"enabled").unwrap();

        let stashed = stash_old_content_files(&dir, "sodium.jar.disabled").unwrap();
        assert_eq!(
            stashed.len(),
            2,
            "both the disabled file and its enabled twin must be parked"
        );
        assert!(!dir.join("sodium.jar.disabled").exists());
        assert!(!dir.join("sodium.jar").exists());
        assert!(dir.join("sodium.jar.disabled.voidlauncher-old").exists());
        assert!(dir.join("sodium.jar.voidlauncher-old").exists());

        discard_stashed_files(&stashed);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "discard removes both parks"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_name_update_replaces_old_file() {
        let dir = temp_dir("stash_same_name");
        std::fs::write(dir.join("sodium.jar"), b"old-bytes").unwrap();

        let stashed = stash_old_content_files(&dir, "sodium.jar").unwrap();
        assert_eq!(stashed.len(), 1);
        // The name is free now: the "download" writes the replacement.
        std::fs::write(dir.join("sodium.jar"), b"new-bytes").unwrap();
        discard_stashed_files(&stashed);

        assert_eq!(std::fs::read(dir.join("sodium.jar")).unwrap(), b"new-bytes");
        assert!(
            !dir.join("sodium.jar.voidlauncher-old").exists(),
            "no leftovers after success"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_download_with_bad_replacement_rolls_back() {
        let dir = temp_dir("stash_bad_replacement");
        std::fs::write(dir.join("sodium-0.5.8.jar"), b"old-bytes").unwrap();

        let stashed = stash_old_content_files(&dir, "sodium-0.5.8.jar").unwrap();
        // Simulate a download that renamed into place but failed verification
        // (zip-magic failure happens after the rename).
        std::fs::write(dir.join("sodium-0.6.0.jar"), b"<html>error page</html>").unwrap();
        let dest = dir.join("sodium-0.6.0.jar");
        let _ = std::fs::remove_file(&dest);
        restore_stashed_files(&stashed);

        assert!(dir.join("sodium-0.5.8.jar").exists(), "old file restored");
        assert!(!dest.exists(), "bad replacement dropped");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stash_sanitizes_traversal_and_rejects_dotdot() {
        let dir = temp_dir("stash_traversal");
        // A decoy outside the content dir that must never be touched.
        let outside = dir.parent().unwrap().join(format!(
            "voidlauncher_stash_outside_{}_evil.jar",
            std::process::id()
        ));
        std::fs::write(&outside, b"outside").unwrap();

        // Path components are stripped: only files inside `dir` are candidates.
        let stashed = stash_old_content_files(&dir, "../evil.jar").unwrap();
        assert!(
            stashed.is_empty(),
            "sanitized name must not resolve outside the dir"
        );
        assert!(
            outside.exists(),
            "decoy outside the content dir must be untouched"
        );

        assert!(stash_old_content_files(&dir, "..").is_err());
        assert!(stash_old_content_files(&dir, ".").is_err());

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recover_restores_park_when_original_missing_and_drops_stale_park() {
        let dir = temp_dir("stash_recover");
        // Interrupted update: only the park survived → restore it.
        std::fs::write(dir.join("sodium.jar.voidlauncher-old"), b"old-bytes").unwrap();
        recover_stashed_content_files(&dir);
        assert!(
            dir.join("sodium.jar").exists(),
            "park must be restored when original is missing"
        );
        assert!(!dir.join("sodium.jar.voidlauncher-old").exists());

        // Completed update with a leftover park → drop the stale park.
        std::fs::write(dir.join("sodium.jar"), b"new-bytes").unwrap();
        std::fs::write(dir.join("sodium.jar.voidlauncher-old"), b"old-bytes").unwrap();
        recover_stashed_content_files(&dir);
        assert_eq!(std::fs::read(dir.join("sodium.jar")).unwrap(), b"new-bytes");
        assert!(!dir.join("sodium.jar.voidlauncher-old").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_old_sidecar_only_on_stem_change() {
        let dir = temp_dir("stash_sidecar");
        std::fs::create_dir_all(dir.join(".index")).unwrap();
        std::fs::write(dir.join(".index").join("sodium.voidlauncher.json"), b"{}").unwrap();
        std::fs::write(
            dir.join(".index").join("sodium-0.5.8.voidlauncher.json"),
            b"{}",
        )
        .unwrap();
        std::fs::write(
            dir.join(".index").join("sodium-0.6.0.voidlauncher.json"),
            b"{}",
        )
        .unwrap();

        // Same stem (re-download under the same file name): keep the sidecar.
        cleanup_old_sidecar_after_update(&dir, "sodium.jar", "sodium.jar");
        assert!(
            dir.join(".index").join("sodium.voidlauncher.json").exists(),
            "same-stem sidecar must survive"
        );

        // Stem changed: the old sidecar would describe a file that is gone.
        cleanup_old_sidecar_after_update(&dir, "sodium-0.5.8.jar", "sodium-0.6.0.jar");
        assert!(
            !dir.join(".index")
                .join("sodium-0.5.8.voidlauncher.json")
                .exists(),
            "stale sidecar must be removed when the stem changes"
        );
        assert!(
            dir.join(".index")
                .join("sodium-0.6.0.voidlauncher.json")
                .exists(),
            "new sidecar must survive"
        );
        assert!(
            dir.join(".index").join("sodium.voidlauncher.json").exists(),
            "unrelated sidecar must survive"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_world(instances_dir: &Path, instance: &Instance, world: &str, marker: &str) {
        let world_dir = instance
            .minecraft_dir(instances_dir)
            .join("saves")
            .join(world);
        std::fs::create_dir_all(world_dir.join("region")).unwrap();
        std::fs::write(world_dir.join("level.dat"), format!("level-{}", marker)).unwrap();
        std::fs::write(
            world_dir.join("region").join("r.0.0.mca"),
            format!("region-{}", marker),
        )
        .unwrap();
    }

    #[test]
    fn nbt_rejects_oversized_arrays_and_lists_without_allocating() {
        use std::io::Cursor;

        // Byte array claiming 4 GiB (the tag itself is the caller's argument,
        // so the payload starts with the length).
        let mut c = Cursor::new(u32::MAX.to_be_bytes().to_vec());
        let err = nbt_skip(&mut c, 7, 0).unwrap_err().to_string();
        assert!(err.contains("too large"), "got: {}", err);

        // Int array claiming ~1.6 GiB (400M × 4 bytes).
        let mut c = Cursor::new(400_000_000u32.to_be_bytes().to_vec());
        assert!(nbt_skip(&mut c, 11, 0).is_err());

        // A list of TAG_End with a non-zero length would loop millions of
        // times doing no I/O.
        let mut bytes = vec![0u8]; // element type TAG_End
        bytes.extend_from_slice(&1u32.to_be_bytes());
        let mut c = Cursor::new(bytes);
        assert!(nbt_skip(&mut c, 9, 0).is_err());

        // A list claiming 4 billion entries is rejected before looping.
        let mut bytes = vec![1u8]; // element type TAG_Byte
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        let mut c = Cursor::new(bytes);
        assert!(nbt_skip(&mut c, 9, 0).is_err());
    }

    #[test]
    fn nbt_rejects_deep_nesting() {
        use std::io::Cursor;

        let levels = NBT_MAX_DEPTH as usize + 10;
        let mut bytes = Vec::new();
        for _ in 0..levels {
            bytes.push(10u8); // TAG_Compound
            bytes.extend_from_slice(&0u16.to_be_bytes()); // empty name
        }
        bytes.push(0); // close the deepest compound
        bytes.extend(std::iter::repeat_n(0u8, levels)); // close each parent
        let mut c = Cursor::new(bytes);
        let err = nbt_skip(&mut c, 10, 0).unwrap_err().to_string();
        assert!(err.contains("depth"), "got: {}", err);
    }

    #[test]
    fn nbt_parses_flat_compound_fields() {
        use std::io::Cursor;

        let mut bytes = vec![3u8]; // TAG_Int
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(b"GameType");
        bytes.extend_from_slice(&2i32.to_be_bytes());
        bytes.push(8); // TAG_String
        bytes.extend_from_slice(&9u16.to_be_bytes());
        bytes.extend_from_slice(b"LevelName");
        bytes.extend_from_slice(&4u16.to_be_bytes());
        bytes.extend_from_slice(b"Test");
        bytes.push(0);

        let mut c = Cursor::new(bytes);
        let mut data = LevelDatData::default();
        nbt_parse_compound_fields(&mut c, &mut data, 0).unwrap();
        assert_eq!(data.game_type, Some(2));
        assert_eq!(data.level_name.as_deref(), Some("Test"));
    }

    #[test]
    fn write_file_atomic_replaces_content_and_leaves_no_temp() {
        let dir =
            std::env::temp_dir().join(format!("vl_atomic_write_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("instance.json");
        write_file_atomic(&path, b"first").unwrap();
        write_file_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        assert!(!dir.join(".instance.json.tmp").exists());
        assert!(!dir.join(".instance.json.bak").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn backup_world_creates_archive_and_restore_replace_overwrites() {
        let dir = temp_dir("world_backup");
        let instances_dir = dir.join("instances");
        let instance = test_instance("WorldPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_world(&instances_dir, &instance, "MyWorld", "v1");

        let info = backup_world(&instances_dir, "WorldPack", "MyWorld").unwrap();
        assert!(info.filename.starts_with("MyWorld__") && info.filename.ends_with(".zip"));
        assert!(info.size_bytes > 0);

        let list = list_world_backups(&instances_dir, "WorldPack", "MyWorld").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].filename, info.filename);

        // Damage the live world, then restore by replacing it.
        let world_dir = instance
            .minecraft_dir(&instances_dir)
            .join("saves")
            .join("MyWorld");
        std::fs::write(world_dir.join("level.dat"), "level-corrupted").unwrap();
        restore_world(
            &instances_dir,
            "WorldPack",
            "MyWorld",
            &info.filename,
            "replace",
            None,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(world_dir.join("level.dat")).unwrap(),
            "level-v1"
        );
        assert_eq!(
            std::fs::read_to_string(world_dir.join("region").join("r.0.0.mca")).unwrap(),
            "region-v1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_world_as_new_creates_distinct_world() {
        let dir = temp_dir("world_restore_new");
        let instances_dir = dir.join("instances");
        let instance = test_instance("NewWorldPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_world(&instances_dir, &instance, "Src", "orig");

        let info = backup_world(&instances_dir, "NewWorldPack", "Src").unwrap();
        restore_world(
            &instances_dir,
            "NewWorldPack",
            "Src",
            &info.filename,
            "as_new",
            Some("Restored Copy"),
        )
        .unwrap();

        let saves = instance.minecraft_dir(&instances_dir).join("saves");
        assert_eq!(
            std::fs::read_to_string(saves.join("Restored Copy").join("level.dat")).unwrap(),
            "level-orig"
        );
        // The source world is untouched.
        assert_eq!(
            std::fs::read_to_string(saves.join("Src").join("level.dat")).unwrap(),
            "level-orig"
        );

        // as_new onto an existing name must fail (no overwrite outside "replace").
        assert!(restore_world(
            &instances_dir,
            "NewWorldPack",
            "Src",
            &info.filename,
            "as_new",
            Some("Src")
        )
        .is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_world_corrupt_backup_keeps_original() {
        let dir = temp_dir("world_restore_corrupt");
        let instances_dir = dir.join("instances");
        let instance = test_instance("CorruptPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_world(&instances_dir, &instance, "Keep", "original");

        // Garbage file where a backup would be: extraction must fail BEFORE
        // touching the live world (regression: "replace" used to delete the
        // world first, so a corrupt backup destroyed it).
        let backups = world_backups_dir(&instances_dir, "CorruptPack", "Keep");
        std::fs::create_dir_all(&backups).unwrap();
        std::fs::write(backups.join("Keep__broken.zip"), b"not a zip").unwrap();

        let err = restore_world(
            &instances_dir,
            "CorruptPack",
            "Keep",
            "Keep__broken.zip",
            "replace",
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("Invalid backup archive"),
            "got: {}",
            err
        );

        let world_dir = instance
            .minecraft_dir(&instances_dir)
            .join("saves")
            .join("Keep");
        assert_eq!(
            std::fs::read_to_string(world_dir.join("level.dat")).unwrap(),
            "level-original"
        );
        assert_eq!(
            std::fs::read_to_string(world_dir.join("region").join("r.0.0.mca")).unwrap(),
            "region-original"
        );
        // No leftover restore temp/parked directories.
        let backups_root = instance.dir(&instances_dir).join(".backups");
        assert!(!backups_root.join(".restore-tmp").exists());
        assert!(!backups_root.join(".restore-old").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_archive_traversal_entry_is_rejected() {
        let dir = temp_dir("backup_zip_traversal");
        let zip_path = dir.join("evil.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_w = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zip_w.start_file("../escape.txt", opts).unwrap();
        zip_w.write_all(b"escaped").unwrap();
        zip_w.finish().unwrap();

        let dest = dir.join("restore");
        let err = extract_zip_to_dir(&zip_path, &dest).unwrap_err();
        assert!(
            err.to_string().contains("Unsafe path"),
            "error must name the unsafe entry: {}",
            err
        );
        assert!(
            !dir.join("escape.txt").exists(),
            "traversal entry must not be written outside the destination"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn world_backup_filename_traversal_rejected() {
        let dir = temp_dir("world_traversal");
        let instances_dir = dir.join("instances");
        let instance = test_instance("TravPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_world(&instances_dir, &instance, "W", "x");
        let decoy = instance.dir(&instances_dir).join("decoy.txt");
        std::fs::write(&decoy, b"keep").unwrap();

        for evil in [
            "../decoy.txt.zip",
            "..\\decoy.txt.zip",
            "C:/Windows/x.zip",
            "/abs.zip",
            "no-extension",
            ".hidden.zip",
            "sub/name.zip",
        ] {
            assert!(
                restore_world(&instances_dir, "TravPack", "W", evil, "replace", None).is_err(),
                "filename {:?} must be rejected",
                evil
            );
            assert!(
                delete_world_backup(&instances_dir, "TravPack", "W", evil).is_err(),
                "delete filename {:?} must be rejected",
                evil
            );
        }
        assert!(decoy.exists(), "decoy must survive");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_world_backups_sorts_newest_first_and_delete_removes() {
        let dir = temp_dir("world_backup_list");
        let instances_dir = dir.join("instances");
        let instance = test_instance("ListPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_world(&instances_dir, &instance, "LW", "a");

        // No backups yet → empty list, no error.
        assert!(list_world_backups(&instances_dir, "ListPack", "LW")
            .unwrap()
            .is_empty());

        let a = backup_world(&instances_dir, "ListPack", "LW").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let b = backup_world(&instances_dir, "ListPack", "LW").unwrap();
        assert_ne!(
            a.filename, b.filename,
            "same-second backups must not collide"
        );

        let list = list_world_backups(&instances_dir, "ListPack", "LW").unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].created_at >= list[1].created_at, "newest first");

        delete_world_backup(&instances_dir, "ListPack", "LW", &a.filename).unwrap();
        let after = list_world_backups(&instances_dir, "ListPack", "LW").unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].filename, b.filename);
        assert!(delete_world_backup(&instances_dir, "ListPack", "LW", &a.filename).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Instance backup / restore ────────────────────────────────

    fn make_instance_files(instances_dir: &Path, instance: &Instance) {
        let mc = instance.minecraft_dir(instances_dir);
        std::fs::create_dir_all(mc.join("mods")).unwrap();
        std::fs::write(mc.join("mods").join("sodium.jar"), b"jar-bytes").unwrap();
        std::fs::create_dir_all(mc.join("logs")).unwrap();
        std::fs::write(mc.join("logs").join("latest.log"), b"log").unwrap();
        std::fs::create_dir_all(mc.join("screenshots")).unwrap();
        std::fs::write(mc.join("screenshots").join("shot.png"), b"png").unwrap();
        std::fs::create_dir_all(mc.join("crash-reports")).unwrap();
        std::fs::write(mc.join("crash-reports").join("crash.txt"), b"crash").unwrap();
    }

    fn zip_entry_names(path: &std::path::Path) -> Vec<String> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        (0..archive.len())
            .map(|i| {
                archive
                    .by_index(i)
                    .unwrap()
                    .name()
                    .ok()
                    .map(|name| name.replace('\\', "/"))
                    .unwrap_or_default()
            })
            .collect()
    }

    #[test]
    fn backup_instance_excludes_junk_and_lists() {
        let dir = temp_dir("inst_backup");
        let instances_dir = dir.join("instances");
        let instance = test_instance("BackupMe", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        let info = backup_instance(&instances_dir, "BackupMe").unwrap();
        assert!(info.filename.starts_with("BackupMe__") && info.filename.ends_with(".zip"));
        assert!(info.size_bytes > 0);

        let zip_path = instances_dir
            .join("BackupMe")
            .join(".backups")
            .join("instances")
            .join(&info.filename);
        let names = zip_entry_names(&zip_path);
        assert!(names.iter().any(|n| n == "instance.json"), "{:?}", names);
        assert!(names.iter().any(|n| n == "instance.cfg"), "{:?}", names);
        assert!(
            names.iter().any(|n| n == ".minecraft/mods/sodium.jar"),
            "{:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("logs")),
            "logs leaked: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("screenshots")),
            "screenshots leaked: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains("crash-reports")),
            "crash reports leaked: {:?}",
            names
        );
        assert!(
            !names.iter().any(|n| n.contains(".backups")),
            "backups leaked into themselves: {:?}",
            names
        );

        assert_eq!(
            list_instance_backups(&instances_dir, "BackupMe")
                .unwrap()
                .len(),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn backup_and_export_do_not_follow_junctions_outside_the_instance() {
        use std::os::windows::process::CommandExt;
        let dir = temp_dir("backup_junction");
        let instances_dir = dir.join("instances");
        let instance = test_instance("JunctionPack", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"do-not-archive").unwrap();
        let link = instance
            .minecraft_dir(&instances_dir)
            .join("linked-outside");
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .raw_arg(format!(
                "mklink /J \"{}\" \"{}\"",
                link.display(),
                outside.display()
            ))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !status.map(|s| s.success()).unwrap_or(false) {
            eprintln!("skipping backup junction test: mklink /J failed");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        // Export: the junction must not be followed (and must not loop).
        let export_path = dir.join("export.zip");
        export_instance(
            &instances_dir,
            "JunctionPack",
            export_path.to_str().unwrap(),
        )
        .unwrap();
        let exported = zip_entry_names(&export_path);
        assert!(
            !exported.iter().any(|n| n.contains("linked-outside")),
            "export captured data outside the instance: {:?}",
            exported
        );

        // Backup: same guarantee.
        let info = backup_instance(&instances_dir, "JunctionPack").unwrap();
        let zip_path = instances_dir
            .join("JunctionPack")
            .join(".backups")
            .join("instances")
            .join(&info.filename);
        let backed_up = zip_entry_names(&zip_path);
        assert!(
            !backed_up.iter().any(|n| n.contains("linked-outside")),
            "backup captured data outside the instance: {:?}",
            backed_up
        );
        assert!(
            std::fs::read(outside.join("secret.txt")).unwrap() == b"do-not-archive",
            "the junction target must stay untouched"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_and_export_reserve_the_instance_against_rename_and_delete() {
        let dir = temp_dir("inst_read_reservation");
        let instances_dir = dir.join("instances");
        let instance = test_instance("ReadMe", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);
        let out = dir.join("export.zip");

        // A rename/delete reservation (acquired by the competing operation)
        // must make both read operations fail instead of archiving a moving
        // directory.
        {
            let _competitor =
                InstanceNameReservation::acquire(&instances_dir, &["ReadMe"]).unwrap();
            assert!(backup_instance(&instances_dir, "ReadMe").is_err());
            assert!(export_instance(&instances_dir, "ReadMe", out.to_str().unwrap()).is_err());
            assert!(!out.exists());
        }

        // Once the competing operation is gone, backup and export succeed and
        // their own guards are released for the next operation.
        backup_instance(&instances_dir, "ReadMe").expect("backup after release");
        export_instance(&instances_dir, "ReadMe", out.to_str().unwrap()).expect("export");
        assert!(out.is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_instance_replace_recovers_content_and_keeps_backup_history() {
        let dir = temp_dir("inst_restore_replace");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Victim", "1.21.1", LoaderType::Fabric, Some("0.16.0"));
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        let info = backup_instance(&instances_dir, "Victim").unwrap();

        // Corrupt: break the metadata AND delete the mod — restore must not
        // require the damaged metadata to run.
        let inst = instances_dir.join("Victim");
        std::fs::remove_file(inst.join("instance.json")).unwrap();
        std::fs::remove_file(inst.join(".minecraft").join("mods").join("sodium.jar")).unwrap();

        restore_instance(&instances_dir, "Victim", &info.filename, "replace", None).unwrap();

        assert!(inst.join("instance.json").exists(), "metadata restored");
        assert_eq!(
            std::fs::read(inst.join(".minecraft").join("mods").join("sodium.jar")).unwrap(),
            b"jar-bytes",
            "mod restored"
        );
        assert!(
            !inst.join(".minecraft").join("logs").exists(),
            "junk stays excluded"
        );
        // The swap must not lose the backup history that lives in `.backups`.
        let list = list_instance_backups(&instances_dir, "Victim").unwrap();
        assert_eq!(list.len(), 1, "backup history preserved across restore");
        assert_eq!(list[0].filename, info.filename);
        // And it still parses as a launchable instance.
        assert_eq!(
            get_instance(&instances_dir, "Victim").unwrap().name,
            "Victim"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_instance_as_new_creates_second_instance() {
        let dir = temp_dir("inst_restore_asnew");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Original", "1.21.1", LoaderType::NeoForge, Some("21.1.135"));
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        let info = backup_instance(&instances_dir, "Original").unwrap();
        restore_instance(
            &instances_dir,
            "Original",
            &info.filename,
            "as_new",
            Some("Restored Copy"),
        )
        .unwrap();

        let restored = instances_dir.join("Restored Copy");
        assert!(restored.join("instance.json").exists());
        assert_eq!(
            std::fs::read(restored.join(".minecraft").join("mods").join("sodium.jar")).unwrap(),
            b"jar-bytes"
        );
        assert_eq!(
            get_instance(&instances_dir, "Restored Copy").unwrap().name,
            "Restored Copy"
        );

        // Original untouched, its backups stay with it; the copy has none.
        assert!(instances_dir
            .join("Original")
            .join("instance.json")
            .exists());
        assert_eq!(
            list_instance_backups(&instances_dir, "Original")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_instance_backups(&instances_dir, "Restored Copy")
                .unwrap()
                .len(),
            0
        );

        // as_new onto an existing name must fail (no silent overwrite).
        assert!(restore_instance(
            &instances_dir,
            "Original",
            &info.filename,
            "as_new",
            Some("Restored Copy")
        )
        .is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn instance_backup_filename_traversal_rejected() {
        let dir = temp_dir("inst_backup_traversal");
        let instances_dir = dir.join("instances");
        let instance = test_instance("TravI", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);
        let decoy = instance.dir(&instances_dir).join("decoy.txt");
        std::fs::write(&decoy, b"keep").unwrap();

        for evil in [
            "../decoy.txt.zip",
            "..\\decoy.txt.zip",
            "C:/Windows/x.zip",
            "/abs.zip",
            "no-extension",
            ".hidden.zip",
            "sub/name.zip",
        ] {
            assert!(
                restore_instance(&instances_dir, "TravI", evil, "replace", None).is_err(),
                "restore filename {:?} must be rejected",
                evil
            );
            assert!(
                delete_instance_backup(&instances_dir, "TravI", evil).is_err(),
                "delete filename {:?} must be rejected",
                evil
            );
        }
        assert!(decoy.exists(), "decoy must survive");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_instance_rejects_archive_without_metadata_and_cleans_temp() {
        let dir = temp_dir("inst_restore_invalid");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Brokee", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        // A zip that is not an instance (no instance.json / instance.cfg).
        let backups = instances_dir
            .join("Brokee")
            .join(".backups")
            .join("instances");
        std::fs::create_dir_all(&backups).unwrap();
        let bad = backups.join("not-instance.zip");
        let file = std::fs::File::create(&bad).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zw.start_file("junk.txt", opts).unwrap();
        zw.write_all(b"hi").unwrap();
        zw.finish().unwrap();

        let mod_before = std::fs::read(
            instances_dir
                .join("Brokee")
                .join(".minecraft")
                .join("mods")
                .join("sodium.jar"),
        )
        .unwrap();
        assert!(restore_instance(
            &instances_dir,
            "Brokee",
            "not-instance.zip",
            "replace",
            None
        )
        .is_err());

        // The live instance must be untouched and no temp dirs left behind.
        assert!(instances_dir.join("Brokee").join("instance.json").exists());
        assert_eq!(
            std::fs::read(
                instances_dir
                    .join("Brokee")
                    .join(".minecraft")
                    .join("mods")
                    .join("sodium.jar")
            )
            .unwrap(),
            mod_before
        );
        for entry in std::fs::read_dir(&instances_dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with(".void-restore-tmp"),
                "temp dir leaked: {}",
                name
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_instance_metadata_failure_leaves_the_live_instance_untouched() {
        let dir = temp_dir("inst_restore_meta_fail");
        let instances_dir = dir.join("instances");
        let instance = test_instance("KeepMe", "1.21.1", LoaderType::Fabric, Some("0.16.0"));
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);

        let backups = instance_backups_dir(&instances_dir, "KeepMe");
        std::fs::create_dir_all(&backups).unwrap();
        let bad = backups.join("valid-but-unwritable.zip");
        let file = std::fs::File::create(&bad).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default();
        zw.start_file("instance.json", opts).unwrap();
        zw.write_all(
            serde_json::to_string(&instance)
                .expect("instance serializes")
                .as_bytes(),
        )
        .unwrap();
        // The atomic metadata write uses `.instance.json.tmp`; an archive
        // entry that creates a directory there makes the rewrite fail exactly
        // like a transient sharing violation would.
        zw.start_file(".instance.json.tmp/child", opts).unwrap();
        zw.write_all(b"x").unwrap();
        zw.finish().unwrap();

        let mod_path = instance
            .minecraft_dir(&instances_dir)
            .join("mods")
            .join("sodium.jar");
        let before = std::fs::read(&mod_path).unwrap();

        let err = restore_instance(
            &instances_dir,
            "KeepMe",
            "valid-but-unwritable.zip",
            "replace",
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("instance.json"),
            "error must name the failing metadata write: {err}"
        );

        // The failure happened in staging: the live instance is untouched and
        // no hidden staging/rollback directories were left behind.
        assert!(instances_dir.join("KeepMe").join("instance.json").is_file());
        assert_eq!(std::fs::read(&mod_path).unwrap(), before);
        for entry in std::fs::read_dir(&instances_dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(!name.starts_with(".void-"), "staging leaked: {name}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rename wrapper used to inject a Windows-style failure at one restore
    /// stage. `ErrorKind::Other` is non-retryable on purpose: these tests
    /// exercise the swap/rollback logic, while retry behaviour is covered by
    /// `rename_with_retry_*` below.
    struct ScriptedRename {
        fail_paths: Vec<PathBuf>,
        fail_prefixes: Vec<&'static str>,
    }

    impl ScriptedRename {
        fn new(fail_paths: Vec<PathBuf>, fail_prefixes: Vec<&'static str>) -> Self {
            Self {
                fail_paths,
                fail_prefixes,
            }
        }

        fn should_fail(&self, from: &Path) -> bool {
            self.fail_paths.iter().any(|p| p == from)
                || self.fail_prefixes.iter().any(|prefix| {
                    from.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(prefix))
                })
        }
    }

    impl RenameOp for ScriptedRename {
        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            if self.should_fail(from) {
                return Err(std::io::Error::other(
                    "Access is denied. (os error 5) [injected]",
                ));
            }
            std::fs::rename(from, to)
        }
    }

    struct FlakyRename {
        failures_left: std::cell::Cell<u32>,
        calls: std::cell::Cell<u32>,
    }

    impl RenameOp for FlakyRename {
        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.calls.set(self.calls.get() + 1);
            if self.failures_left.get() > 0 {
                self.failures_left.set(self.failures_left.get() - 1);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Access is denied. (os error 5) [injected]",
                ));
            }
            std::fs::rename(from, to)
        }
    }

    /// `replace` fixture where `live-only.txt` exists only in the live
    /// instance (not in the backup), so a failed swap that keeps the original
    /// can be distinguished from one that installed the archive.
    fn restore_replace_fixture(label: &str) -> (PathBuf, PathBuf, Instance, BackupInfo) {
        let dir = temp_dir(label);
        let instances_dir = dir.join("instances");
        let instance = test_instance("Locked", "1.21.1", LoaderType::Fabric, Some("0.16.0"));
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);
        let backup = backup_instance(&instances_dir, "Locked").unwrap();
        std::fs::write(
            instance.minecraft_dir(&instances_dir).join("live-only.txt"),
            b"live",
        )
        .unwrap();
        (dir, instances_dir, instance, backup)
    }

    fn assert_no_staging_dirs(instances_dir: &Path) {
        for entry in std::fs::read_dir(instances_dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(!name.starts_with(".void-"), "staging leaked: {name}");
        }
    }

    #[test]
    fn rename_with_retry_recovers_from_transient_access_denied() {
        let dir = temp_dir("restore_rename_retry");
        let from = dir.join("from");
        let to = dir.join("to");
        std::fs::create_dir_all(&from).unwrap();
        let op = FlakyRename {
            failures_left: std::cell::Cell::new(2),
            calls: std::cell::Cell::new(0),
        };

        rename_with_retry_policy(
            &op,
            &from,
            &to,
            "Moving the current instance aside",
            4,
            1,
            2,
        )
        .unwrap();

        assert_eq!(op.calls.get(), 3, "two failures then a success");
        assert!(to.is_dir() && !from.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_with_retry_reports_stage_path_and_attempts() {
        let dir = temp_dir("restore_rename_exhausted");
        let from = dir.join("from");
        std::fs::create_dir_all(&from).unwrap();
        let op = FlakyRename {
            failures_left: std::cell::Cell::new(u32::MAX),
            calls: std::cell::Cell::new(0),
        };

        let err = rename_with_retry_policy(
            &op,
            &from,
            &dir.join("to"),
            "Moving the current instance aside",
            3,
            1,
            1,
        )
        .unwrap_err();

        let text = err.to_string();
        assert!(text.contains("Moving the current instance aside"), "{text}");
        assert!(text.contains(&from.display().to_string()), "{text}");
        assert!(text.contains("after 3 attempts"), "{text}");
        assert_eq!(op.calls.get(), 3, "all attempts must be used");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_as_new_respects_a_reserved_target_name() {
        let (dir, instances_dir, _instance, backup) =
            restore_replace_fixture("restore_reserved_target");
        let guard = InstanceNameReservation::acquire(&instances_dir, &["TargetPack"]).unwrap();

        let err = restore_instance(
            &instances_dir,
            "Locked",
            &backup.filename,
            "as_new",
            Some("TargetPack"),
        )
        .expect_err("restore must not claim a reserved target");
        assert!(err.to_string().contains("Another operation"), "got: {err}");
        assert!(
            !instances_dir.join("TargetPack").exists(),
            "the reserved target must not be created"
        );
        assert!(
            instances_dir.join("Locked").join("instance.json").is_file(),
            "the source instance must stay intact"
        );
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_move_aside_failure_keeps_the_original_and_backup() {
        let (dir, instances_dir, instance, backup) =
            restore_replace_fixture("restore_locked_aside");
        let inst = instances_dir.join("Locked");
        let op = ScriptedRename::new(vec![inst.clone()], Vec::new());

        let err = restore_instance_with(
            &instances_dir,
            "Locked",
            &backup.filename,
            "replace",
            None,
            &op,
        )
        .unwrap_err();

        let text = err.to_string();
        assert!(text.contains("Moving the current instance aside"), "{text}");
        assert!(text.contains("Nothing was changed"), "{text}");
        assert!(
            inst.join("instance.json").is_file(),
            "original metadata kept"
        );
        assert_eq!(
            std::fs::read(instance.minecraft_dir(&instances_dir).join("live-only.txt")).unwrap(),
            b"live",
            "live instance content kept"
        );
        assert_eq!(
            list_instance_backups(&instances_dir, "Locked")
                .unwrap()
                .len(),
            1,
            "backup history kept"
        );
        assert!(instance_backups_dir(&instances_dir, "Locked")
            .join(&backup.filename)
            .is_file());
        assert_no_staging_dirs(&instances_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_install_failure_rolls_back_the_original() {
        let (dir, instances_dir, instance, backup) =
            restore_replace_fixture("restore_install_fail");
        let op = ScriptedRename::new(Vec::new(), vec![".void-restore-tmp-"]);

        let err = restore_instance_with(
            &instances_dir,
            "Locked",
            &backup.filename,
            "replace",
            None,
            &op,
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("Installing the restored instance"),
            "{err}"
        );
        // The rollback put the original directory back under its real name.
        assert!(instances_dir.join("Locked").join("instance.json").is_file());
        assert_eq!(
            std::fs::read(instance.minecraft_dir(&instances_dir).join("live-only.txt")).unwrap(),
            b"live",
            "original content rolled back, not the archive"
        );
        assert_eq!(
            list_instance_backups(&instances_dir, "Locked")
                .unwrap()
                .len(),
            1,
            "backup history survived the rollback"
        );
        assert_no_staging_dirs(&instances_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_rollback_failure_preserves_the_original_directory() {
        let (dir, instances_dir, _instance, backup) =
            restore_replace_fixture("restore_rollback_fail");
        let op = ScriptedRename::new(Vec::new(), vec![".void-restore-tmp-", ".void-old-"]);

        let err = restore_instance_with(
            &instances_dir,
            "Locked",
            &backup.filename,
            "replace",
            None,
            &op,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("original instance is preserved at"),
            "{err}"
        );
        assert!(
            !instances_dir.join("Locked").exists(),
            "the failed swap must not leave a half-installed instance"
        );

        let preserved: Vec<PathBuf> = std::fs::read_dir(&instances_dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(".void-old-"))
            })
            .collect();
        assert_eq!(preserved.len(), 1, "original directory must be preserved");
        let preserved = &preserved[0];
        assert!(preserved.join("instance.json").is_file());
        assert_eq!(
            std::fs::read(preserved.join(".minecraft/live-only.txt")).unwrap(),
            b"live"
        );
        assert!(
            preserved
                .join(".backups/instances")
                .join(&backup.filename)
                .is_file(),
            "backup archive stays with the preserved original"
        );
        for entry in std::fs::read_dir(&instances_dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with(".void-restore-tmp-"),
                "staging copy leaked: {name}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_as_new_install_failure_leaves_the_original_untouched() {
        let (dir, instances_dir, instance, backup) = restore_replace_fixture("restore_as_new_fail");
        let op = ScriptedRename::new(Vec::new(), vec![".void-restore-tmp-"]);

        let err = restore_instance_with(
            &instances_dir,
            "Locked",
            &backup.filename,
            "as_new",
            Some("Copy"),
            &op,
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("Installing the restored instance"),
            "{err}"
        );
        assert!(!instances_dir.join("Copy").exists());
        assert!(instances_dir.join("Locked").join("instance.json").is_file());
        assert_eq!(
            std::fs::read(instance.minecraft_dir(&instances_dir).join("live-only.txt")).unwrap(),
            b"live"
        );
        assert_eq!(
            list_instance_backups(&instances_dir, "Locked")
                .unwrap()
                .len(),
            1
        );
        assert_no_staging_dirs(&instances_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_dir_all_force_removes_read_only_content() {
        let dir = temp_dir("restore_cleanup_readonly");
        let root = dir.join("old");
        let nested = root.join(".minecraft").join("mods");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("readonly.jar");
        std::fs::write(&file, b"x").unwrap();
        let mut perms = std::fs::metadata(&file).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&file, perms).unwrap();

        remove_dir_all_force(&root).unwrap();
        assert!(
            !root.exists(),
            "read-only content must not block the cleanup"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Duplicate ────────────────────────────────────────────────

    #[test]
    fn duplicate_instance_copies_everything_except_backup_history() {
        let dir = temp_dir("dup_default");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Src", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);
        let backups = instances_dir.join("Src").join(".backups").join("instances");
        std::fs::create_dir_all(&backups).unwrap();
        std::fs::write(backups.join("Src__old.zip"), b"zip-bytes").unwrap();

        let copy = duplicate_instance(
            &instances_dir,
            "Src",
            "Dst",
            DuplicateExclusions::default(),
            &crate::launch_state::LaunchRegistry::new(),
        )
        .unwrap();

        assert_eq!(copy.name, "Dst");
        assert_eq!(copy.play_time_seconds, 0);
        assert!(copy.last_played.is_none());
        let dst = instances_dir.join("Dst");
        assert!(dst.join("instance.json").exists());
        assert!(dst
            .join(".minecraft")
            .join("mods")
            .join("sodium.jar")
            .exists());
        assert!(dst
            .join(".minecraft")
            .join("logs")
            .join("latest.log")
            .exists());
        assert!(
            !dst.join(".backups").exists(),
            "source backup history must stay with the original"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_never_publishes_an_incomplete_target() {
        let dir = temp_dir("dup_readiness");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Src", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);
        std::fs::write(
            instances_dir.join("Src").join(".void-instance-pending"),
            br#"{"stage":"importing","operation":"import","started_at":"2026-11-14T00:00:00Z","token":"x"}"#,
        )
        .unwrap();

        // A partial source must never be cloned into a launchable instance.
        let launches = crate::launch_state::LaunchRegistry::new();
        let err = duplicate_instance(
            &instances_dir,
            "Src",
            "Dst",
            DuplicateExclusions::default(),
            &launches,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not fully set up"), "got: {err}");
        assert!(!instances_dir.join("Dst").exists());

        // A complete source produces a target with no inherited marker and a
        // launch slot that is free again.
        std::fs::remove_file(instances_dir.join("Src").join(".void-instance-pending")).unwrap();
        let copy = duplicate_instance(
            &instances_dir,
            "Src",
            "Dst",
            DuplicateExclusions::default(),
            &launches,
        )
        .unwrap();
        assert_eq!(copy.name, "Dst");
        assert!(
            crate::instance_readiness::read_pending(&instances_dir.join("Dst")).is_none(),
            "duplicating must not copy the source's setup marker"
        );
        assert!(launches.begin_launch("Dst").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_instance_excludes_picked_content() {
        let dir = temp_dir("dup_exclude");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Src", "1.21.1", LoaderType::Fabric, Some("0.16.0"));
        save_instance(&instances_dir, &instance, None).unwrap();
        make_instance_files(&instances_dir, &instance);
        let mc = instance.minecraft_dir(&instances_dir);
        std::fs::create_dir_all(mc.join("saves").join("World")).unwrap();
        std::fs::write(mc.join("saves").join("World").join("level.dat"), b"lvl").unwrap();

        let exclude = DuplicateExclusions {
            saves: true,
            screenshots: true,
            logs: true,
        };
        duplicate_instance(
            &instances_dir,
            "Src",
            "Dst",
            exclude,
            &crate::launch_state::LaunchRegistry::new(),
        )
        .unwrap();

        let dst_mc = instances_dir.join("Dst").join(".minecraft");
        // Metadata + mods always travel.
        assert!(instances_dir.join("Dst").join("instance.json").exists());
        assert!(dst_mc.join("mods").join("sodium.jar").exists());
        assert!(!dst_mc.join("saves").exists(), "saves excluded");
        assert!(!dst_mc.join("screenshots").exists(), "screenshots excluded");
        assert!(!dst_mc.join("logs").exists(), "logs excluded");
        assert!(
            !dst_mc.join("crash-reports").exists(),
            "crash reports follow logs"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_staging_paths_are_unique_and_hidden() {
        let dir = temp_dir("restore_tokens");
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            let token = new_restore_token();
            assert!(seen.insert(token.clone()), "restore tokens must be unique");
            let paths = restore_staging_paths(&dir, &token);
            for path in [&paths.tmp, &paths.old] {
                let name = path.file_name().unwrap().to_str().unwrap();
                assert!(name.starts_with(".void-"), "not hidden: {name}");
                assert!(name.contains(&token), "token missing from {name}");
            }
            assert_ne!(paths.tmp, paths.old);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_restore_after_rename_keeps_the_target_name() {
        let dir = temp_dir("restore_rename");
        let instances_dir = dir.join("instances");
        let instance = test_instance("OldName", "1.20.1", LoaderType::Fabric, Some("0.15.11"));
        save_instance(&instances_dir, &instance, None).unwrap();
        let mc_dir = instance.minecraft_dir(&instances_dir);
        std::fs::create_dir_all(&mc_dir).unwrap();
        std::fs::write(mc_dir.join("from-backup.txt"), b"backup content").unwrap();

        let backup = backup_instance(&instances_dir, "OldName").unwrap();

        // Rename A (OldName) -> B (NewName): the directory and metadata move.
        let mut renamed = get_instance(&instances_dir, "OldName").unwrap();
        renamed.name = "NewName".to_string();
        save_instance(&instances_dir, &renamed, Some("OldName")).unwrap();
        assert!(instances_dir.join("NewName").exists());
        assert!(!instances_dir.join("OldName").exists());
        // Live state that must disappear when the older backup is restored.
        std::fs::write(
            instances_dir.join("NewName/.minecraft/live-only.txt"),
            b"live",
        )
        .unwrap();

        // The backup still says "OldName"; replace-restore into B must not
        // resurrect the old directory or leave metadata pointing at it.
        restore_instance(&instances_dir, "NewName", &backup.filename, "replace", None).unwrap();

        let restored = get_instance(&instances_dir, "NewName").unwrap();
        assert_eq!(restored.name, "NewName");
        assert_eq!(restored.mc_version, "1.20.1");
        assert_eq!(restored.loader, LoaderType::Fabric);
        assert!(instances_dir
            .join("NewName/.minecraft/from-backup.txt")
            .exists());
        assert!(
            !instances_dir
                .join("NewName/.minecraft/live-only.txt")
                .exists(),
            "replace-restore must swap in the archived content"
        );
        assert!(
            !instances_dir.join("OldName").exists(),
            "metadata must not point at the pre-rename folder"
        );

        let listed = list_instances(&instances_dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "NewName");

        // The backup archive travelled with the rename and survived restore.
        let backups = list_instance_backups(&instances_dir, "NewName").unwrap();
        assert!(backups.iter().any(|b| b.filename == backup.filename));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_replace_restores_leave_no_staging_directories() {
        let dir = temp_dir("restore_repeat");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Repeat", "1.20.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &instance, None).unwrap();
        std::fs::create_dir_all(instance.minecraft_dir(&instances_dir)).unwrap();
        std::fs::write(
            instance.minecraft_dir(&instances_dir).join("state.txt"),
            b"original",
        )
        .unwrap();
        let backup = backup_instance(&instances_dir, "Repeat").unwrap();

        for _ in 0..3 {
            restore_instance(&instances_dir, "Repeat", &backup.filename, "replace", None).unwrap();
        }

        let leftovers: Vec<String> = std::fs::read_dir(&instances_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".void-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "restore staging directories left behind: {leftovers:?}"
        );
        assert_eq!(
            get_instance(&instances_dir, "Repeat").unwrap().name,
            "Repeat"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_as_new_uses_the_new_name_and_cleans_up_on_its_own() {
        let dir = temp_dir("restore_as_new_name");
        let instances_dir = dir.join("instances");
        let instance = test_instance("Source", "1.21.1", LoaderType::Forge, Some("52.0.0"));
        save_instance(&instances_dir, &instance, None).unwrap();
        let backup = backup_instance(&instances_dir, "Source").unwrap();

        restore_instance(
            &instances_dir,
            "Source",
            &backup.filename,
            "as_new",
            Some("Copy"),
        )
        .unwrap();

        let copy = get_instance(&instances_dir, "Copy").unwrap();
        assert_eq!(copy.name, "Copy");
        assert_eq!(copy.mc_version, "1.21.1");
        assert!(instances_dir.join("Source").exists(), "original untouched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_options_resource_packs_roundtrip_and_preserve_settings() {
        let dir = temp_dir("options_test");
        let options_path = dir.join("options.txt");

        let initial_content = "fov:70.0\r\nsoundCategory_master:1.0\r\nresourcePacks:[\"vanilla\",\"mod_resources\"]\r\ngamma:1.0\r\n";
        std::fs::write(&options_path, initial_content).unwrap();

        // 1. Enable pack
        set_options_resource_pack_enabled(&options_path, "Faithful.zip", true).unwrap();

        let (packs, _) = read_options_resource_packs(&options_path);
        assert_eq!(packs, vec!["vanilla", "mod_resources", "file/Faithful.zip"]);

        // Check settings preserved and backup created
        let new_content = std::fs::read_to_string(&options_path).unwrap();
        assert!(new_content.contains("fov:70.0"));
        assert!(new_content.contains("soundCategory_master:1.0"));
        assert!(new_content.contains("gamma:1.0"));

        let bak_path = dir.join("options.txt.bak");
        assert!(bak_path.exists());
        let bak_content = std::fs::read_to_string(&bak_path).unwrap();
        assert_eq!(bak_content, initial_content);

        // 2. Disable pack
        set_options_resource_pack_enabled(&options_path, "Faithful.zip", false).unwrap();
        let (packs_after_disable, _) = read_options_resource_packs(&options_path);
        assert_eq!(packs_after_disable, vec!["vanilla", "mod_resources"]);

        // 3. Disable with .disabled suffix argument (should also match)
        set_options_resource_pack_enabled(&options_path, "Faithful.zip", true).unwrap();
        set_options_resource_pack_enabled(&options_path, "Faithful.zip.disabled", false).unwrap();
        let (packs_disabled_again, _) = read_options_resource_packs(&options_path);
        assert_eq!(packs_disabled_again, vec!["vanilla", "mod_resources"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn options_read_error_is_never_treated_as_an_empty_file() {
        let dir = temp_dir("options_unreadable");
        let options_path = dir.join("options.txt");
        // Invalid UTF-8 is a realistic read error on legacy/localized files.
        let original = vec![0xFF, 0xFE, 0x00, 0x41];
        std::fs::write(&options_path, &original).unwrap();

        assert!(set_options_resource_pack_enabled(&options_path, "A.zip", true).is_err());
        assert_eq!(std::fs::read(&options_path).unwrap(), original);
        assert!(set_options_resource_packs_order(&options_path, &["A.zip".into()]).is_err());
        assert_eq!(std::fs::read(&options_path).unwrap(), original);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_resource_packs_line_is_not_rewritten() {
        let dir = temp_dir("options_bad_packs");
        let options_path = dir.join("options.txt");
        let original = "fov:70.0\nresourcePacks:not-json\ngamma:1.0\n";
        std::fs::write(&options_path, original).unwrap();

        assert!(set_options_resource_pack_enabled(&options_path, "A.zip", true).is_err());
        assert_eq!(std::fs::read_to_string(&options_path).unwrap(), original);
        assert!(set_options_resource_packs_order(&options_path, &["A.zip".into()]).is_err());
        assert_eq!(std::fs::read_to_string(&options_path).unwrap(), original);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_set_options_resource_packs_order_preserves_system_packs() {
        let dir = temp_dir("options_order_test");
        let options_path = dir.join("options.txt");

        let initial_content =
            "resourcePacks:[\"vanilla\",\"mod_resources\",\"file/PackA.zip\",\"file/PackB.zip\"]\n";
        std::fs::write(&options_path, initial_content).unwrap();

        set_options_resource_packs_order(&options_path, &["PackB.zip".into(), "PackA.zip".into()])
            .unwrap();

        let (packs, _) = read_options_resource_packs(&options_path);
        assert_eq!(
            packs,
            vec![
                "vanilla",
                "mod_resources",
                "file/PackB.zip",
                "file/PackA.zip"
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_set_options_resource_packs_order_ignores_inactive_and_duplicate_names() {
        let dir = temp_dir("options_order_active_only");
        let options_path = dir.join("options.txt");

        std::fs::write(
            &options_path,
            "resourcePacks:[\"vanilla\",\"file/PackA.zip\",\"file/PackB.zip\"]\n",
        )
        .unwrap();

        set_options_resource_packs_order(
            &options_path,
            &[
                "Disabled.zip".into(),
                "PackB.zip".into(),
                "PackB.zip".into(),
                "PackA.zip".into(),
            ],
        )
        .unwrap();

        let (packs, _) = read_options_resource_packs(&options_path);
        assert_eq!(packs, vec!["vanilla", "file/PackB.zip", "file/PackA.zip"]);
        assert!(
            !is_pack_enabled_in_list("Disabled.zip", &packs),
            "a disabled pack must never be enabled by a reorder"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_set_options_resource_packs_order_keeps_unmentioned_active_packs() {
        let dir = temp_dir("options_order_keep_unmentioned");
        let options_path = dir.join("options.txt");

        std::fs::write(
            &options_path,
            "resourcePacks:[\"vanilla\",\"file/A.zip\",\"file/B.zip\",\"file/C.zip\"]\n",
        )
        .unwrap();

        set_options_resource_packs_order(&options_path, &["C.zip".into()]).unwrap();

        let (packs, _) = read_options_resource_packs(&options_path);
        assert_eq!(
            packs,
            vec!["vanilla", "file/C.zip", "file/A.zip", "file/B.zip"],
            "an active pack omitted from the request must stay active"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resource_pack_commands_reject_path_separators() {
        let dir = temp_dir("options_reject_paths");
        let options_path = dir.join("options.txt");
        std::fs::write(
            &options_path,
            "resourcePacks:[\"vanilla\",\"file/Valid.zip\"]\n",
        )
        .unwrap();

        assert!(set_options_resource_pack_enabled(&options_path, "../Evil.zip", true).is_err());
        assert!(set_options_resource_packs_order(&options_path, &["../Evil.zip".into()]).is_err());
        assert_eq!(
            std::fs::read_to_string(&options_path).unwrap(),
            "resourcePacks:[\"vanilla\",\"file/Valid.zip\"]\n",
            "rejected input must not modify options.txt"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_set_options_resource_packs_order_does_not_create_missing_options_file() {
        let dir = temp_dir("options_order_missing");
        let options_path = dir.join("options.txt");

        set_options_resource_packs_order(&options_path, &["A.zip".into()]).unwrap();

        assert!(
            !options_path.exists(),
            "reordering must not invent options.txt"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_packs_resourcepacks_sorted_by_options_priority() {
        let dir = temp_dir("list_packs_priority");
        let instances_dir = dir.join("instances");
        let instance_name = "PriorityTest";
        save_instance(&instances_dir, &vanilla_instance(instance_name), None).unwrap();

        let mc_dir = instances_dir.join(instance_name).join(".minecraft");
        let rp_dir = mc_dir.join("resourcepacks");
        std::fs::create_dir_all(&rp_dir).unwrap();
        for name in ["A.zip", "B.zip", "C.zip", "D.zip"] {
            std::fs::write(rp_dir.join(name), b"dummy").unwrap();
        }

        // options.txt is low -> high priority: B, C, A. D is not active.
        std::fs::write(
            mc_dir.join("options.txt"),
            "resourcePacks:[\"vanilla\",\"file/B.zip\",\"file/C.zip\",\"file/A.zip\"]\n",
        )
        .unwrap();

        let packs = list_packs(&instances_dir, instance_name, "resourcepacks").unwrap();
        let filenames: Vec<&str> = packs.iter().map(|p| p.filename.as_str()).collect();
        assert_eq!(
            filenames,
            vec!["A.zip", "C.zip", "B.zip", "D.zip"],
            "highest-priority active pack must be listed first"
        );
        assert!(packs[0].enabled && packs[1].enabled && packs[2].enabled);
        assert!(!packs[3].enabled);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_packs_fallback_name_drops_only_instance_mc_suffix() {
        let dir = temp_dir("list_packs_mc_name");
        let instances_dir = dir.join("instances");
        let instance_name = "NameTest";
        save_instance(&instances_dir, &vanilla_instance(instance_name), None).unwrap();

        let mc_dir = instances_dir.join(instance_name).join(".minecraft");
        let rp_dir = mc_dir.join("resourcepacks");
        std::fs::create_dir_all(&rp_dir).unwrap();
        std::fs::write(rp_dir.join("Faithful 32x - 1.20.1.zip"), b"PK\x03\x04dummy").unwrap();
        std::fs::write(rp_dir.join("Faithful 32x - 26.3.zip"), b"PK\x03\x04dummy").unwrap();

        let packs = list_packs(&instances_dir, instance_name, "resourcepacks").unwrap();
        let names: Vec<(&str, &str)> = packs
            .iter()
            .map(|p| (p.filename.as_str(), p.name.as_str()))
            .collect();
        assert!(names.contains(&("Faithful 32x - 1.20.1.zip", "Faithful 32x")));
        // A suffix for another MC version may be part of the real pack name.
        assert!(names.contains(&("Faithful 32x - 26.3.zip", "Faithful 32x - 26.3")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_is_pack_enabled_in_list_matching() {
        let list = vec![
            "vanilla".to_string(),
            "mod_resources".to_string(),
            "file/FreshAnimations.zip".to_string(),
            "BareBones.zip".to_string(),
        ];

        assert!(is_pack_enabled_in_list("FreshAnimations.zip", &list));
        assert!(is_pack_enabled_in_list(
            "FreshAnimations.zip.disabled",
            &list
        ));
        assert!(is_pack_enabled_in_list("BareBones.zip", &list));
        assert!(!is_pack_enabled_in_list("OtherPack.zip", &list));
    }

    #[test]
    fn clean_pack_stem_strips_extensions_and_color_codes() {
        assert_eq!(clean_pack_stem("Faithful 32x.zip"), "Faithful 32x");
        assert_eq!(clean_pack_stem("Faithful 32x.zip.disabled"), "Faithful 32x");
        assert_eq!(clean_pack_stem("Bare_Bones.jar"), "Bare_Bones");
        assert_eq!(clean_pack_stem("Bare_Bones.jar.disabled"), "Bare_Bones");
        assert_eq!(clean_pack_stem("§6Fancy Pack§r.zip"), "Fancy Pack");
        assert_eq!(clean_pack_stem("FolderPack"), "FolderPack");
    }

    #[test]
    fn pack_filename_fallback_removes_only_the_instance_mc_version() {
        assert_eq!(
            clean_pack_stem_for_mc("Faithful 32x - 26.3.zip", "26.3"),
            "Faithful 32x"
        );
        assert_eq!(
            clean_pack_stem_for_mc("Faithful 32x (1.20.1).zip", "1.20.1"),
            "Faithful 32x"
        );
        assert_eq!(
            clean_pack_stem_for_mc("Faithful 32x - 1.20.1.zip", "26.3"),
            "Faithful 32x - 1.20.1",
            "a suffix for another MC version may be part of the pack name"
        );
        assert_eq!(
            clean_pack_stem_for_mc("Faithful 32x.zip", "26.3"),
            "Faithful 32x"
        );
        assert_eq!(
            clean_pack_stem_for_mc("26.3.zip", "26.3"),
            "26.3",
            "never reduce a name to an empty string"
        );
    }

    #[test]
    fn parse_minecraft_text_handles_strings_objects_and_recursive_extra() {
        // String
        let v1 = serde_json::json!("Simple description");
        assert_eq!(
            parse_minecraft_text(&v1).as_deref(),
            Some("Simple description")
        );

        // Object with text and nested extra
        let v2 = serde_json::json!({
            "text": "Root",
            "extra": [
                {
                    "text": " Child",
                    "extra": [
                        { "text": " Leaf", "color": "green" }
                    ]
                }
            ]
        });
        assert_eq!(
            parse_minecraft_text(&v2).as_deref(),
            Some("Root Child Leaf")
        );

        // Array with color codes
        let v3 = serde_json::json!([
            { "text": "§eWarning: " },
            { "text": "§aAll good!" }
        ]);
        assert_eq!(
            parse_minecraft_text(&v3).as_deref(),
            Some("Warning: All good!")
        );
    }

    #[test]
    fn read_pack_meta_tolerates_corrupt_archives_and_missing_files() {
        let dir = temp_dir("pack_meta_corrupt");
        let broken_zip = dir.join("broken.zip");
        std::fs::write(&broken_zip, b"not a zip archive").unwrap();

        assert_eq!(read_pack_meta_from_zip(&broken_zip), (None, None));
        assert_eq!(read_pack_meta_from_dir(&dir), (None, None));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_pack_meta_from_json_separates_name_and_description() {
        // Standard vanilla pack.mcmeta (only description)
        let standard = serde_json::json!({
            "pack": {
                "pack_format": 15,
                "description": "§6A nice pack description§r\nBy Dev"
            }
        });
        let (name, desc) = read_pack_meta_from_json(&standard);
        assert_eq!(
            name, None,
            "Vanilla pack.mcmeta has no pack.name, name must be None"
        );
        assert_eq!(desc.as_deref(), Some("A nice pack description\nBy Dev"));

        // Extended pack.mcmeta with explicit pack.name
        let extended = serde_json::json!({
            "pack": {
                "name": "Custom Pack Name",
                "pack_format": 15,
                "description": "Some description"
            }
        });
        let (name2, desc2) = read_pack_meta_from_json(&extended);
        assert_eq!(name2.as_deref(), Some("Custom Pack Name"));
        assert_eq!(desc2.as_deref(), Some("Some description"));
    }

    #[test]
    fn test_list_packs_resourcepacks_auto_migrates_and_reads_options() {
        let dir = temp_dir("list_packs_test");
        let instances_dir = dir.join("instances");
        let instance_name = "PackTest";

        let inst = Instance {
            name: instance_name.into(),
            mc_version: "1.20.1".into(),
            loader: LoaderType::Vanilla,
            loader_version: None,
            loader_profile: None,
            memory_mb: None,
            jvm_args: None,
            gc_preset: None,
            java_path: None,
            resolution: None,
            icon: None,
            banner: None,
            created_at: Utc::now().to_rfc3339(),
            last_played: None,
            play_time_seconds: 0,
            notes: String::new(),
        };
        save_instance(&instances_dir, &inst, None).unwrap();

        let mc_dir = instances_dir.join(instance_name).join(".minecraft");
        let rp_dir = mc_dir.join("resourcepacks");
        std::fs::create_dir_all(&rp_dir).unwrap();

        // Create one pack as .disabled and one pack as active
        std::fs::write(rp_dir.join("OldDisabledPack.zip.disabled"), b"dummy").unwrap();
        std::fs::write(rp_dir.join("ActivePack.zip"), b"dummy").unwrap();

        // Write options.txt with only ActivePack enabled
        let options_content = "resourcePacks:[\"vanilla\",\"file/ActivePack.zip\"]\n";
        std::fs::write(mc_dir.join("options.txt"), options_content).unwrap();

        let packs = list_packs(&instances_dir, instance_name, "resourcepacks").unwrap();

        // Verify OldDisabledPack was migrated on disk
        assert!(
            rp_dir.join("OldDisabledPack.zip").exists(),
            "Migrated to .zip"
        );
        assert!(
            !rp_dir.join("OldDisabledPack.zip.disabled").exists(),
            "Old .disabled removed"
        );

        // Verify active statuses
        let active = packs
            .iter()
            .find(|p| p.filename == "ActivePack.zip")
            .expect("ActivePack found");
        assert!(active.enabled, "ActivePack should be enabled");

        let old = packs
            .iter()
            .find(|p| p.filename == "OldDisabledPack.zip")
            .expect("OldDisabledPack found");
        assert!(
            !old.enabled,
            "OldDisabledPack should be disabled in options.txt"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_atomic_write_file_roundtrip_and_safety() {
        let dir = temp_dir("atomic_test");
        let target = dir.join("options.txt");

        // 1. Initial write
        atomic_write_file(&target, "version:1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "version:1\n");

        // 2. Replacement
        atomic_write_file(&target, "version:2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "version:2\n");
        assert!(
            !dir.join("options.tmp").exists(),
            "tmp file must be cleaned up"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_atomic_write_file_failure_leaves_original_intact() {
        let dir = temp_dir("atomic_fail_test");
        let target = dir.join("options.txt");
        let original_content = "original:true\n";
        atomic_write_file(&target, original_content).unwrap();

        // Simulate write failure by creating options.tmp as a directory so std::fs::write(&tmp, ...) fails
        let tmp = target.with_extension("tmp");
        std::fs::create_dir_all(&tmp).unwrap();

        let res = atomic_write_file(&target, "corrupted:true\n");
        assert!(res.is_err(), "Must return error when tmp cannot be written");

        // Verify original file is 100% intact and was not lost or truncated
        assert_eq!(std::fs::read_to_string(&target).unwrap(), original_content);

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_atomic_write_file_rollback_when_replace_fails() {
        let dir = temp_dir("atomic_rollback_test");
        let target = dir.join("options.txt");
        let old = target.with_extension("old");
        let tmp = target.with_extension("tmp");
        let original_content = "original:true\ngameplay:smooth\n";
        atomic_write_file(&target, original_content).unwrap();

        // Custom rename function to simulate:
        // 1. Direct replace (tmp -> target) fails (e.g. Windows sharing violation)
        // 2. Safe fallback: rename target -> old succeeds
        // 3. Rename tmp -> target fails (e.g. AV scan / permission denied on new destination)
        // 4. Rollback: rename old -> target succeeds
        let rename_mock = |from: &Path, to: &Path| -> std::io::Result<()> {
            if from == tmp && to == target {
                // Fail any attempt to rename tmp -> target
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Simulated rename tmp -> target failure",
                ))
            } else {
                // Allow target -> old and old -> target
                std::fs::rename(from, to)
            }
        };

        let res = atomic_write_file_impl(&target, "new_broken_content:true\n", rename_mock);

        // 1. Operation must return an error
        assert!(
            res.is_err(),
            "Must report error when replacing target fails"
        );

        // 2. Original options.txt must be restored from .old and be 100% intact
        assert!(target.exists(), "Target file options.txt must exist");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            original_content,
            "Target file options.txt must contain original content intact"
        );

        // 3. options.old must no longer exist (moved back to options.txt)
        assert!(
            !old.exists(),
            "options.old must be moved back to options.txt"
        );

        // 4. Temporary file options.tmp must be cleaned up
        assert!(!tmp.exists(), "Temporary file options.tmp must be removed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_instance_never_overwrites_an_existing_instance() {
        let dir = temp_dir("save_duplicate_name");
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();

        let alpha = Instance::new("Alpha", "1.20.1", LoaderType::Vanilla, None);
        let beta = Instance::new("Beta", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &alpha, None).unwrap();
        save_instance(&instances_dir, &beta, None).unwrap();

        let beta_config = instances_dir.join("Beta").join("instance.json");
        let beta_before = std::fs::read_to_string(&beta_config).unwrap();

        let mut renamed = alpha.clone();
        renamed.name = "Beta".to_string();
        let error = save_instance(&instances_dir, &renamed, Some("Alpha")).unwrap_err();
        assert!(error.to_string().contains("already exists"), "got: {error}");

        assert_eq!(
            std::fs::read_to_string(&beta_config).unwrap(),
            beta_before,
            "the existing instance's metadata must stay untouched"
        );
        assert!(
            instances_dir.join("Alpha").join("instance.json").is_file(),
            "the source instance must not be renamed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn case_only_rename_is_allowed_and_updates_metadata() {
        let dir = temp_dir("case_only_rename");
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();

        let original = Instance::new("Alpha", "1.20.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &original, None).unwrap();

        let mut renamed = original.clone();
        renamed.name = "alpha".to_string();
        save_instance(&instances_dir, &renamed, Some("Alpha")).unwrap();

        let listed = list_instances(&instances_dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "alpha");
        let on_disk = std::fs::read_dir(&instances_dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .find(|name| name.eq_ignore_ascii_case("alpha"))
            .expect("the instance directory must still exist");
        assert_eq!(on_disk, "alpha", "the on-disk casing must change");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_marker(root: &Path, marker: &str, bytes: &[u8]) {
        let path = root.join(".minecraft").join(marker);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn save_instance_rejects_unsafe_old_and_new_names() {
        let dir = temp_dir("unsafe_instance_names");
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let good = Instance::new("Good", "1.20.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &good, None).unwrap();
        write_marker(&instances_dir.join("Good"), "keep.txt", b"keep");

        let bad_names = [
            "",
            ".",
            "..",
            "../Outside",
            "..\\Outside",
            "/Outside",
            "\\Outside",
            "C:\\Outside",
            "C:Outside",
            "Sub/Child",
            "Sub\\Child",
            "CON",
            "CON.txt",
            "Name.",
            " Name",
            "Name ",
            "bad:name",
            "bad?name",
        ];
        for bad in bad_names {
            let mut instance = good.clone();
            instance.name = bad.to_string();
            assert!(
                save_instance(&instances_dir, &instance, None).is_err(),
                "new name {bad:?} must be rejected"
            );

            let mut renamed = good.clone();
            renamed.name = "Renamed".to_string();
            assert!(
                save_instance(&instances_dir, &renamed, Some(bad)).is_err(),
                "old name {bad:?} must be rejected"
            );
            assert!(
                !instances_dir.join("Renamed").exists(),
                "a rejected rename must not create the target for {bad:?}"
            );
            assert_eq!(
                std::fs::read(instances_dir.join("Good").join(".minecraft/keep.txt")).unwrap(),
                b"keep",
                "the source instance must stay intact for {bad:?}"
            );
        }
        assert!(!dir.join("Outside").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn instance_name_reservation_blocks_competing_directory_operations() {
        let dir = temp_dir("instance_name_registry");
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let source = Instance::new("Source", "1.20.1", LoaderType::Vanilla, None);
        let target = Instance::new("Target", "1.21.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &source, None).unwrap();
        save_instance(&instances_dir, &target, None).unwrap();
        write_marker(&instances_dir.join("Target"), "keep.txt", b"target");
        write_marker(&instances_dir.join("Source"), "source.txt", b"source");
        let target_config = instances_dir.join("Target").join("instance.json");
        let target_before = std::fs::read_to_string(&target_config).unwrap();

        let guard = InstanceNameReservation::acquire(&instances_dir, &["Target"]).unwrap();
        assert!(
            InstanceNameReservation::acquire(&instances_dir, &["target"]).is_err(),
            "the registry must be case-insensitive"
        );
        assert!(
            duplicate_instance(
                &instances_dir,
                "Source",
                "Target",
                DuplicateExclusions::default(),
                &crate::launch_state::LaunchRegistry::new(),
            )
            .is_err(),
            "a copy must not target a reserved name"
        );
        assert!(
            delete_instance(&instances_dir, "Target").is_err(),
            "a delete must not remove a reserved name"
        );
        let created = Instance::new("Target", "1.22.1", LoaderType::Vanilla, None);
        assert!(
            create_instance(
                &instances_dir,
                &created,
                &crate::launch_state::LaunchRegistry::new()
            )
            .is_err(),
            "a create must not claim a reserved name"
        );
        let mut renamed = source.clone();
        renamed.name = "Target".to_string();
        assert!(
            save_instance(&instances_dir, &renamed, Some("Source")).is_err(),
            "a rename must not claim a reserved target"
        );

        assert_eq!(
            std::fs::read_to_string(&target_config).unwrap(),
            target_before,
            "the reserved target must stay untouched"
        );
        assert_eq!(
            std::fs::read(instances_dir.join("Source").join(".minecraft/source.txt")).unwrap(),
            b"source",
            "the source must stay intact"
        );
        drop(guard);
        assert!(InstanceNameReservation::acquire(&instances_dir, &["Target"]).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn name_reservation_canonicalizes_junction_aliases() {
        use std::os::windows::process::CommandExt;
        let dir = temp_dir("instance_alias_registry");
        let real = dir.join("real-instances");
        std::fs::create_dir_all(&real).unwrap();
        let alias = dir.join("alias-instances");
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .raw_arg(format!(
                "mklink /J \"{}\" \"{}\"",
                alias.display(),
                real.display()
            ))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !status.map(|s| s.success()).unwrap_or(false) {
            eprintln!("skipping junction alias test: privilege unavailable");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        // Two lexical paths to the same directory must share one registry key.
        let guard = InstanceNameReservation::acquire(&real, &["Pack"]).expect("real path");
        assert!(
            InstanceNameReservation::acquire(&alias, &["Pack"]).is_err(),
            "a junction alias must not bypass the name reservation"
        );
        drop(guard);
        assert!(InstanceNameReservation::acquire(&alias, &["Pack"]).is_ok());

        let _ = std::fs::remove_dir(&alias);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_renames_to_the_same_target_have_exactly_one_winner() {
        use std::sync::{Arc, Barrier};

        let dir = temp_dir("concurrent_rename");
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();
        let alpha = Instance::new("Alpha", "1.20.1", LoaderType::Vanilla, None);
        let bravo = Instance::new("Bravo", "1.20.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &alpha, None).unwrap();
        save_instance(&instances_dir, &bravo, None).unwrap();
        write_marker(&instances_dir.join("Alpha"), "alpha.txt", b"alpha");
        write_marker(&instances_dir.join("Bravo"), "bravo.txt", b"bravo");

        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for mut instance in [alpha, bravo] {
            let instances_dir = instances_dir.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let old_name = instance.name.clone();
                instance.name = "Target".to_string();
                barrier.wait();
                save_instance(&instances_dir, &instance, Some(&old_name))
                    .map(|_| old_name)
                    .map_err(|e| e.to_string())
            }));
        }
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("rename worker must not panic"))
            .collect();
        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "exactly one rename may win: {results:?}"
        );

        let target = instances_dir.join("Target");
        assert!(target.join("instance.json").is_file());
        let alpha_marker = target.join(".minecraft/alpha.txt").is_file();
        let bravo_marker = target.join(".minecraft/bravo.txt").is_file();
        assert_ne!(
            alpha_marker, bravo_marker,
            "the target must contain exactly one source tree, never a merge"
        );

        let loser_dir = if alpha_marker {
            instances_dir.join("Bravo")
        } else {
            instances_dir.join("Alpha")
        };
        assert!(
            loser_dir.join("instance.json").is_file(),
            "the losing source instance must stay intact"
        );
        let leftovers: Vec<String> = std::fs::read_dir(&instances_dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".voidlauncher-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no copy/rename staging directories may remain: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_fallback_never_merges_and_removes_the_source_only_after_success() {
        let dir = temp_dir("copy_fallback");
        let source = dir.join("Source");
        std::fs::create_dir_all(source.join(".minecraft/mods")).unwrap();
        std::fs::write(source.join(".minecraft/mods/source.jar"), b"source").unwrap();
        std::fs::write(source.join("instance.json"), b"{}").unwrap();

        let existing = dir.join("Existing");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("keep.txt"), b"keep").unwrap();
        let err = copy_tree_to_new_path(&source, &existing, &|_| false).unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {err}");
        assert_eq!(std::fs::read(existing.join("keep.txt")).unwrap(), b"keep");
        assert!(source.join(".minecraft/mods/source.jar").is_file());

        // Simulate fs::rename failing (Windows sharing violation): the
        // fallback copies to a hidden sibling, renames it into place, and only
        // then removes the source.
        let moved = dir.join("Moved");
        move_directory_with(&source, &moved, "Source", &|_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated locked directory",
            ))
        })
        .expect("the copy fallback must succeed");
        assert_eq!(
            std::fs::read(moved.join(".minecraft/mods/source.jar")).unwrap(),
            b"source"
        );
        assert!(!source.exists(), "the source is removed after a full copy");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".voidlauncher-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging must be cleaned: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_instances_ignores_restore_staging_directories() {
        let dir = temp_dir("list_ignores_staging");
        let instances_dir = dir.join("instances");
        std::fs::create_dir_all(&instances_dir).unwrap();

        let real = Instance::new("Real", "1.20.1", LoaderType::Vanilla, None);
        save_instance(&instances_dir, &real, None).unwrap();

        let staging = instances_dir.join(".void-restore-tmp-test");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(
            staging.join("instance.json"),
            serde_json::to_string(&real).unwrap(),
        )
        .unwrap();

        let listed = list_instances(&instances_dir).unwrap();
        assert_eq!(listed.len(), 1, "staging directory must be hidden");
        assert_eq!(listed[0].name, "Real");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_summaries_expose_pending_state_without_persisting_it() {
        let dir = temp_dir("summary_pending");
        let instances_dir = dir.join("instances");
        let instance = Instance::new("Pack", "1.20.1", LoaderType::Vanilla, None);
        create_instance(
            &instances_dir,
            &instance,
            &crate::launch_state::LaunchRegistry::new(),
        )
        .unwrap();

        let ready = list_instance_summaries(&instances_dir).unwrap();
        assert_eq!(ready.len(), 1);
        assert!(
            ready[0].setup_state.is_none(),
            "a ready instance has no pending state"
        );

        crate::instance_readiness::write_pending(
            &instances_dir.join("Pack"),
            &crate::instance_readiness::PendingSetup {
                stage: crate::instance_readiness::SetupStage::Preparing,
                operation: "import".to_string(),
                started_at: "2026-11-14T00:00:00Z".to_string(),
                token: "summary-owner".to_string(),
            },
        )
        .unwrap();

        let pending = list_instance_summaries(&instances_dir).unwrap();
        let state = pending[0].setup_state.as_ref().expect("pending state");
        assert_eq!(state.stage, "preparing");
        assert_eq!(state.operation, "import");

        // The state is derived from the marker, never written into metadata.
        let raw = std::fs::read_to_string(instances_dir.join("Pack/instance.json")).unwrap();
        assert!(
            !raw.contains("setup_state"),
            "readiness must not leak into instance.json"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

use crate::config::AppConfig;
use crate::error::{LauncherError, Result};
use crate::instances::Instance;
use crate::java::{detect_java_installations, JavaInstallation};
use crate::jvm::{
    build_jvm_args, detect_java_major, strip_gc_selection_flags, strip_removed_jvm_flags, GcPreset,
};
use crate::versions::{get_game_arguments, get_jvm_arguments, VersionInfo};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Open (create) files to receive a raw byte-for-byte copy of the game
/// process stdout/stderr (crash forensics).
///
/// Returns `None` for a stream when its file cannot be created — the unified
/// session log still receives every line via the pipe readers, so a tee
/// failure must never block the game.
fn open_game_output_files(
    data_dir: &std::path::Path,
    instance_name: &str,
) -> Result<(Option<std::fs::File>, Option<std::fs::File>)> {
    use std::io::Write;

    let game_logs_dir = data_dir.join("logs").join("game");
    std::fs::create_dir_all(&game_logs_dir)
        .map_err(|e| LauncherError::Launch(format!("Failed to create game logs dir: {}", e)))?;

    let now = chrono::Local::now();
    let timestamp = now.format("%Y%m%d_%H%M%S");
    let safe_name = crate::game_logs::sanitize_instance_name(instance_name);
    let base = game_logs_dir.join(format!("{}_{}", safe_name, timestamp));
    let stdout_path = format!("{}.stdout.log", base.display());
    let stderr_path = format!("{}.stderr.log", base.display());

    let open_raw = |path: &str, label: &str| -> Option<std::fs::File> {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!(target: "launcher", "Raw {} tee unavailable ({}): {}", label, path, e);
                None
            }
        }
    };
    let stdout_file = open_raw(&stdout_path, "stdout");
    let stderr_file = open_raw(&stderr_path, "stderr");

    let header = format!(
        "VoidLauncher game stdout\nInstance: {}\nStarted: {}\n{}\n",
        instance_name,
        now.format("%d/%m/%Y %H:%M:%S"),
        "=".repeat(60),
    );
    if let Some(f) = &stdout_file {
        let mut f = f;
        let _ = writeln!(f, "{}", header);
    }
    if let Some(f) = &stderr_file {
        let mut f = f;
        let _ = writeln!(f, "{}", header);
    }

    tracing::info!(target: "launcher", "Game stdout -> {}, stderr -> {}", stdout_path, stderr_path);
    Ok((stdout_file, stderr_file))
}

/// Account credentials handed to the launched game process.
pub struct LaunchAuth<'a> {
    pub access_token: &'a str,
    pub uuid: &'a str,
    pub username: &'a str,
}

/// Fully composed Java invocation for one launch: paths resolved, arguments
/// substituted and validated, but no process created yet.
///
/// This is the process-level projection of [`LaunchProfile`]: `args` is the
/// exact Java argv. Tests assert against it, and [`spawn_launch`] executes it.
/// Java selection is injected: the caller passes the already-probed path and
/// major version into the profile.
pub struct LaunchCommand {
    pub java_path: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

const LAUNCHER_NAME: &str = "VoidLauncher";
const ENTRY_JAR_NAME: &str = "VoidLauncherEntry.jar";
const ENTRY_MAIN_CLASS: &str = "voidlauncher.entry.VoidLauncherEntry";
const CLASSPATH_SEPARATOR: &str = ";";

/// Semantic description of one launch: what to start and with which arguments.
///
/// `LaunchProfile` is pure data. It contains no Java executable and no flattened
/// argv — the process layer is [`LaunchCommand`], produced by
/// [`LaunchProfile::to_command`]. Building a profile never writes files; the
/// only filesystem input is the injected [`LaunchFs`] probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchProfile {
    /// Effective main class: the loader's main class or Mojang's `mainClass`.
    /// For vanilla the profile also sets `traits.entry_wrapper`, which routes
    /// the launch through `VoidLauncherEntry.jar`.
    pub main_class: String,
    /// Ordered, deduplicated and validated classpath. The entry JAR is first;
    /// loader libraries precede vanilla ones.
    pub classpath: Vec<PathBuf>,
    pub natives_dir: PathBuf,
    /// JVM arguments without the `-cp` pair and without GC-selection flags
    /// from Mojang/loader metadata. The user's preset and custom args are here.
    pub jvm_args: Vec<String>,
    /// Game arguments with every known placeholder expanded.
    pub game_args: Vec<String>,
    pub traits: LaunchTraits,
}

/// Behavioural flags that are not fully represented by the argument list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaunchTraits {
    /// The loader profile's `game_args` came from a legacy
    /// `minecraftArguments` string and replace the vanilla arguments.
    pub legacy_args: bool,
    /// The manifest's `has_custom_resolution` feature rule was enabled, so
    /// `--width/--height` came from the manifest itself.
    pub has_custom_resolution: bool,
    /// Quick Play join arguments were added (rather than the legacy
    /// `--server/--port` pair).
    pub quick_play: bool,
    /// Launch through `VoidLauncherEntry.jar`; vanilla only.
    pub entry_wrapper: bool,
}

impl LaunchProfile {
    /// Project the profile onto a concrete Java invocation.
    ///
    /// This is the only place that decides how the profile is flattened into
    /// argv: JVM arguments, `-cp <classpath>`, the entry/main class, then game
    /// arguments.
    pub fn to_command(&self, java_path: PathBuf, cwd: PathBuf) -> LaunchCommand {
        let mut args = Vec::with_capacity(self.jvm_args.len() + self.game_args.len() + 4);
        args.extend(self.jvm_args.iter().cloned());
        args.push("-cp".to_string());
        args.push(crate::versions::join_classpath(&self.classpath));
        if self.traits.entry_wrapper {
            args.push(format!("-Dvoidlauncher.mainClass={}", self.main_class));
            args.push(ENTRY_MAIN_CLASS.to_string());
        } else {
            args.push(self.main_class.clone());
        }
        args.extend(self.game_args.iter().cloned());
        LaunchCommand {
            java_path,
            args,
            cwd,
        }
    }
}

/// Read-only filesystem queries the profile builder is allowed to make.
///
/// Writes (entry JAR, natives extraction, directories) happen in
/// [`prepare_launch_files`]. Injecting this probe lets the composition be
/// tested without touching disk.
pub trait LaunchFs: crate::versions::ClasspathProbe {
    /// Does the path exist (file or directory)?
    fn exists(&self, path: &Path) -> bool;
    /// Does a processor-generated SRG JAR for this Minecraft version exist?
    fn has_srg_jar(&self, libraries_dir: &Path, mc_version: &str) -> bool;
}

/// Production [`LaunchFs`] backed by `std::fs`.
pub struct RealLaunchFs;

impl crate::versions::ClasspathProbe for RealLaunchFs {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

impl LaunchFs for RealLaunchFs {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn has_srg_jar(&self, libraries_dir: &Path, mc_version: &str) -> bool {
        // Check if THIS version's processor-generated SRG JAR exists
        // (libraries/net/minecraft/client/<mcp-version>/client-<mcp-version>-srg.jar).
        // The check MUST be scoped to the instance's MC version: old Forge
        // (e.g. 1.8.9) has no processor JARs and still needs client.jar on the
        // classpath, while a global scan would see SRG JARs from other
        // versions (1.14+/1.19+) and wrongly strip client.jar from old ones.
        let client_base = libraries_dir.join("net").join("minecraft").join("client");
        let prefix = format!("client-{}-", mc_version);
        let Ok(entries) = std::fs::read_dir(&client_base) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                // Scan inside version subdirectory
                std::fs::read_dir(&path)
                    .ok()
                    .map(|inner| {
                        inner.flatten().any(|f| {
                            let name = f.file_name().to_string_lossy().to_string();
                            name.starts_with(&prefix) && name.ends_with("-srg.jar")
                        })
                    })
                    .unwrap_or(false)
            } else {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with(&prefix) && name.ends_with("-srg.jar")
            }
        })
    }
}

/// All inputs needed to build a [`LaunchProfile`]. Paths are explicit so the
/// builder does not have to reach into `AppConfig`.
pub struct LaunchProfileRequest<'a> {
    pub instance: &'a Instance,
    pub version_info: &'a VersionInfo,
    pub auth: &'a LaunchAuth<'a>,
    pub server: Option<(&'a str, Option<u16>)>,
    pub session_log_path: Option<&'a str>,
    pub java_major: u32,
    pub versions_dir: &'a Path,
    pub libraries_dir: &'a Path,
    pub assets_dir: &'a Path,
    pub game_dir: &'a Path,
    pub default_memory_mb: u32,
    pub default_gc_preset: &'a str,
    pub default_jvm_args: &'a [String],
}

/// Expand every placeholder the launcher knows about.
struct PlaceholderContext<'a> {
    username: &'a str,
    uuid: &'a str,
    access_token: &'a str,
    auth_session: &'a str,
    version_name: &'a str,
    version_type: &'a str,
    game_dir: &'a Path,
    assets_dir: &'a Path,
    game_assets_dir: &'a Path,
    assets_index: &'a str,
    resolution_width: &'a str,
    resolution_height: &'a str,
    natives_dir: &'a Path,
    library_dir: &'a Path,
    classpath: &'a str,
}

impl PlaceholderContext<'_> {
    fn expand(&self, arg: &str) -> String {
        arg.replace("${auth_player_name}", self.username)
            .replace("${version_name}", self.version_name)
            .replace("${game_directory}", &self.game_dir.to_string_lossy())
            .replace("${assets_root}", &self.assets_dir.to_string_lossy())
            .replace("${game_assets}", &self.game_assets_dir.to_string_lossy())
            .replace("${assets_index_name}", self.assets_index)
            .replace("${auth_uuid}", self.uuid)
            .replace("${auth_access_token}", self.access_token)
            .replace("${auth_session}", self.auth_session)
            .replace("${user_type}", "msa")
            .replace("${version_type}", self.version_type)
            .replace("${auth_xuid}", "0")
            .replace("${clientid}", "")
            .replace("${user_properties}", "{}")
            .replace("${resolution_width}", self.resolution_width)
            .replace("${resolution_height}", self.resolution_height)
            .replace("${natives_directory}", &self.natives_dir.to_string_lossy())
            .replace("${library_directory}", &self.library_dir.to_string_lossy())
            .replace("${classpath_separator}", CLASSPATH_SEPARATOR)
            .replace("${classpath}", self.classpath)
            .replace("${launcher_name}", LAUNCHER_NAME)
            .replace("${launcher_version}", LAUNCHER_VERSION)
    }

    /// Tokens [`Self::expand`] knows how to replace. Kept next to `expand` so
    /// the two lists cannot drift silently; `placeholder_expansion_covers_every_known_token`
    /// exercises every entry through `expand_checked`.
    fn is_known(token: &str) -> bool {
        matches!(
            token,
            "${auth_player_name}"
                | "${version_name}"
                | "${game_directory}"
                | "${assets_root}"
                | "${game_assets}"
                | "${assets_index_name}"
                | "${auth_uuid}"
                | "${auth_access_token}"
                | "${auth_session}"
                | "${user_type}"
                | "${version_type}"
                | "${auth_xuid}"
                | "${clientid}"
                | "${user_properties}"
                | "${resolution_width}"
                | "${resolution_height}"
                | "${natives_directory}"
                | "${library_directory}"
                | "${classpath_separator}"
                | "${classpath}"
                | "${launcher_name}"
                | "${launcher_version}"
        )
    }
}

/// Expand a metadata-provided argument and fail loudly if a placeholder
/// survived.
///
/// A literal `${...}` handed to Java is never useful: it either becomes a
/// bogus property value (KI-5) or silently changes the launch. Real metadata
/// only uses the tokens handled by [`PlaceholderContext`]. User-provided JVM
/// arguments are intentionally not passed through this check — they are the
/// user's responsibility and may legitimately contain `${...}`.
fn expand_checked(arg: &str, placeholders: &PlaceholderContext<'_>, kind: &str) -> Result<String> {
    if let Some(token) = unknown_template_placeholder(arg) {
        return Err(LauncherError::Launch(format!(
            "Unresolved {} placeholder after expansion: {} (in {})",
            kind, token, arg
        )));
    }
    Ok(placeholders.expand(arg))
}

/// Find a `${...}` token in a metadata *template* that the launcher does not
/// know how to replace.
///
/// Only the template is checked. The expanded value may legitimately contain
/// `${...}` when a substituted value (an instance directory, a username, ...)
/// contains it; that text is data, not a placeholder.
fn unknown_template_placeholder(template: &str) -> Option<String> {
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Some(rest[start..].to_string());
        };
        let token = &rest[start..start + 2 + end + 1];
        if !PlaceholderContext::is_known(token) {
            return Some(token.to_string());
        }
        rest = &rest[start + 2 + end + 1..];
    }
    None
}

/// Compose a launch profile from pure inputs and an injected FS probe.
///
/// No files are written and no network is touched. The caller is responsible
/// for preparing the entry JAR, natives directory and game directory first
/// (see [`prepare_and_build_launch_profile`]).
pub fn build_launch_profile(
    request: &LaunchProfileRequest<'_>,
    fs: &dyn LaunchFs,
) -> Result<LaunchProfile> {
    let instance = request.instance;
    let version_info = request.version_info;

    // The version id is used as a directory name below and ultimately came
    // from remote metadata — refuse anything that is not a bare name.
    crate::import::check_safe_file_name(&version_info.id)
        .map_err(|_| LauncherError::Launch(format!("Invalid version id: {}", version_info.id)))?;

    let version_dir = request.versions_dir.join(&version_info.id);
    let client_jar = version_dir.join("client.jar");
    let entry_jar = version_dir.join("entry").join(ENTRY_JAR_NAME);
    let natives_dir = version_dir.join("natives");

    if !fs.is_file(&client_jar) {
        return Err(LauncherError::Launch(format!(
            "Client JAR is missing for version {}: {}",
            version_info.id,
            client_jar.display()
        )));
    }
    if !fs.is_file(&entry_jar) {
        return Err(LauncherError::Launch(format!(
            "Launch entry JAR is missing: {}",
            entry_jar.display()
        )));
    }

    // Vanilla classpath (includes client.jar last).
    let vanilla_entries = crate::versions::build_classpath_entries(
        version_info,
        request.libraries_dir,
        &client_jar,
        fs,
    )?;

    // Collect loader libraries (if any).
    let mut loader_entries: Vec<PathBuf> = Vec::new();
    if let Some(profile) = &instance.loader_profile {
        tracing::info!(target: "launcher", "Mod loader: main_class={}", profile.main_class);
        for lib in &profile.libraries {
            let lib_path = loader_library_path(request.libraries_dir, lib)?;
            if fs.is_file(&lib_path) {
                loader_entries.push(lib_path);
            } else {
                return Err(LauncherError::Launch(format!(
                    "Mod loader library is missing: {} ({})",
                    lib.name,
                    lib_path.display()
                )));
            }
        }
    }

    // NeoForge/Forge profiles override some vanilla libraries with newer
    // versions (e.g. NeoForge 21.x ships asm 9.8 while MC 1.21.4 vanilla
    // carries asm 9.6). Both must not sit on the classpath together: the
    // module system aborts with "Module ... already on the module path but
    // class-path contains it" when the loader's -p modules collide with a
    // different-version JAR on the classpath. Skip any vanilla library
    // whose artifact (group:artifact) is provided by a loader library that
    // actually exists on disk (loader wins — dedup by path couldn't handle
    // the same artifact under different versions).
    let mut skip_vanilla_paths: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::new();
    if let Some(profile) = &instance.loader_profile {
        let loader_artifacts: std::collections::HashSet<String> = profile
            .libraries
            .iter()
            .filter(|lib| {
                crate::import::check_safe_remote_path(&lib.path).is_ok()
                    && fs.exists(&request.libraries_dir.join(&lib.path))
            })
            .filter_map(|lib| artifact_key(&lib.name))
            .collect();
        if !loader_artifacts.is_empty() {
            for lib in &version_info.libraries {
                if !crate::versions::should_include_library(lib) {
                    continue;
                }
                let Some(key) = artifact_key(&lib.name) else {
                    continue;
                };
                if loader_artifacts.contains(&key) {
                    if let Some(artifact) = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref())
                    {
                        // Remote metadata path — never let it point outside
                        // the libraries directory, even in the dedup set.
                        if crate::import::check_safe_remote_path(&artifact.path).is_err() {
                            continue;
                        }
                        let path = request.libraries_dir.join(&artifact.path);
                        if fs.exists(&path) {
                            skip_vanilla_paths.insert(path);
                        }
                    }
                }
            }
        }
    }

    // Loader libraries first (highest priority), then vanilla libraries.
    // Deduplicate by exact path after the override filter.
    let mut classpath: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for entry in loader_entries {
        if seen.insert(entry.clone()) {
            classpath.push(entry);
        }
    }
    for entry in vanilla_entries {
        if skip_vanilla_paths.contains(&entry) {
            tracing::debug!(target: "launcher", "Skipping vanilla library overridden by loader: {}", entry.display());
            continue;
        }
        if seen.insert(entry.clone()) {
            classpath.push(entry);
        }
    }

    // For Forge/NeoForge instances, remove vanilla client.jar to avoid JPMS
    // split-package conflict with processor-generated JARs (client-*-srg.jar,
    // forge-*-client.jar). The processor JARs contain the same packages and
    // replace client.jar. They are NOT in the loader profile's library list
    // (they're discovered by MinecraftLocator / the production client
    // provider at runtime), so the presence check is part of the FS probe.
    if matches!(
        instance.loader,
        crate::instances::LoaderType::Forge | crate::instances::LoaderType::NeoForge
    ) && fs.has_srg_jar(request.libraries_dir, &instance.mc_version)
    {
        let before = classpath.len();
        classpath.retain(|entry| entry != &client_jar);
        if classpath.len() < before {
            tracing::debug!(target: "launcher", "Removed vanilla client.jar for Forge (replaced by srg/extra JARs)");
        }
    }

    // VoidLauncherEntry.jar is first: the JVM derives the automatic module
    // from the JAR that contains the main class, and the entry point lives
    // outside the obfuscated client.jar. Vanilla launches route through it;
    // loader launches keep it on the classpath but use the loader main class.
    classpath.insert(0, entry_jar);

    // JVM arguments.
    //   - Memory: instance override > launcher default. Xms == Xmx.
    //   - Preset: instance override > default_gc_preset (safe default).
    //   - Any custom instance.jvm_args are appended AFTER the preset.
    let memory_mb = instance.memory_mb.unwrap_or(request.default_memory_mb);
    let preset_str = instance
        .gc_preset
        .as_deref()
        .unwrap_or(request.default_gc_preset);
    let requested_preset = GcPreset::from_str(preset_str);
    let (mut jvm_args, effective_preset) =
        build_jvm_args(requested_preset, memory_mb, request.java_major);
    tracing::info!(target: "launcher", "Memory: Xms=Xmx={}M, preset={:?} (requested {:?})",
              memory_mb, effective_preset, requested_preset);

    // User-provided custom args go AFTER the preset so they can override
    // anything it decided. Instance-level args take priority over the global
    // default. Both are stripped of GC-selection flags: the chosen preset is
    // the single source of truth for which GC the JVM starts with.
    let user_args = instance
        .jvm_args
        .as_deref()
        .unwrap_or(request.default_jvm_args);
    for arg in strip_gc_selection_flags(user_args) {
        jvm_args.push(arg);
    }

    let classpath_string = crate::versions::join_classpath(&classpath);

    // 1.6.x and older read flat files straight from --assetsDir, so the
    // launcher points them at the mirrored virtual/legacy tree.
    let game_assets_dir = if version_info.assets == "legacy" {
        request.assets_dir.join("virtual").join("legacy")
    } else {
        request.assets_dir.to_path_buf()
    };

    // Old session format (1.6.x): --session <token>:<uuid>:<username>.
    // "0" is the conventional offline token.
    let auth_session = format!("0:{}:{}", request.auth.uuid, request.auth.username);

    // Resolution: when the instance sets it, the manifest's
    // `has_custom_resolution` feature rule emits --width/--height for versions
    // with structured arguments; for legacy versions the manifest has no
    // resolution placeholders, so they are appended below — but only if the
    // manifest did not already provide them.
    let (res_w, res_h) = match &instance.resolution {
        Some(res) => (res.width.to_string(), res.height.to_string()),
        None => ("1280".to_string(), "720".to_string()),
    };

    let placeholders = PlaceholderContext {
        username: request.auth.username,
        uuid: request.auth.uuid,
        access_token: request.auth.access_token,
        auth_session: &auth_session,
        version_name: &version_info.id,
        version_type: &version_info.version_type,
        game_dir: request.game_dir,
        assets_dir: request.assets_dir,
        game_assets_dir: &game_assets_dir,
        assets_index: &version_info.assets,
        resolution_width: &res_w,
        resolution_height: &res_h,
        natives_dir: &natives_dir,
        library_dir: request.libraries_dir,
        classpath: &classpath_string,
    };

    // Add mod loader JVM args (skip the standalone `-cp` / `${classpath}`
    // pair; the final classpath is added by `to_command`). Loader profiles
    // (Forge/NeoForge in particular) sometimes include their own GC selector,
    // so those are stripped too — the user's chosen preset wins.
    if let Some(profile) = &instance.loader_profile {
        let loader_args = strip_gc_selection_flags(&profile.jvm_args);
        let mut i = 0;
        while i < loader_args.len() {
            let raw = &loader_args[i];
            if raw == "-cp" || raw == "${classpath}" {
                i += 1;
                continue;
            }
            let processed = expand_checked(raw, &placeholders, "JVM")?;

            // Diagnostics: log -p value at INFO and check module JARs exist.
            if raw == "-p" && i + 1 < loader_args.len() {
                let modpath = placeholders.expand(&loader_args[i + 1]);
                tracing::info!(target: "launcher", "Module path (-p): {}", modpath);
                for (j, entry) in modpath.split(CLASSPATH_SEPARATOR).enumerate() {
                    let p = std::path::Path::new(entry);
                    if p.exists() {
                        tracing::info!(target: "launcher", "  [{}] EXISTS: {}", j, entry);
                    } else {
                        tracing::warn!(target: "launcher", "  [{}] MISSING: {}", j, entry);
                    }
                }
            }

            jvm_args.push(processed);
            i += 1;
        }
    }

    // JVM arguments from the version manifest (skip the standalone `-cp` /
    // `${classpath}` pair).
    //
    // IMPORTANT: Mojang's 1.20.5+ manifests ship `-XX:+UseG1GC` as the
    // default GC selector. If the user picked a different preset
    // (ZGC, Standard, …) we MUST drop every GC-selection flag from the
    // upstream args first — otherwise HotSpot aborts with
    // "multiple garbage collectors selected" before the game can start.
    // See `jvm::strip_gc_selection_flags` for the full list.
    let version_jvm_args = get_jvm_arguments(version_info);
    tracing::debug!(target: "launcher", "Version manifest JVM args ({} total):", version_jvm_args.len());
    for a in &version_jvm_args {
        tracing::debug!(target: "launcher", "  raw: {}", a);
    }
    let version_jvm_args = strip_gc_selection_flags(&version_jvm_args);
    tracing::debug!(target: "launcher", "After GC-strip ({} remaining):", version_jvm_args.len());
    for a in &version_jvm_args {
        tracing::debug!(target: "launcher", "  kept: {}", a);
    }
    for arg in &version_jvm_args {
        if arg == "-cp" || arg == "${classpath}" {
            continue;
        }
        jvm_args.push(expand_checked(arg, &placeholders, "JVM")?);
    }

    // Remove obsolete/invalid-on-this-Java flags from everything assembled
    // above (custom args, loader profile args, version-manifest args):
    // `-XX:+G1UnlockCommercialFeatures` never existed in any Java version
    // (old Forge profiles shipped it incorrectly), and `-XX:[+-]ZGenerational`
    // was removed in Java 23+/24 (JEP 474) where it only prints an
    // "Ignoring option" warning on every launch.
    strip_removed_jvm_flags(&mut jvm_args, request.java_major);

    // Game arguments.
    //
    // For legacy loaders (MC <= 1.12.2 Forge), the loader profile carries the
    // FULL game argument list in its `minecraftArguments` string — those
    // REPLACE the vanilla args.
    let legacy_full_args = instance
        .loader_profile
        .as_ref()
        .map(|p| p.legacy_args && !p.game_args.is_empty())
        .unwrap_or(false);

    let mut game_args: Vec<String> = Vec::new();
    if !legacy_full_args {
        for arg in get_game_arguments(version_info, instance.resolution.is_some()) {
            game_args.push(expand_checked(&arg, &placeholders, "game")?);
        }
    } else {
        tracing::info!(target: "launcher", "Using legacy loader game arguments (replaces vanilla)");
    }

    if let Some(profile) = &instance.loader_profile {
        for arg in &profile.game_args {
            game_args.push(expand_checked(arg, &placeholders, "game")?);
        }
    }

    // Add resolution if specified AND not already provided by the manifest
    // (feature rule has_custom_resolution above) or the loader profile.
    if let Some(res) = &instance.resolution {
        if !game_args.iter().any(|a| a == "--width") {
            game_args.push("--width".to_string());
            game_args.push(res.width.to_string());
        }
        if !game_args.iter().any(|a| a == "--height") {
            game_args.push("--height".to_string());
            game_args.push(res.height.to_string());
        }
    }

    // Room auto-join, version-aware. Minecraft ≤ 1.19.4 still supports the
    // legacy `--server/--port` pair; from 1.20+ (including 26.x and
    // snapshots) the client reads Quick Play arguments instead:
    // `--quickPlayMultiplayer <host>:<port>` plus `--quickPlayPath` — the
    // client writes a machine-readable join-log there on a successful
    // connection, which the launcher uses to confirm the auto-join.
    let mut quick_play = false;
    if let Some((host, Some(port))) = request.server.filter(|(h, _)| !h.trim().is_empty()) {
        let connector = crate::rooms::minecraft_connector::MinecraftConnector::default();
        let mechanism = connector.discovery().join_mechanism(&instance.mc_version);
        if let Some(mut join_args) =
            connector
                .discovery()
                .build_join_args(&instance.mc_version, host.trim(), port)
        {
            if mechanism == Some(crate::rooms::minecraft_connector::JoinMechanism::QuickPlay) {
                quick_play = true;
                // For Quick Play, also hand the client the log file to write
                // so the join can be verified (path derived from the active
                // session).
                if let Some(session_path) = request.session_log_path {
                    join_args.push("--quickPlayPath".to_string());
                    join_args.push(format!("{}.quickplay.json", session_path));
                }
            }
            tracing::info!(target: "launcher", "Room join args for MC {}: {:?}", instance.mc_version, join_args);
            game_args.extend(join_args);
        } else {
            tracing::warn!(target: "launcher", "MC {} has no auto-join mechanism; joining manually", instance.mc_version);
        }
    }

    // Main class selection:
    //   - Vanilla: use the VoidLauncherEntry wrapper to avoid module-system
    //     derivation errors (client.jar has classes in the unnamed package).
    //   - Forge/NeoForge/Fabric: use the loader profile's main class
    //     directly. Its -p (module path) contains the bootstrap JARs and the
    //     real main class lives there — Class.forName from our entry point
    //     would NOT find it because it only searches the classpath.
    let main_class = instance
        .loader_profile
        .as_ref()
        .map(|p| p.main_class.clone())
        .unwrap_or_else(|| version_info.main_class.clone());
    let entry_wrapper = instance.loader_profile.is_none();
    tracing::info!(
        target: "launcher",
        "Main class: {} ({})",
        main_class,
        if entry_wrapper { "vanilla entry wrapper" } else { "loader" }
    );

    Ok(LaunchProfile {
        main_class,
        classpath,
        natives_dir,
        jvm_args,
        game_args,
        traits: LaunchTraits {
            legacy_args: legacy_full_args,
            // The trait describes the manifest feature rule; legacy
            // `minecraftArguments` versions get `--width/--height` appended
            // directly, not through the manifest.
            has_custom_resolution: instance.resolution.is_some()
                && version_info.arguments.is_some(),
            quick_play,
            entry_wrapper,
        },
    })
}

/// Launch-owned files that must exist before the pure profile builder runs.
struct PreparedLaunch {
    game_dir: PathBuf,
}

/// Create directories, the entry JAR and extracted natives. This is the only
/// write side of launch composition; the profile builder itself is pure.
fn prepare_launch_files(
    config: &AppConfig,
    instance: &Instance,
    version_info: &VersionInfo,
) -> Result<PreparedLaunch> {
    // The version id becomes a directory name below and ultimately came from
    // remote metadata — validate it before any write or join.
    crate::import::check_safe_file_name(&version_info.id)
        .map_err(|_| LauncherError::Launch(format!("Invalid version id: {}", version_info.id)))?;

    let version_dir = config.versions_dir().join(&version_info.id);
    let client_jar = version_dir.join("client.jar");

    // Fallback: migrate old {version}.jar to client.jar.
    if !client_jar.exists() {
        let old_jar = version_dir.join(format!("{}.jar", version_info.id));
        if old_jar.exists() {
            std::fs::rename(&old_jar, &client_jar).ok();
        }
    }
    tracing::debug!(target: "launcher", "Client JAR: {:?}", client_jar);

    if !client_jar.is_file() {
        return Err(LauncherError::Launch(format!(
            "Client JAR is missing for version {}: {}",
            version_info.id,
            client_jar.display()
        )));
    }

    // Write VoidLauncherEntry.jar and keep it first on the classpath. Always
    // overwrite to ensure embedded bytes match (an old JAR from a previous
    // build may contain a class without the voidlauncher.entry package).
    let entry_dir = version_dir.join("entry");
    std::fs::create_dir_all(&entry_dir).map_err(|e| {
        LauncherError::Launch(format!(
            "Failed to create entry JAR directory {}: {}",
            entry_dir.display(),
            e
        ))
    })?;
    let entry_jar = entry_dir.join(ENTRY_JAR_NAME);
    std::fs::write(&entry_jar, crate::entry::JAR_BYTES).map_err(|e| {
        LauncherError::Launch(format!(
            "Failed to write entry JAR {}: {}",
            entry_jar.display(),
            e
        ))
    })?;
    // Remove old .class file from previous versions (was in the default
    // package and did not work with the package-based main class name).
    let _ = std::fs::remove_file(entry_dir.join("VoidLauncherEntry.class"));

    let natives_dir = version_dir.join("natives");
    std::fs::create_dir_all(&natives_dir)?;

    // Extract native libraries (DLLs) from native classifier JARs into the
    // natives directory, so old LWJGL versions can find them via
    // -Djava.library.path. The native JARs are also on the classpath (added
    // by build_classpath_entries), which covers newer LWJGL.
    crate::versions::extract_natives(version_info, &config.libraries_dir(), &natives_dir)?;

    let game_dir = instance.minecraft_dir(&config.instances_dir());
    std::fs::create_dir_all(&game_dir)?;

    Ok(PreparedLaunch { game_dir })
}

/// Prepare launch-owned files, then compose the pure launch profile.
///
/// Returns the profile and the game directory (the process working directory).
#[allow(clippy::too_many_arguments)]
pub fn prepare_and_build_launch_profile(
    config: &AppConfig,
    instance: &Instance,
    version_info: &VersionInfo,
    auth: &LaunchAuth,
    server: Option<(&str, Option<u16>)>,
    session_log_path: Option<&str>,
    java_major: u32,
) -> Result<(LaunchProfile, PathBuf)> {
    let prepared = prepare_launch_files(config, instance, version_info)?;

    let versions_dir = config.versions_dir();
    let libraries_dir = config.libraries_dir();
    let assets_dir = config.assets_dir();
    let request = LaunchProfileRequest {
        instance,
        version_info,
        auth,
        server,
        session_log_path,
        java_major,
        versions_dir: &versions_dir,
        libraries_dir: &libraries_dir,
        assets_dir: &assets_dir,
        game_dir: &prepared.game_dir,
        default_memory_mb: config.default_memory_mb,
        default_gc_preset: &config.default_gc_preset,
        default_jvm_args: &config.default_jvm_args,
    };
    let profile = build_launch_profile(&request, &RealLaunchFs)?;
    Ok((profile, prepared.game_dir))
}

/// Compose the Java command line for an instance.
///
/// Thin wrapper around [`prepare_and_build_launch_profile`] +
/// [`LaunchProfile::to_command`]. Nothing is spawned.
#[allow(clippy::too_many_arguments)]
pub fn build_launch_command(
    config: &AppConfig,
    instance: &Instance,
    version_info: &VersionInfo,
    auth: &LaunchAuth,
    server: Option<(&str, Option<u16>)>,
    session_log_path: Option<&str>,
    java_path: PathBuf,
    java_major: u32,
) -> Result<LaunchCommand> {
    tracing::info!(target: "launcher", "Building launch command for instance: {}", instance.name);
    tracing::info!(target: "launcher", "MC version: {}", instance.mc_version);
    tracing::info!(target: "launcher", "Username: {}", auth.username);
    tracing::info!(target: "launcher", "Using Java: {:?} (major {})", java_path, java_major);

    let (profile, game_dir) = prepare_and_build_launch_profile(
        config,
        instance,
        version_info,
        auth,
        server,
        session_log_path,
        java_major,
    )?;
    Ok(profile.to_command(java_path, game_dir))
}

/// Spawn the already-composed launch profile.
pub fn spawn_launch(
    profile: &LaunchProfile,
    java_path: &Path,
    cwd: &Path,
) -> Result<std::process::Child> {
    let command = profile.to_command(java_path.to_path_buf(), cwd.to_path_buf());
    let mut cmd = Command::new(&command.java_path);
    cmd.args(&command.args)
        .current_dir(&command.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd.spawn().map_err(|e| {
        let msg = if e.raw_os_error() == Some(740) {
            format!("Java requires administrator privileges. Right-click java.exe → Properties → Compatibility → disable 'Run as admin'. Java path: {}", command.java_path.display())
        } else {
            format!("Failed to launch: {}", e)
        };
        tracing::error!(target: "launcher", "FAILED to spawn Java: {}", msg);
        LauncherError::Launch(msg)
    })?;

    tracing::info!(target: "launcher", "Java process spawned with PID: {}", child.id());
    Ok(child)
}

/// Launch Minecraft for a given instance. `session_log_path` is this run's
/// game-log file (used for the Quick Play join log) — passed explicitly so
/// concurrent launches cannot mix their logs up via a global pointer.
pub fn launch_minecraft(
    config: &AppConfig,
    instance: &Instance,
    version_info: &VersionInfo,
    auth: &LaunchAuth,
    server: Option<(&str, Option<u16>)>,
    session_log_path: Option<&str>,
) -> Result<std::process::Child> {
    tracing::info!(target: "launcher", "Starting launch for instance: {}", instance.name);

    // 1. Determine Java path
    tracing::info!(target: "launcher", "Detecting Java installation...");
    let java_path = get_java_path(config, instance, version_info)?;
    tracing::info!(target: "launcher", "Using Java: {:?}", java_path);

    // 2. Probe the selected Java's major version BEFORE composing the command.
    //    This is what lets us safely fall back from ZGC to G1GC for older JDKs.
    let java_major = detect_java_major(&java_path).unwrap_or(0);
    tracing::info!(target: "launcher", "Detected Java major version: {}", java_major);

    // 3. Prepare launch-owned files, then build the pure profile.
    let (profile, game_dir) = prepare_and_build_launch_profile(
        config,
        instance,
        version_info,
        auth,
        server,
        session_log_path,
        java_major,
    )?;
    let command = profile.to_command(java_path.clone(), game_dir.clone());

    // 4. Spawn.
    tracing::debug!(target: "launcher", "Game dir: {:?}", game_dir);
    tracing::debug!(target: "launcher", "argv:");
    let sanitized: Vec<String> = command
        .args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            if a == "--accessToken" || a == "--authAccessToken" {
                command
                    .args
                    .get(i + 1)
                    .map(|_| format!("{} ***", a))
                    .unwrap_or_else(|| a.clone())
            } else if i > 0
                && (command.args[i - 1] == "--accessToken"
                    || command.args[i - 1] == "--authAccessToken")
            {
                "***".to_string()
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>();
    for (i, a) in sanitized.iter().enumerate() {
        tracing::info!(target: "launcher", "  [{}] {}", i, a);
    }
    tracing::info!(target: "launcher", "Spawning Java process...");

    let mut child = spawn_launch(&profile, &java_path, &game_dir)?;

    // Prism-style unified logging: drain BOTH pipes concurrently and append
    // every line to the current session log as it arrives (interleaved with
    // the launcher's own messages). Raw bytes are also teed to forensic
    // files. Piping is safe here — dedicated reader threads guarantee the
    // OS pipe buffers never fill up and block the game.
    if let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) {
        // Raw tee files are best-effort: if they fail we still stream to the
        // session log (passing None disables only the forensic copy).
        let raw = open_game_output_files(&config.data_dir, &instance.name);
        if let Err(e) = &raw {
            tracing::warn!(target: "launcher", "Raw output tee unavailable: {}", e);
        }
        let (raw_out, raw_err) = raw.unwrap_or((None, None));
        crate::game_logs::attach_output_readers(
            child.id(),
            Box::new(out),
            Box::new(err),
            raw_out,
            raw_err,
            session_log_path.map(|s| s.to_string()),
        );
    }

    Ok(child)
}

/// Find all available Java installations (system + managed)
pub fn find_all_java_installations(data_dir: &Path) -> Vec<JavaInstallation> {
    let mut installations = detect_java_installations();
    installations.extend(
        crate::java_download::list_managed_java(data_dir)
            .into_iter()
            .map(|m| JavaInstallation {
                path: m.path,
                version: m.version,
                major_version: m.major_version,
                is_64bit: m.is_64bit,
                vendor: m.vendor,
                arch: m.arch,
            }),
    );
    installations
}

/// Validate a user-configured Java executable (instance or launcher
/// settings) against the major version this Minecraft version needs.
///
/// `probe_java_by_path` is the same validation the Settings page uses: the
/// file must be a real, runnable Java (a broken MSI stub is rejected by the
/// size floor, a non-Java executable by the exit-status/version parse). The
/// older `detect_java_major` helper accepted any spawnable EXE, so a wrong
/// path could slip a bogus "JVM" into the game command line.
fn validate_explicit_java(path: &Path, required_major: u32, source: &str) -> Result<PathBuf> {
    match crate::java::probe_java_by_path(path) {
        None => Err(LauncherError::Java(format!(
            "The Java configured for this {} does not appear to be a working Java installation: {}. Pick the Java executable again in the settings, or set Java to 'Automatic'.",
            source,
            path.display()
        ))),
        Some(inst) if inst.major_version != required_major => Err(LauncherError::Java(format!(
            "The Java configured for this {} is Java {} ({}), but this Minecraft version requires Java {}. Change the Java version in the settings, or set it to 'Automatic' to let the launcher choose.",
            source, inst.major_version, inst.version, required_major
        ))),
        Some(inst) => {
            tracing::info!(target: "launcher", "Using {} Java: {:?} (v{}, {})", source, path, inst.version, inst.vendor);
            Ok(path.to_path_buf())
        }
    }
}

/// Determine which Java executable to use
fn get_java_path(
    config: &AppConfig,
    instance: &Instance,
    version_info: &VersionInfo,
) -> Result<PathBuf> {
    let required_java = version_info.required_java_major();

    // Priority: instance java > config java > auto-detect (system + managed)
    if let Some(path) = &instance.java_path {
        if path.exists() {
            return validate_explicit_java(path, required_java, "instance");
        }
        tracing::debug!(target: "launcher", "Instance Java not found: {:?}", path);
    }

    if let Some(path) = &config.java_path {
        if path.exists() {
            return validate_explicit_java(path, required_java, "launcher settings");
        }
        tracing::debug!(target: "launcher", "Config Java not found: {:?}", path);
    }

    // Auto-detect (system + managed). Only an EXACT 64-bit major match is
    // acceptable — the same rule `ensure_java_for_launch` enforces when it
    // downloads a runtime. "Closest newer" is never safe: modern JDKs abort
    // on obfuscated old clients (unnamed-package classes trigger
    // java.lang.module.FindException during boot-layer scan).
    tracing::info!(target: "launcher", "Auto-detecting Java installations...");
    let installations = find_all_java_installations(&config.data_dir);
    tracing::info!(target: "launcher", "Found {} Java installations", installations.len());
    for (i, inst) in installations.iter().enumerate() {
        tracing::debug!(target: "launcher", "  [{}] {} v{} ({})", i, inst.vendor, inst.version, inst.path.display());
    }

    if installations.is_empty() {
        return Err(LauncherError::Java(
            "No Java installation found. Please install Java.".into(),
        ));
    }

    tracing::info!(target: "launcher", "Required Java version: {}", required_java);

    match installations
        .iter()
        .find(|j| j.is_64bit && j.major_version == required_java)
    {
        Some(java) => {
            tracing::info!(target: "launcher", "Selected Java: {} v{} at {:?}", java.vendor, java.version, java.path);
            Ok(java.path.clone())
        }
        None => Err(LauncherError::Java(format!(
            "No 64-bit Java {} found. Install Java {} or let the launcher download it automatically.",
            required_java, required_java
        ))),
    }
}

/// Resolve a loader-library path from the persisted instance profile.
///
/// Loader metadata decides these relative paths. An unsafe path means the
/// profile was tampered with; fail loudly instead of launching with a
/// silently incomplete (or externally sourced) classpath.
fn loader_library_path(
    libraries_dir: &Path,
    lib: &crate::modloaders::LoaderLibrary,
) -> Result<PathBuf> {
    if crate::import::check_safe_remote_path(&lib.path).is_err() {
        return Err(LauncherError::Launch(format!(
            "Unsafe path in loader library: {} ({})",
            lib.name, lib.path
        )));
    }
    Ok(libraries_dir.join(&lib.path))
}

/// Extract "group:artifact" from a Maven coordinate like
/// "org.ow2.asm:asm:9.8" or "com.google.code.findbugs:jsr305:3.0.2".
fn artifact_key(name: &str) -> Option<String> {
    let mut parts = name.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    if group.is_empty() || artifact.is_empty() {
        return None;
    }
    Some(format!("{}:{}", group, artifact))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loader_lib(path: &str) -> crate::modloaders::LoaderLibrary {
        crate::modloaders::LoaderLibrary {
            name: "com.example:evil:1.0".to_string(),
            url: "https://libraries.minecraft.net/".to_string(),
            path: path.to_string(),
            sha1: None,
            size: None,
        }
    }

    #[test]
    fn loader_library_path_rejects_traversal() {
        let err = loader_library_path(Path::new("libraries"), &loader_lib("../../evil.jar"))
            .expect_err("traversal path must fail launch");
        assert!(err.to_string().contains("Unsafe path"), "got: {}", err);
    }

    #[test]
    fn loader_library_path_keeps_safe_path_under_libraries() {
        let path = loader_library_path(
            Path::new("libraries"),
            &loader_lib("com/example/evil/1.0/evil-1.0.jar"),
        )
        .expect("safe path must resolve");
        assert!(path.starts_with("libraries"));
    }

    /// In-memory FS probe: proves the profile builder never touches disk.
    struct FakeLaunchFs {
        files: std::collections::HashSet<PathBuf>,
        srg: bool,
    }

    impl crate::versions::ClasspathProbe for FakeLaunchFs {
        fn is_file(&self, path: &Path) -> bool {
            self.files.contains(path)
        }
    }

    impl LaunchFs for FakeLaunchFs {
        fn exists(&self, path: &Path) -> bool {
            self.files.contains(path)
        }

        fn has_srg_jar(&self, _libraries_dir: &Path, _mc_version: &str) -> bool {
            self.srg
        }
    }

    fn synthetic_version(id: &str) -> VersionInfo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "type": "release",
            "mainClass": "synthetic.Main",
            "arguments": {
                "game": ["--username", "${auth_player_name}"],
                "jvm": ["-cp", "${classpath}"]
            },
            "libraries": [],
            "downloads": { "client": { "sha1": "", "size": 0, "url": "" } },
            "assetIndex": { "id": "1", "sha1": "", "size": 0, "totalSize": 0, "url": "" },
            "assets": "1"
        }))
        .expect("synthetic version JSON")
    }

    #[test]
    fn profile_builder_uses_only_the_injected_probe() {
        let root = std::env::temp_dir().join(format!(
            "voidlauncher-pure-profile-{}-does-not-exist",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let version = synthetic_version("1.20.1");
        let instance = crate::instances::Instance::new(
            "Pure",
            "1.20.1",
            4096,
            "g1gc",
            crate::instances::LoaderType::Vanilla,
            None,
        );
        let client_jar = root.join("versions/1.20.1/client.jar");
        let entry_jar = root.join("versions/1.20.1/entry/VoidLauncherEntry.jar");
        let files = [client_jar.clone(), entry_jar.clone()]
            .into_iter()
            .collect();
        let fs = FakeLaunchFs { files, srg: false };
        let auth = LaunchAuth {
            access_token: "token",
            uuid: "uuid",
            username: "Player",
        };
        let default_jvm_args = vec!["-XX:+DisableExplicitGC".to_string()];
        let versions_dir = root.join("versions");
        let libraries_dir = root.join("libraries");
        let assets_dir = root.join("assets");
        let game_dir = root.join("instances/Pure/.minecraft");
        let request = LaunchProfileRequest {
            instance: &instance,
            version_info: &version,
            auth: &auth,
            server: None,
            session_log_path: None,
            java_major: 17,
            versions_dir: &versions_dir,
            libraries_dir: &libraries_dir,
            assets_dir: &assets_dir,
            game_dir: &game_dir,
            default_memory_mb: 4096,
            default_gc_preset: "g1gc",
            default_jvm_args: &default_jvm_args,
        };

        let profile = build_launch_profile(&request, &fs).expect("profile builds");

        assert!(!root.exists(), "builder must not create the fake data dir");
        assert_eq!(profile.classpath, vec![entry_jar, client_jar]);
        assert_eq!(profile.main_class, "synthetic.Main");
        assert!(profile.traits.entry_wrapper);
        assert_eq!(profile.game_args, vec!["--username", "Player"]);
        assert_eq!(
            profile.jvm_args.first().map(String::as_str),
            Some("-Xms4096M")
        );
    }

    #[test]
    fn profile_builder_reports_missing_client_jar_through_the_probe() {
        let version = synthetic_version("1.20.1");
        let instance = crate::instances::Instance::new(
            "Pure",
            "1.20.1",
            4096,
            "g1gc",
            crate::instances::LoaderType::Vanilla,
            None,
        );
        let fs = FakeLaunchFs {
            files: std::collections::HashSet::new(),
            srg: false,
        };
        let auth = LaunchAuth {
            access_token: "token",
            uuid: "uuid",
            username: "Player",
        };
        let default_jvm_args = Vec::new();
        let root = PathBuf::from("missing");
        let request = LaunchProfileRequest {
            instance: &instance,
            version_info: &version,
            auth: &auth,
            server: None,
            session_log_path: None,
            java_major: 17,
            versions_dir: &root,
            libraries_dir: &root,
            assets_dir: &root,
            game_dir: &root,
            default_memory_mb: 4096,
            default_gc_preset: "g1gc",
            default_jvm_args: &default_jvm_args,
        };

        let err = build_launch_profile(&request, &fs).expect_err("client jar missing");
        assert!(
            err.to_string().contains("Client JAR is missing"),
            "got: {}",
            err
        );
    }
}

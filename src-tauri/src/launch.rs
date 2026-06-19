use crate::error::{LauncherError, Result};
use crate::config::AppConfig;
use crate::instances::Instance;
use crate::versions::{VersionInfo, build_classpath, get_game_arguments, get_jvm_arguments};
use crate::java::{detect_java_installations, get_recommended_java};
use crate::jvm::{build_jvm_args, detect_java_major, strip_gc_selection_flags, GcPreset};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Launch Minecraft for a given instance
pub fn launch_minecraft(
    config: &AppConfig,
    instance: &Instance,
    version_info: &VersionInfo,
    access_token: &str,
    uuid: &str,
    username: &str,
) -> Result<std::process::Child> {
    tracing::info!(target: "launcher", "Starting launch for instance: {}", instance.name);
    tracing::info!(target: "launcher", "MC version: {}", instance.mc_version);
    tracing::info!(target: "launcher", "Username: {}", username);

    // 1. Determine Java path
    tracing::info!(target: "launcher", "Detecting Java installation...");
    let java_path = get_java_path(config, instance, version_info)?;
    tracing::info!(target: "launcher", "Using Java: {:?}", java_path);

    // 2. Probe the selected Java's major version BEFORE composing the command.
    //    This is what lets us safely fall back from ZGC to G1GC for older JDKs.
    let java_major = detect_java_major(&java_path).unwrap_or(0);
    tracing::info!(target: "launcher", "Detected Java major version: {}", java_major);

    // 3. Build classpath
    let client_jar = config
        .versions_dir()
        .join(&version_info.id)
        .join(format!("{}.jar", version_info.id));
    tracing::debug!(target: "launcher", "Client JAR: {:?}", client_jar);
    tracing::debug!(target: "launcher", "Client JAR exists: {}", client_jar.exists());

    let mut classpath = build_classpath(version_info, &config.libraries_dir(), &client_jar);

    // Track already added library paths to avoid duplicates (e.g., gson in both vanilla and NeoForge)
    let mut added_libs: std::collections::HashSet<String> = classpath
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    // Add mod loader libraries to classpath (deduplicated)
    if let Some(profile) = &instance.loader_profile {
        tracing::info!(target: "launcher", "Mod loader: main_class={}", profile.main_class);
        for lib in &profile.libraries {
            let lib_path = config.libraries_dir().join(&lib.path);
            if lib_path.exists() {
                let lib_path_str = lib_path.to_string_lossy().to_string();
                if added_libs.insert(lib_path_str.clone()) {
                    if !classpath.is_empty() {
                        classpath.push(';');
                    }
                    classpath.push_str(&lib_path_str);
                } else {
                    tracing::debug!(target: "launcher", "Skipping duplicate library on classpath: {}", lib.name);
                }
            } else {
                tracing::warn!(target: "launcher", "Mod library not found: {:?}", lib_path);
            }
        }
    }

    // 4. Build JVM arguments.
    //    - Memory: instance override > config default. Xms == Xmx.
    //    - Preset: instance override > "g1gc" (safe default).
    //    - Any custom instance.jvm_args are appended AFTER the preset.
    let memory_mb = instance.memory_mb.unwrap_or(config.default_memory_mb);
    let preset_str = instance.gc_preset.as_deref().unwrap_or("g1gc");
    let requested_preset = GcPreset::from_str(preset_str);
    let (mut args, effective_preset) = build_jvm_args(requested_preset, memory_mb, java_major);
    tracing::info!(target: "launcher", "Memory: Xms=Xmx={}M, preset={:?} (requested {:?})",
              memory_mb, effective_preset, requested_preset);

    // Append user-provided custom args (for power users). These go AFTER the
    // preset so they can override anything the preset decided. Instance-level
    // custom args take priority; otherwise we fall back to the global default.
    //
    // Both sources are stripped of GC-selection flags: the user's chosen
    // preset in `default_gc_preset` is the single source of truth for which
    // GC the JVM starts with. This guards against old `config.json` files
    // that were written before the strip logic existed.
    if let Some(custom) = &instance.jvm_args {
        let stripped = strip_gc_selection_flags(custom);
        for a in stripped { args.push(a); }
    } else {
        let stripped = strip_gc_selection_flags(&config.default_jvm_args);
        for a in stripped { args.push(a); }
    }

    let natives_dir = config
        .versions_dir()
        .join(&version_info.id)
        .join("natives");
    std::fs::create_dir_all(&natives_dir)?;

    // Add mod loader JVM args (skip -cp and ${classpath}, we add them explicitly below).
    // Loader profiles (Forge/NeoForge in particular) sometimes include their own
    // GC selector, so we strip those here too — the user's chosen preset wins.
    let library_dir = config.libraries_dir();
    let classpath_sep = ";";
    let version_name = &version_info.id;
    if let Some(profile) = &instance.loader_profile {
        let loader_args = strip_gc_selection_flags(&profile.jvm_args);
        for loader_arg in &loader_args {
            if loader_arg == "-cp" || loader_arg == "${classpath}" {
                continue;
            }
            let processed = loader_arg
                .replace("${natives_directory}", &natives_dir.to_string_lossy())
                .replace("${library_directory}", &library_dir.to_string_lossy())
                .replace("${classpath_separator}", classpath_sep)
                .replace("${version_name}", version_name)
                .replace("${launcher_name}", "VoidLauncher")
                .replace("${launcher_version}", LAUNCHER_VERSION);
            args.push(processed);
        }
    }

    // JVM arguments from version manifest (skip -cp and ${classpath}).
    //
    // IMPORTANT: Mojang's 1.20.5+ manifests ship `-XX:+UseG1GC` as the
    // default GC selector. If the user picked a different preset
    // (ZGC, Standard, …) we MUST drop every GC-selection flag from the
    // upstream args first — otherwise HotSpot aborts with
    // "multiple garbage collectors selected" before the game can start.
    // See `jvm::strip_gc_selection_flags` for the full list.
    let version_jvm_args = get_jvm_arguments(version_info);
    tracing::debug!(target: "launcher", "Version manifest JVM args ({} total):", version_jvm_args.len());
    for a in &version_jvm_args { tracing::debug!(target: "launcher", "  raw: {}", a); }
    let version_jvm_args = strip_gc_selection_flags(&version_jvm_args);
    tracing::debug!(target: "launcher", "After GC-strip ({} remaining):", version_jvm_args.len());
    for a in &version_jvm_args { tracing::debug!(target: "launcher", "  kept: {}", a); }
    for arg in &version_jvm_args {
        if arg == "-cp" || arg == "${classpath}" {
            continue;
        }
        let processed = arg
            .replace("${natives_directory}", &natives_dir.to_string_lossy())
            .replace("${library_directory}", &library_dir.to_string_lossy())
            .replace("${classpath_separator}", classpath_sep)
            .replace("${version_name}", version_name)
            .replace("${launcher_name}", "VoidLauncher")
            .replace("${launcher_version}", LAUNCHER_VERSION);
        args.push(processed);
    }

    // Add classpath
    args.push("-cp".to_string());
    args.push(classpath.clone());

    // NeoForge's -p module path only includes NeoForge-specific jars, but
    // JiJ dependencies (e.g. endec from owo-lib) may require other modules
    // like fastutil that are on the classpath but not the module path.
    // Fix: append all classpath entries (except native JARs) to the -p value
    // so the module system can resolve all module dependencies.
    if instance.loader != crate::instances::LoaderType::Vanilla {
        if let Some(p_idx) = args.iter().position(|a| a == "-p") {
            let cp_entries: Vec<&str> = classpath.split(';').collect();
            if let Some(module_path) = args.get_mut(p_idx + 1) {
                for entry in &cp_entries {
                    if entry.is_empty() || module_path.contains(entry) {
                        continue;
                    }
                    // Skip native JARs (e.g. lwjgl-freetype-3.3.3-natives-linux.jar)
                    // which would cause "Module named X was already on the JVMs module path"
                    let fname = std::path::Path::new(entry)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    let is_native = fname.contains("-natives-linux")
                        || fname.contains("-natives-macos")
                        || fname.contains("-natives-windows")
                        || fname.contains("-natives-");
                    if is_native {
                        continue;
                    }
                    module_path.push_str(classpath_sep);
                    module_path.push_str(entry);
                }
            }
        }
    }

    // Main class (use loader profile if available)
    let main_class = instance
        .loader_profile
        .as_ref()
        .map(|p| p.main_class.clone())
        .unwrap_or_else(|| version_info.main_class.clone());
    args.push(main_class.clone());
    tracing::info!(target: "launcher", "Main class: {}", main_class);

    // 5. Build game arguments
    let game_dir = instance.minecraft_dir(&config.instances_dir());
    std::fs::create_dir_all(&game_dir)?;

    let assets_dir = config.assets_dir();
    let game_args = get_game_arguments(version_info);

    for arg in &game_args {
        let processed = arg
            .replace("${auth_player_name}", username)
            .replace("${version_name}", &version_info.id)
            .replace("${game_directory}", &game_dir.to_string_lossy())
            .replace("${assets_root}", &assets_dir.to_string_lossy())
            .replace("${assets_index_name}", &version_info.assets)
            .replace("${auth_uuid}", uuid)
            .replace("${auth_access_token}", access_token)
            .replace("${user_type}", "msa")
            .replace("${version_type}", &version_info.version_type)
            .replace("${auth_xuid}", "0")
            .replace("${clientid}", "")
            .replace("${user_properties}", "{}")
            .replace("${resolution_width}", "1280")
            .replace("${resolution_height}", "720");

        args.push(processed);
    }

    // Add mod loader game arguments
    if let Some(profile) = &instance.loader_profile {
        for loader_arg in &profile.game_args {
            let processed = loader_arg
                .replace("${auth_player_name}", username)
                .replace("${version_name}", &version_info.id)
                .replace("${game_directory}", &game_dir.to_string_lossy())
                .replace("${assets_root}", &assets_dir.to_string_lossy())
                .replace("${assets_index_name}", &version_info.assets)
                .replace("${auth_uuid}", uuid)
                .replace("${auth_access_token}", access_token)
                .replace("${user_type}", "msa")
                .replace("${version_type}", &version_info.version_type)
                .replace("${auth_xuid}", "0")
                .replace("${clientid}", "")
                .replace("${user_properties}", "{}")
                .replace("${resolution_width}", "1280")
                .replace("${resolution_height}", "720");
            args.push(processed);
        }
    }

    // Add resolution if specified
    if let Some(res) = &instance.resolution {
        args.push("--width".to_string());
        args.push(res.width.to_string());
        args.push("--height".to_string());
        args.push(res.height.to_string());
    }

    // 6. Launch
    tracing::info!(target: "launcher", "Final args count: {}", args.len());
    tracing::debug!(target: "launcher", "Game dir: {:?}", game_dir);
    tracing::debug!(target: "launcher", "argv:");
    let sanitized: Vec<String> = args.iter().enumerate().map(|(i, a)| {
        if a == "--accessToken" || a == "--authAccessToken" {
            args.get(i + 1).map(|_| {
                format!("{} ***", a)
            }).unwrap_or_else(|| a.clone())
        } else if i > 0 && (args[i - 1] == "--accessToken" || args[i - 1] == "--authAccessToken") {
            "***".to_string()
        } else {
            a.clone()
        }
    }).collect::<Vec<_>>();
    for (i, a) in sanitized.iter().enumerate() {
        tracing::debug!(target: "launcher", "  [{}] {}", i, a);
    }
    tracing::info!(target: "launcher", "Spawning Java process...");

    let mut cmd = Command::new(&java_path);
    cmd.args(&args)
        .current_dir(&game_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd
        .spawn()
        .map_err(|e| {
            let msg = if e.raw_os_error() == Some(740) {
                format!("Java requires administrator privileges. Right-click java.exe → Properties → Compatibility → disable 'Run as admin'. Java path: {}", java_path.display())
            } else {
                format!("Failed to launch: {}", e)
            };
            tracing::error!(target: "launcher", "FAILED to spawn Java: {}", msg);
            LauncherError::Launch(msg)
        })?;

    tracing::info!(target: "launcher", "Java process spawned with PID: {}", child.id());
    Ok(child)
}

/// Determine which Java executable to use
fn get_java_path(
    config: &AppConfig,
    instance: &Instance,
    version_info: &VersionInfo,
) -> Result<PathBuf> {
    // Priority: instance java > config java > auto-detect
    if let Some(path) = &instance.java_path {
        if path.exists() {
            tracing::info!(target: "launcher", "Using instance Java: {:?}", path);
            return Ok(path.clone());
        }
        tracing::debug!(target: "launcher", "Instance Java not found: {:?}", path);
    }

    if let Some(path) = &config.java_path {
        if path.exists() {
            tracing::info!(target: "launcher", "Using config Java: {:?}", path);
            return Ok(path.clone());
        }
        tracing::debug!(target: "launcher", "Config Java not found: {:?}", path);
    }

    // Auto-detect
    tracing::info!(target: "launcher", "Auto-detecting Java installations...");
    let installations = detect_java_installations();
    tracing::info!(target: "launcher", "Found {} Java installations", installations.len());
    for (i, inst) in installations.iter().enumerate() {
        tracing::debug!(target: "launcher", "  [{}] {} v{} ({})", i, inst.vendor, inst.version, inst.path.display());
    }

    if installations.is_empty() {
        return Err(LauncherError::Java(
            "No Java installation found. Please install Java.".into(),
        ));
    }

    let required_java = version_info
        .java_version
        .as_ref()
        .map(|v| v.major_version);
    tracing::info!(target: "launcher", "Required Java version: {}+", required_java.unwrap_or(21));

    match get_recommended_java(required_java, &installations) {
        Some(java) => {
            tracing::info!(target: "launcher", "Selected Java: {} v{} at {:?}", java.vendor, java.version, java.path);
            Ok(java.path)
        }
        None => Err(LauncherError::Java(format!(
            "No suitable Java found. Required: Java {}+",
            required_java.unwrap_or(21)
        ))),
    }
}

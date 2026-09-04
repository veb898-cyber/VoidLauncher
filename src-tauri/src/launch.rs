use crate::error::{LauncherError, Result};
use crate::config::AppConfig;
use crate::instances::Instance;
use crate::versions::{VersionInfo, build_classpath, get_game_arguments, get_jvm_arguments};
use crate::java::{detect_java_installations, get_recommended_java, JavaInstallation};
use crate::jvm::{build_jvm_args, detect_java_major, strip_gc_selection_flags, GcPreset};
use std::path::PathBuf;
use std::process::{Command, Stdio};
const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

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
    std::fs::create_dir_all(&game_logs_dir).map_err(|e| {
        LauncherError::Launch(format!("Failed to create game logs dir: {}", e))
    })?;

    let now = chrono::Local::now();
    let timestamp = now.format("%Y%m%d_%H%M%S");
    let safe_name = crate::game_logs::sanitize_instance_name(instance_name);
    let base = game_logs_dir.join(format!("{}_{}", safe_name, timestamp));
    let stdout_path = format!("{}.stdout.log", base.display());
    let stderr_path = format!("{}.stderr.log", base.display());

    let open_raw = |path: &str, label: &str| -> Option<std::fs::File> {
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
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
        now.format("%Y-%m-%d %H:%M:%S"),
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
        .join("client.jar");
    // Fallback: migrate old {version}.jar to client.jar
    if !client_jar.exists() {
        let old_jar = config
            .versions_dir()
            .join(&version_info.id)
            .join(format!("{}.jar", version_info.id));
        if old_jar.exists() {
            std::fs::rename(&old_jar, &client_jar).ok();
        }
    }
    tracing::debug!(target: "launcher", "Client JAR: {:?}", client_jar);
    tracing::debug!(target: "launcher", "Client JAR exists: {}", client_jar.exists());

    // Build vanilla classpath first, then we may reorder below
    let vanilla_cp = build_classpath(version_info, &config.libraries_dir(), &client_jar);

    // Collect loader libraries (if any) before inserting into final classpath
    let mut loader_cp_entries: Vec<String> = Vec::new();
    if let Some(profile) = &instance.loader_profile {
        tracing::info!(target: "launcher", "Mod loader: main_class={}", profile.main_class);
        for lib in &profile.libraries {
            let lib_path = config.libraries_dir().join(&lib.path);
            if lib_path.exists() {
                loader_cp_entries.push(lib_path.to_string_lossy().to_string());
            } else {
                tracing::warn!(target: "launcher", "Mod library not found: {:?}", lib_path);
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
    // actually exists on disk (loader wins, as before — dedup by path
    // couldn't handle the same artifact under different versions).
    let mut skip_vanilla_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(profile) = &instance.loader_profile {
        let loader_artifacts: std::collections::HashSet<String> = profile
            .libraries
            .iter()
            .filter(|lib| config.libraries_dir().join(&lib.path).exists())
            .filter_map(|lib| artifact_key(&lib.name))
            .collect();
        if !loader_artifacts.is_empty() {
            for lib in &version_info.libraries {
                if !crate::versions::should_include_library(lib) {
                    continue;
                }
                let Some(key) = artifact_key(&lib.name) else { continue };
                if loader_artifacts.contains(&key) {
                    if let Some(artifact) = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref()) {
                        let p = config.libraries_dir().join(&artifact.path);
                        if p.exists() {
                            skip_vanilla_paths.insert(p.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }

    // For Forge/NeoForge, loader libraries MUST come first on classpath so
    // their Log4j version (with the API that ModLauncher expects) takes
    // priority over the vanilla one. Build a deduplicated classpath string.
    let mut classpath = String::new();
    let mut added_libs: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Add loader libraries first (highest priority)
    for entry in &loader_cp_entries {
        if added_libs.insert(entry.clone()) {
            if !classpath.is_empty() { classpath.push(';'); }
            classpath.push_str(entry);
        }
    }

    // Add vanilla libraries second (lower priority; duplicates already in set are skipped)
    for entry in vanilla_cp.split(';').filter(|s| !s.is_empty()) {
        if skip_vanilla_paths.contains(entry) {
            tracing::debug!(target: "launcher", "Skipping vanilla library overridden by loader: {}", entry);
            continue;
        }
        if added_libs.insert(entry.to_string()) {
            if !classpath.is_empty() { classpath.push(';'); }
            classpath.push_str(entry);
        }
    }

    // For Forge/NeoForge instances, remove vanilla client.jar to avoid JPMS
    // split-package conflict with processor-generated JARs (client-*-srg.jar,
    // forge-*-client.jar). The processor JARs contain the same packages and
    // replace client.jar. Since processor JARs are NOT in the loader profile's
    // library list (they're discovered by MinecraftLocator / the production
    // client provider at runtime), we check the filesystem for them.
    if matches!(instance.loader, crate::instances::LoaderType::Forge | crate::instances::LoaderType::NeoForge) {
        let client_jar_str = client_jar.to_string_lossy().to_string();
        // Check if THIS version's processor-generated SRG JAR exists
        // (libraries/net/minecraft/client/<mcp-version>/client-<mcp-version>-srg.jar).
        // The check MUST be scoped to the instance's MC version: old Forge
        // (e.g. 1.8.9) has no processor JARs and still needs client.jar on the
        // classpath, while a global scan would see SRG JARs from other
        // versions (1.14+/1.19+) and wrongly strip client.jar from old ones.
        let client_base = config.libraries_dir().join("net").join("minecraft").join("client");
        let prefix = format!("client-{}-", instance.mc_version);
        let has_srg = if client_base.is_dir() {
            std::fs::read_dir(&client_base)
                .ok()
                .map(|entries| {
                    entries.flatten().any(|e| {
                        let path = e.path();
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
                            let name = e.file_name().to_string_lossy().to_string();
                            name.starts_with(&prefix) && name.ends_with("-srg.jar")
                        }
                    })
                })
                .unwrap_or(false)
        } else {
            false
        };

        if has_srg {
            // Remove the client.jar entry from classpath
            let old_len = classpath.len();
            if classpath.starts_with(&client_jar_str) {
                let after = classpath.trim_start_matches(&client_jar_str);
                if after.starts_with(';') {
                    classpath = after[1..].to_string();
                } else {
                    classpath = after.to_string();
                }
            } else {
                classpath = classpath.replace(&format!("{};", client_jar_str), "");
                classpath = classpath.replace(&format!(";{}", client_jar_str), "");
                classpath = classpath.trim_end_matches(&client_jar_str).to_string();
            }
            if classpath.len() < old_len {
                added_libs.remove(&client_jar_str);
                tracing::debug!(target: "launcher", "Removed vanilla client.jar for Forge (replaced by srg/extra JARs)");
            }
        }
    }

    // Write VoidLauncherEntry.jar to a temp dir and prepend it to the
    // classpath.  The JVM tries to derive an automatic module from the JAR
    // that contains the main class; by using an entry point that lives
    // OUTSIDE the obfuscated client.jar we avoid
    //   java.lang.module.InvalidModuleDescriptorException
    // caused by Mojang's classes in the unnamed (default) package.
    let entry_dir = config
        .versions_dir()
        .join(&version_info.id)
        .join("entry");
    std::fs::create_dir_all(&entry_dir).ok();
    let entry_jar = entry_dir.join("VoidLauncherEntry.jar");
    // Always overwrite to ensure embedded bytes match (old JAR from previous
    // builds may contain a class without the voidlauncher.entry package).
    let _ = std::fs::write(&entry_jar, crate::entry::JAR_BYTES);
    // Remove old .class file from previous versions (was in default package,
    // didn't work with package-based main class name).
    let _ = std::fs::remove_file(entry_dir.join("VoidLauncherEntry.class"));
    let entry_path = entry_jar.to_string_lossy().to_string();
    classpath = format!("{};{}", entry_path, classpath);

    // 4. Build JVM arguments.
    //    - Memory: instance override > config default. Xms == Xmx.
    //    - Preset: instance override > config default_gc_preset (safe default).
    //    - Any custom instance.jvm_args are appended AFTER the preset.
    let memory_mb = instance.memory_mb.unwrap_or(config.default_memory_mb);
    let preset_str = instance
        .gc_preset
        .as_deref()
        .unwrap_or(&config.default_gc_preset);
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

    // Extract native libraries (DLLs) from native classifier JARs into the
    // natives directory, so old LWJGL versions can find them via
    // -Djava.library.path. The native JARs are also on the classpath (added
    // by build_classpath), which covers newer LWJGL.
    crate::versions::extract_natives(version_info, &config.libraries_dir(), &natives_dir);

    // Add mod loader JVM args (skip -cp and ${classpath}, we add them explicitly below).
    // Loader profiles (Forge/NeoForge in particular) sometimes include their own
    // GC selector, so we strip those here too — the user's chosen preset wins.
    let library_dir = config.libraries_dir();
    let classpath_sep = ";";
    let version_name = &version_info.id;
    if let Some(profile) = &instance.loader_profile {
        let loader_args = strip_gc_selection_flags(&profile.jvm_args);
        let mut i = 0;
        while i < loader_args.len() {
            let loader_arg = &loader_args[i];
            if loader_arg == "-cp" || loader_arg == "${classpath}" {
                i += 1;
                continue;
            }
            let processed = loader_arg
                .replace("${natives_directory}", &natives_dir.to_string_lossy())
                .replace("${library_directory}", &library_dir.to_string_lossy())
                .replace("${classpath_separator}", classpath_sep)
                .replace("${version_name}", version_name)
                .replace("${launcher_name}", "VoidLauncher")
                .replace("${launcher_version}", LAUNCHER_VERSION);

            // Diagnostics: log -p value at INFO and check module JARs exist
            if loader_arg == "-p" && i + 1 < loader_args.len() {
                let raw_modpath = &loader_args[i + 1];
                let modpath = raw_modpath
                    .replace("${library_directory}", &library_dir.to_string_lossy())
                    .replace("${classpath_separator}", classpath_sep);
                tracing::info!(target: "launcher", "Module path (-p): {}", modpath);
                for (j, entry) in modpath.split(';').enumerate() {
                    let p = std::path::Path::new(entry);
                    if p.exists() {
                        tracing::info!(target: "launcher", "  [{}] EXISTS: {}", j, entry);
                    } else {
                        tracing::warn!(target: "launcher", "  [{}] MISSING: {}", j, entry);
                    }
                }
            }

            args.push(processed);
            i += 1;
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

    // Remove obsolete flag that crashes the JVM (never existed in any Java
    // version — old Forge profiles shipped it incorrectly).
    args.retain(|a| a != "-XX:+G1UnlockCommercialFeatures");

    // Add classpath
    args.push("-cp".to_string());
    args.push(classpath.clone());

    // NOTE: PrismLauncher does NOT add classpath entries to -p (module path).
    // They use -cp for everything.  Adding all classpath entries to -p causes
    // .zip files (e.g. mcp_config) and obfuscated JARs (client.jar) to fail
    // with java.lang.module.FindException.  The loader profile's own -p
    // (if present) already contains the necessary module JARs for Forge/NeoForge.
    // We intentionally do NOT augment the module path here.

    // Main class selection:
    //   - Vanilla: use VoidLauncherEntry wrapper to avoid module-system
    //     derivation errors (client.jar has classes in unnamed package).
    //   - Forge/NeoForge/Fabric/Quilt: use the loader profile's main class
    //     directly. Its -p (module path) contains the bootstrap JARs and the
    //     real main class lives there — Class.forName from our entry point
    //     would NOT find it because it only searches the classpath.
    let real_main = instance
        .loader_profile
        .as_ref()
        .map(|p| p.main_class.clone())
        .unwrap_or_else(|| version_info.main_class.clone());
    if instance.loader_profile.is_some() {
        // Mod loader — main class is on the module path, use it directly
        let main_class = real_main.clone();
        args.push(main_class.clone());
        tracing::info!(target: "launcher", "Main class: {} (loader)", main_class);
    } else {
        // Vanilla — use VoidLauncherEntry wrapper, pass real main as sysprop
        args.push(format!("-Dvoidlauncher.mainClass={}", real_main));
        let main_class = "voidlauncher.entry.VoidLauncherEntry".to_string();
        args.push(main_class.clone());
        tracing::info!(target: "launcher", "Main class: {} (delegating to {})", main_class, real_main);
    }

    // 5. Build game arguments
    let game_dir = instance.minecraft_dir(&config.instances_dir());
    std::fs::create_dir_all(&game_dir)?;

    let assets_dir = config.assets_dir();

    // 1.6.x and older read flat files straight from --assetsDir, so the
    // launcher points them at the mirrored virtual/legacy tree.
    let game_assets_dir = if version_info.assets == "legacy" {
        assets_dir.join("virtual").join("legacy")
    } else {
        assets_dir.clone()
    };

    // Old session format (1.6.x): --session <token>:<uuid>:<username>.
    // "0" is the conventional offline token.
    let auth_session = format!("0:{}:{}", uuid, username);

    // For legacy loaders (MC <= 1.12.2 Forge), the loader
    // profile carries the FULL game argument list in its
    // `minecraftArguments` string — those REPLACE the vanilla args.
    let legacy_full_args = instance
        .loader_profile
        .as_ref()
        .map(|p| p.legacy_args && !p.game_args.is_empty())
        .unwrap_or(false);

    // Resolution: when the instance sets it, the manifest's
    // `has_custom_resolution` feature rule emits --width/--height for
    // versions with structured arguments; for legacy versions the
    // manifest has no resolution placeholders, so add them here —
    // but only if the manifest did not already provide them.
    let (res_w, res_h) = match &instance.resolution {
        Some(res) => (res.width.to_string(), res.height.to_string()),
        None => ("1280".to_string(), "720".to_string()),
    };

    let substitute_args = |arg: &str| -> String {
        arg
            .replace("${auth_player_name}", username)
            .replace("${version_name}", &version_info.id)
            .replace("${game_directory}", &game_dir.to_string_lossy())
            .replace("${assets_root}", &assets_dir.to_string_lossy())
            .replace("${game_assets}", &game_assets_dir.to_string_lossy())
            .replace("${assets_index_name}", &version_info.assets)
            .replace("${auth_uuid}", uuid)
            .replace("${auth_access_token}", access_token)
            .replace("${auth_session}", &auth_session)
            .replace("${user_type}", "msa")
            .replace("${version_type}", &version_info.version_type)
            .replace("${auth_xuid}", "0")
            .replace("${clientid}", "")
            .replace("${user_properties}", "{}")
            .replace("${resolution_width}", &res_w)
            .replace("${resolution_height}", &res_h)
    };

    if !legacy_full_args {
        let game_args = get_game_arguments(version_info, instance.resolution.is_some());
        for arg in &game_args {
            args.push(substitute_args(arg));
        }
    } else {
        tracing::info!(target: "launcher", "Using legacy loader game arguments (replaces vanilla)");
    }

    // Add mod loader game arguments
    if let Some(profile) = &instance.loader_profile {
        for loader_arg in &profile.game_args {
            args.push(substitute_args(loader_arg));
        }
    }

    // Add resolution if specified AND not already provided by the manifest
    // (feature rule has_custom_resolution above)
    if let Some(res) = &instance.resolution {
        let has_width = args.iter().any(|a| a == "--width");
        let has_height = args.iter().any(|a| a == "--height");
        if !has_width {
            args.push("--width".to_string());
            args.push(res.width.to_string());
        }
        if !has_height {
            args.push("--height".to_string());
            args.push(res.height.to_string());
        }
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
        tracing::info!(target: "launcher", "  [{}] {}", i, a);
    }
    tracing::info!(target: "launcher", "Spawning Java process...");

    let mut cmd = Command::new(&java_path);
    cmd.args(&args)
        .current_dir(&game_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
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
        );
    }

    Ok(child)
}

/// Find all available Java installations (system + managed)
pub fn find_all_java_installations(data_dir: &PathBuf) -> Vec<JavaInstallation> {
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
            }),
    );
    installations
}

/// Determine which Java executable to use
fn get_java_path(
    config: &AppConfig,
    instance: &Instance,
    version_info: &VersionInfo,
) -> Result<PathBuf> {
    // Priority: instance java > config java > auto-detect (system + managed)
    if let Some(path) = &instance.java_path {
        if path.exists() {
            // A user-supplied path must actually be a runnable Java — never
            // let a non-Java executable be launched. `detect_java_major` runs
            // `java -version` once, which is exactly the on-launch check we
            // want; the result is not cached unnecessarily because launch is
            // a one-shot, per-instance operation.
            if crate::jvm::detect_java_major(path).is_none() {
                return Err(LauncherError::Java(format!(
                    "The configured Java path does not appear to be a runnable Java: {:?}",
                    path
                )));
            }
            tracing::info!(target: "launcher", "Using instance Java: {:?}", path);
            return Ok(path.clone());
        }
        tracing::debug!(target: "launcher", "Instance Java not found: {:?}", path);
    }

    if let Some(path) = &config.java_path {
        if path.exists() {
            if crate::jvm::detect_java_major(path).is_none() {
                return Err(LauncherError::Java(format!(
                    "The configured Java path does not appear to be a runnable Java: {:?}",
                    path
                )));
            }
            tracing::info!(target: "launcher", "Using config Java: {:?}", path);
            return Ok(path.clone());
        }
        tracing::debug!(target: "launcher", "Config Java not found: {:?}", path);
    }

    // Auto-detect (system + managed)
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

    let required_java = version_info.required_java_major();
    tracing::info!(target: "launcher", "Required Java version: {}+", required_java);

    match get_recommended_java(Some(required_java), &installations) {
        Some(java) => {
            tracing::info!(target: "launcher", "Selected Java: {} v{} at {:?}", java.vendor, java.version, java.path);
            Ok(java.path)
        }
        None => Err(LauncherError::Java(format!(
            "No suitable Java found. Required: Java {}+",
            required_java
        ))),
    }
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

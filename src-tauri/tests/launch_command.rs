//! Behavioural tests for the Java command line composed for a launch.
//!
//! These tests are the correctness oracle: every expectation below is derived
//! from the real Mojang metadata committed in `tests/fixtures/versions/` and
//! from the documented loader-profile contract — never from "whatever the code
//! currently prints". The golden files in `tests/fixtures/golden/` are only a
//! change-detection net on top of that.
//!
//! The suite is Windows-only because both Mojang OS rules and native
//! classifiers select different arguments per platform; the launcher itself
//! ships for Windows.

#![cfg(target_os = "windows")]

mod support;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use support::*;
use voidlauncher_lib::instances::LoaderType;
use voidlauncher_lib::launch::{build_launch_command, LaunchCommand};
use voidlauncher_lib::modloaders::LoaderProfile;

/// G1GC preset flags emitted by `jvm::build_jvm_args` for every supported JDK.
const G1_FLAGS: &[&str] = &[
    "-XX:+UnlockExperimentalVMOptions",
    "-XX:+UseG1GC",
    "-XX:+ParallelRefProcEnabled",
    "-XX:MaxGCPauseMillis=50",
    "-XX:G1NewSizePercent=20",
    "-XX:G1ReservePercent=20",
    "-XX:G1HeapRegionSize=32M",
    "-XX:-UseAdaptiveSizePolicy",
    "-XX:-OmitStackTraceInFastThrow",
    "-XX:-DontCompileHugeMethods",
    "-Xss4M",
];

/// The nine module openings the launcher adds on Java 9+ (1.17+ requirement).
fn add_opens() -> Vec<&'static str> {
    vec![
        "--add-opens",
        "java.base/java.lang=ALL-UNNAMED",
        "--add-opens",
        "java.base/java.math=ALL-UNNAMED",
        "--add-opens",
        "java.base/java.util=ALL-UNNAMED",
        "--add-opens",
        "java.base/java.util.concurrent=ALL-UNNAMED",
        "--add-opens",
        "java.base/java.net=ALL-UNNAMED",
        "--add-opens",
        "java.base/java.text=ALL-UNNAMED",
        "--add-opens",
        "java.base/java.lang.reflect=ALL-UNNAMED",
        "--add-opens",
        "java.base/java.io=ALL-UNNAMED",
        "--add-opens",
        "java.desktop/java.awt=ALL-UNNAMED",
    ]
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Replace the (huge, separately asserted) classpath with a marker so the rest
/// of the argv can be compared exactly.
fn mask_classpath(command: &LaunchCommand) -> (Vec<String>, String) {
    let index = index_of(&command.args, "-cp");
    let classpath = command.args[index + 1].clone();
    let mut masked = command.args.clone();
    masked[index + 1] = "<CLASSPATH>".to_string();
    (masked, classpath)
}

// ---------------------------------------------------------------------------
// Exact argv
// ---------------------------------------------------------------------------

#[test]
fn vanilla_1_20_1_composes_expected_argv() {
    let fixture = Fixture::new("vanilla-1.20.1", "1.20.1");
    let command = fixture.build();
    let (masked, _) = mask_classpath(&command);
    let actual = normalize(&masked, fixture.data_dir());

    let game_dir = "<DATA>/instances/Test/.minecraft".to_string();
    let natives = "<DATA>/versions/1.20.1/natives";

    let mut expected: Vec<String> = vec!["-Xms4096M".into(), "-Xmx4096M".into()];
    expected.extend(strings(&add_opens()));
    expected.extend(strings(G1_FLAGS));
    expected.extend(strings(&[
        "-XX:+DisableExplicitGC",
        // From the 1.20.1 manifest: windows-only rule.
        "-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump",
        "-Djava.library.path=<NATIVES>",
        "-Djna.tmpdir=<NATIVES>",
        "-Dorg.lwjgl.system.SharedLibraryExtractPath=<NATIVES>",
        "-Dio.netty.native.workdir=<NATIVES>",
        "-Dminecraft.launcher.brand=VoidLauncher",
        "-Dminecraft.launcher.version=<VERSION>",
        "-cp",
        "<CLASSPATH>",
        // Vanilla launches through the entry JAR to keep obfuscated classes
        // out of the module layer.
        "-Dvoidlauncher.mainClass=net.minecraft.client.main.Main",
        "voidlauncher.entry.VoidLauncherEntry",
        // Manifest game arguments, substituted.
        "--username",
        "TestPlayer",
        "--version",
        "1.20.1",
        "--gameDir",
        "<GAME_DIR>",
        "--assetsDir",
        "<DATA>/assets",
        "--assetIndex",
        "5",
        "--uuid",
        TEST_UUID,
        "--accessToken",
        TEST_TOKEN,
        "--clientId",
        "",
        "--xuid",
        "0",
        "--userType",
        "msa",
        "--versionType",
        "release",
    ]));
    // Keep the readable markers above, then substitute the concrete data paths.
    for arg in &mut expected {
        *arg = arg
            .replace("<NATIVES>", natives)
            .replace("<GAME_DIR>", &game_dir);
    }

    assert_eq!(
        actual, expected,
        "1.20.1 argv changed; see tests/fixtures/SOURCES.md for the metadata"
    );
    assert_no_placeholders(&command.args);
}

#[test]
fn vanilla_1_12_2_uses_legacy_minecraft_arguments() {
    let fixture = Fixture::new("vanilla-1.12.2", "1.12.2");
    let command = fixture.build();
    let (masked, _) = mask_classpath(&command);
    let actual = normalize(&masked, fixture.data_dir());

    let mut expected: Vec<String> = vec!["-Xms4096M".into(), "-Xmx4096M".into()];
    expected.extend(strings(G1_FLAGS));
    expected.extend(strings(&[
        "-XX:+DisableExplicitGC",
        "-Djava.library.path=<DATA>/versions/1.12.2/natives",
        "-Dminecraft.launcher.brand=VoidLauncher",
        "-Dminecraft.launcher.version=<VERSION>",
        "-cp",
        "<CLASSPATH>",
        "-Dvoidlauncher.mainClass=net.minecraft.client.main.Main",
        "voidlauncher.entry.VoidLauncherEntry",
        // Legacy manifests store the whole game argument list as one string.
        "--username",
        "TestPlayer",
        "--version",
        "1.12.2",
        "--gameDir",
        "<DATA>/instances/Test/.minecraft",
        "--assetsDir",
        "<DATA>/assets",
        "--assetIndex",
        "1.12",
        "--uuid",
        TEST_UUID,
        "--accessToken",
        TEST_TOKEN,
        "--userType",
        "msa",
        "--versionType",
        "release",
    ]));

    assert_eq!(actual, expected, "1.12.2 argv changed");
    assert_no_placeholders(&command.args);
}

#[test]
fn vanilla_26_1_2_omits_removed_user_type_flag() {
    let fixture = Fixture::new("vanilla-26.1.2", "26.1.2");
    let command = fixture.build();

    // 26.x dropped `--userType` from the game arguments; passing it would make
    // the client reject the launch. Everything else is still substituted.
    assert!(
        !command.args.iter().any(|a| a == "--userType"),
        "26.1.2 must not receive --userType:\n{:#?}",
        command.args
    );
    assert_eq!(value_after(&command.args, "--assetIndex"), "30");
    assert_eq!(value_after(&command.args, "--versionType"), "release");
    assert_eq!(value_after(&command.args, "--clientId"), "");
    assert_eq!(value_after(&command.args, "--xuid"), "0");

    // Java 25 needs the new module/access flags from its manifest.
    assert!(command
        .args
        .iter()
        .any(|a| a == "--sun-misc-unsafe-memory-access=allow"));
    assert!(command
        .args
        .iter()
        .any(|a| a == "--enable-native-access=ALL-UNNAMED"));
    assert_no_placeholders(&command.args);
}

// ---------------------------------------------------------------------------
// Mojang argument rules
// ---------------------------------------------------------------------------

#[test]
fn manifest_os_rules_only_emit_windows_arguments() {
    let fixture = Fixture::new("rules-1.20.1", "1.20.1");
    let command = fixture.build();

    // windows-only manifest rule
    assert!(command.args.iter().any(|a| a.starts_with(
        "-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe"
    )));
    // osx-only rule
    assert!(!command.args.iter().any(|a| a == "-XstartOnFirstThread"));
    // x86-only rule; the launcher uses 64-bit Java
    assert!(!command.args.iter().any(|a| a == "-Xss1M"));
    // demo/custom resolution/quick-play feature rules stay off
    assert!(!command.args.iter().any(|a| a == "--demo"));
    assert!(!command.args.iter().any(|a| a == "--width"));
    assert!(!command.args.iter().any(|a| a == "--quickPlayMultiplayer"));
}

#[test]
fn custom_resolution_is_emitted_once_for_modern_versions() {
    let fixture = Fixture::new("resolution-1.20.1", "1.20.1").with_resolution(1920, 1080);
    let command = fixture.build();

    assert_eq!(
        count_args(&command.args, "--width"),
        1,
        "{:#?}",
        command.args
    );
    assert_eq!(
        count_args(&command.args, "--height"),
        1,
        "{:#?}",
        command.args
    );
    assert_eq!(value_after(&command.args, "--width"), "1920");
    assert_eq!(value_after(&command.args, "--height"), "1080");
    assert_no_placeholders(&command.args);
}

#[test]
fn custom_resolution_is_appended_once_for_legacy_versions() {
    let fixture = Fixture::new("resolution-1.12.2", "1.12.2").with_resolution(1600, 900);
    let command = fixture.build();

    assert_eq!(
        count_args(&command.args, "--width"),
        1,
        "{:#?}",
        command.args
    );
    assert_eq!(
        count_args(&command.args, "--height"),
        1,
        "{:#?}",
        command.args
    );
    assert_eq!(value_after(&command.args, "--width"), "1600");
    assert_eq!(value_after(&command.args, "--height"), "900");
}

// ---------------------------------------------------------------------------
// Room join arguments
// ---------------------------------------------------------------------------

#[test]
fn quick_play_join_uses_quick_play_path_for_modern() {
    let fixture = Fixture::new("join-1.20.1", "1.20.1");
    let session = fixture.data_dir().join("logs/game/session.log");
    let session_str = session.to_string_lossy().to_string();

    let command = fixture.build_with(Some(("play.example.com", Some(25565))), Some(&session_str));

    assert_eq!(
        value_after(&command.args, "--quickPlayMultiplayer"),
        "play.example.com:25565"
    );
    let expected_log = format!("{}.quickplay.json", session_str);
    assert_eq!(
        value_after(&command.args, "--quickPlayPath"),
        expected_log.as_str()
    );
    assert!(!command.args.iter().any(|a| a == "--server"));
    assert!(!command.args.iter().any(|a| a == "--port"));

    // Without a session log the join still works; only the confirmation file
    // is omitted.
    let without_log = fixture.build_with(Some(("play.example.com", Some(25565))), None);
    assert_eq!(
        value_after(&without_log.args, "--quickPlayMultiplayer"),
        "play.example.com:25565"
    );
    assert!(!without_log.args.iter().any(|a| a == "--quickPlayPath"));
}

#[test]
fn legacy_join_uses_server_and_port() {
    let fixture = Fixture::new("join-1.12.2", "1.12.2");
    // A session path must not leak into legacy launches (no Quick Play).
    let command = fixture.build_with(
        Some(("100.1.0.1", Some(12345))),
        Some(r"C:\tmp\session.log"),
    );

    assert_eq!(value_after(&command.args, "--server"), "100.1.0.1");
    assert_eq!(value_after(&command.args, "--port"), "12345");
    assert!(!command.args.iter().any(|a| a == "--quickPlayMultiplayer"));
    assert!(!command.args.iter().any(|a| a == "--quickPlayPath"));
}

// ---------------------------------------------------------------------------
// Classpath contract
// ---------------------------------------------------------------------------

fn assert_classpath_sane(fixture: &Fixture, entries: &[String]) {
    assert!(!entries.is_empty(), "classpath is empty");
    assert!(
        entries.iter().all(|e| !e.is_empty()),
        "empty classpath entry: {:#?}",
        entries
    );

    let unique: HashSet<&String> = entries.iter().collect();
    assert_eq!(
        unique.len(),
        entries.len(),
        "duplicate classpath entries:\n{:#?}",
        entries
    );

    for entry in entries {
        assert!(
            Path::new(entry).is_file(),
            "classpath entry does not exist: {}",
            entry
        );
    }

    assert_eq!(
        PathBuf::from(&entries[0]),
        fixture.entry_jar(),
        "entry JAR must be first on the classpath"
    );
}

#[test]
fn vanilla_classpath_starts_with_entry_jar_and_ends_with_client_jar() {
    let fixture = Fixture::new("cp-vanilla-1.20.1", "1.20.1");
    let entries = fixture.classpath();

    assert_classpath_sane(&fixture, &entries);
    assert_eq!(
        PathBuf::from(entries.last().unwrap()),
        fixture.client_jar(),
        "vanilla client.jar must be last on the classpath"
    );
    assert!(
        entries.iter().any(|e| e.contains("natives-windows")),
        "windows native artifacts missing:\n{:#?}",
        entries
    );
    assert!(
        !entries.iter().any(|e| e.contains("natives-linux")),
        "linux native artifacts leaked onto the Windows classpath:\n{:#?}",
        entries
    );

    // The manifest lists native artifacts for every platform; only Windows may
    // survive the OS rules.
    assert!(entries
        .iter()
        .any(|e| e.contains("lwjgl-3.3.1-natives-windows.jar")));
    assert!(!entries
        .iter()
        .any(|e| e.contains("lwjgl-3.3.1-natives-linux.jar")));
    assert!(!entries
        .iter()
        .any(|e| e.contains("lwjgl-3.3.1-natives-macos.jar")));
    assert!(!entries
        .iter()
        .any(|e| e.contains("lwjgl-3.3.1-natives-macos-arm64.jar")));
}

#[test]
fn legacy_classpath_includes_windows_natives_and_drops_osx_only_libraries() {
    let fixture = Fixture::new("cp-vanilla-1.12.2", "1.12.2");
    let entries = fixture.classpath();

    assert_classpath_sane(&fixture, &entries);
    assert_eq!(
        PathBuf::from(entries.last().unwrap()),
        fixture.client_jar(),
        "legacy client.jar must be last on the classpath"
    );

    // 1.12.2 uses the old `natives` classifier map, extracted by the launcher
    // and also placed on the classpath.
    assert!(entries
        .iter()
        .any(|e| e.ends_with("lwjgl-platform-2.9.4-nightly-20150209-natives-windows.jar")));
    assert!(entries
        .iter()
        .any(|e| e.ends_with("jinput-platform-2.0.5-natives-windows.jar")));
    assert!(entries
        .iter()
        .any(|e| e.ends_with("text2speech-1.10.3-natives-windows.jar")));

    assert!(!entries.iter().any(|e| e.contains("-natives-osx")));
    assert!(!entries.iter().any(|e| e.contains("-natives-linux")));
    // Allow-osx-only libraries and the 2.9.2 fallback must be excluded.
    assert!(!entries.iter().any(|e| e.contains("java-objc-bridge")));
    assert!(!entries
        .iter()
        .any(|e| e.contains("lwjgl-platform-2.9.2-nightly-20140822")));
}

#[test]
fn fabric_loader_libraries_come_before_vanilla() {
    let profile = load_loader_profile("fabric-0.16.9-1.20.1");
    let fixture =
        Fixture::new("cp-fabric-1.20.1", "1.20.1").with_loader(LoaderType::Fabric, profile.clone());
    let entries = fixture.classpath();

    let loader_paths: Vec<String> = profile
        .libraries
        .iter()
        .map(|lib| {
            fixture
                .config
                .libraries_dir()
                .join(&lib.path)
                .to_string_lossy()
                .to_string()
        })
        .collect();

    assert_classpath_sane(&fixture, &entries);
    assert_eq!(
        entries[1..1 + loader_paths.len()].to_vec(),
        loader_paths,
        "loader libraries must immediately follow the entry JAR:\n{:#?}",
        entries
    );
    assert_eq!(
        PathBuf::from(entries.last().unwrap()),
        fixture.client_jar(),
        "client.jar must stay last"
    );

    let command = fixture.build();
    assert!(
        !command
            .args
            .iter()
            .any(|a| a.starts_with("-Dvoidlauncher.mainClass=")),
        "loader launches must not use the vanilla entry wrapper:\n{:#?}",
        command.args
    );
    assert!(command
        .args
        .iter()
        .any(|a| a == "net.fabricmc.loader.impl.launch.knot.KnotClient"));
    assert!(command
        .args
        .iter()
        .any(|a| a == "-DFabricMcEmu= net.minecraft.client.main.Main "));
    assert_no_placeholders(&command.args);
}

#[test]
fn neoforge_overrides_vanilla_asm_and_keeps_loader_first() {
    let profile = load_loader_profile("neoforge-21.1.172-1.21.4");
    let fixture = Fixture::new("cp-neoforge-1.21.4", "1.21.4")
        .with_loader(LoaderType::NeoForge, profile.clone());
    let command = fixture.build();
    let entries = split_classpath(&command);

    let loader_paths: Vec<String> = profile
        .libraries
        .iter()
        .map(|lib| {
            fixture
                .config
                .libraries_dir()
                .join(&lib.path)
                .to_string_lossy()
                .to_string()
        })
        .collect();

    assert_classpath_sane(&fixture, &entries);
    assert_eq!(
        entries[1..1 + loader_paths.len()].to_vec(),
        loader_paths,
        "loader libraries must immediately follow the entry JAR:\n{:#?}",
        entries
    );

    let vanilla_asm = fixture
        .config
        .libraries_dir()
        .join("org/ow2/asm/asm/9.6/asm-9.6.jar")
        .to_string_lossy()
        .to_string();
    let loader_asm = fixture
        .config
        .libraries_dir()
        .join("org/ow2/asm/asm/9.8/asm-9.8.jar")
        .to_string_lossy()
        .to_string();
    assert!(
        !entries.contains(&vanilla_asm),
        "vanilla ASM 9.6 must be dropped when NeoForge ships ASM 9.8:\n{:#?}",
        entries
    );
    assert_eq!(
        entries.iter().filter(|e| **e == loader_asm).count(),
        1,
        "loader ASM 9.8 must appear exactly once:\n{:#?}",
        entries
    );
    assert_eq!(
        PathBuf::from(entries.last().unwrap()),
        fixture.client_jar(),
        "client.jar must stay last without an SRG jar"
    );

    // NeoForge starts through its module path, not the entry wrapper.
    assert!(
        !command
            .args
            .iter()
            .any(|a| a.starts_with("-Dvoidlauncher.mainClass=")),
        "loader launches must not use the vanilla entry wrapper:\n{:#?}",
        command.args
    );
    let module_path = value_after(&command.args, "-p");
    assert!(module_path.contains("bootstraplauncher"));
    assert!(module_path.contains("securejarhandler"));
    assert!(
        !module_path.contains("${"),
        "module path was not substituted: {}",
        module_path
    );
    let library_dir_arg = format!(
        "-DlibraryDirectory={}",
        fixture.config.libraries_dir().to_string_lossy()
    );
    assert!(
        command.args.contains(&library_dir_arg),
        "expected {} in argv:\n{:#?}",
        library_dir_arg,
        command.args
    );
    assert!(command
        .args
        .iter()
        .any(|a| a == "cpw.mods.bootstraplauncher.BootstrapLauncher"));
    assert_no_placeholders(&command.args);

    // Loader game arguments are appended after the vanilla ones.
    let launch_target = index_of(&command.args, "--launchTarget");
    assert_eq!(command.args[launch_target + 1], "forgeclient");
    assert_eq!(
        value_after(&command.args, "--fml.neoForgeVersion"),
        "21.1.172"
    );
    assert_eq!(value_after(&command.args, "--fml.mcVersion"), "1.21.4");
}

#[test]
fn client_jar_is_dropped_only_when_the_matching_srg_jar_exists() {
    let profile = load_loader_profile("neoforge-21.1.172-1.21.4");
    let fixture =
        Fixture::new("cp-srg-1.21.4", "1.21.4").with_loader(LoaderType::NeoForge, profile);

    let client_jar = fixture.client_jar().to_string_lossy().to_string();
    let before = split_classpath(&fixture.build());
    assert!(
        before.contains(&client_jar),
        "client.jar must stay without processor JARs"
    );

    // Simulate the NeoForge installer's processor output for THIS version.
    write_empty(
        &fixture
            .config
            .libraries_dir()
            .join("net/minecraft/client/1.21.4-test/client-1.21.4-test-srg.jar"),
    );

    let after = split_classpath(&fixture.build());
    assert!(
        !after.contains(&client_jar),
        "client.jar must be removed when the matching SRG jar exists:\n{:#?}",
        after
    );
    assert_classpath_sane(&fixture, &after);
}

#[test]
fn forge_legacy_game_arguments_replace_vanilla_ones() {
    let profile = load_loader_profile("forge-14.23.5.2860-1.12.2");
    let fixture =
        Fixture::new("cp-forge-1.12.2", "1.12.2").with_loader(LoaderType::Forge, profile.clone());
    let command = fixture.build();
    let entries = split_classpath(&command);

    assert_classpath_sane(&fixture, &entries);
    assert_eq!(
        PathBuf::from(entries.last().unwrap()),
        fixture.client_jar(),
        "1.12.2 Forge has no SRG jar, so client.jar stays"
    );

    // `legacy_args: true` means the profile's game arguments REPLACE the
    // vanilla `minecraftArguments`; emitting both would duplicate every flag.
    assert_eq!(
        count_args(&command.args, "--username"),
        1,
        "legacy game arguments must not be duplicated:\n{:#?}",
        command.args
    );
    assert_eq!(count_args(&command.args, "--accessToken"), 1);
    assert_eq!(
        value_after(&command.args, "--tweakClass"),
        "net.minecraftforge.fml.common.launcher.FMLTweaker"
    );
    assert!(command
        .args
        .iter()
        .any(|a| a == "net.minecraft.launchwrapper.Launch"));
    assert_no_placeholders(&command.args);
}

// ---------------------------------------------------------------------------
// Launch profile contract (M2)
// ---------------------------------------------------------------------------

#[test]
fn profile_traits_and_layout_are_explicit() {
    let forge = Fixture::new("profile-forge", "1.12.2").with_loader(
        LoaderType::Forge,
        load_loader_profile("forge-14.23.5.2860-1.12.2"),
    );
    let profile = forge.profile();

    assert!(profile.traits.legacy_args, "legacy Forge profile expected");
    assert!(
        !profile.traits.entry_wrapper,
        "loader launches must use the loader main class"
    );
    assert!(!profile.traits.quick_play);
    assert!(!profile.traits.has_custom_resolution);
    assert_eq!(profile.main_class, "net.minecraft.launchwrapper.Launch");
    assert_eq!(
        profile.classpath.first(),
        Some(&forge.entry_jar()),
        "entry JAR must stay first"
    );
    assert_eq!(
        profile.classpath.last(),
        Some(&forge.client_jar()),
        "1.12.2 Forge has no SRG JAR, so client.jar stays last"
    );
    assert_eq!(
        profile.natives_dir,
        forge.data_dir().join("versions/1.12.2/natives")
    );

    let modern = Fixture::new("profile-modern", "1.20.1").with_resolution(1920, 1080);
    let session = modern.data_dir().join("logs/game/session.log");
    let session_str = session.to_string_lossy().to_string();
    let profile = modern.profile_with(Some(("play.example.com", Some(25565))), Some(&session_str));

    assert!(
        profile.traits.entry_wrapper,
        "vanilla uses the entry wrapper"
    );
    assert!(profile.traits.has_custom_resolution);
    assert!(profile.traits.quick_play, "1.20+ join must use Quick Play");
    assert!(!profile.traits.legacy_args);
    assert_eq!(profile.main_class, "net.minecraft.client.main.Main");
    assert_eq!(
        profile
            .game_args
            .iter()
            .filter(|a| a.as_str() == "--width")
            .count(),
        1,
        "resolution must be emitted exactly once: {:#?}",
        profile.game_args
    );
    assert_eq!(
        profile
            .game_args
            .iter()
            .filter(|a| a.as_str() == "--height")
            .count(),
        1
    );
    assert!(
        !profile.jvm_args.iter().any(|a| a == "-cp"),
        "classpath is structured in the profile, not flattened into jvm_args"
    );
    assert!(!profile.jvm_args.iter().any(|a| a == "${classpath}"));
}

#[test]
fn placeholder_expansion_covers_every_known_token() {
    let loader = LoaderProfile {
        main_class: "example.PlaceholderMain".to_string(),
        libraries: Vec::new(),
        jvm_args: vec![
            "-Dnatives=${natives_directory}".to_string(),
            "-Dlib=${library_directory}".to_string(),
            "-Dsep=${classpath_separator}".to_string(),
            "-Dver=${version_name}".to_string(),
            "-Dln=${launcher_name}".to_string(),
            "-Dlv=${launcher_version}".to_string(),
            "-Dcp=${classpath}".to_string(),
        ],
        game_args: vec![
            "--user".to_string(),
            "${auth_player_name}".to_string(),
            "--session".to_string(),
            "${auth_session}".to_string(),
            "--userTypeArg".to_string(),
            "${user_type}".to_string(),
            "--versionTypeArg".to_string(),
            "${version_type}".to_string(),
            "--xuidArg".to_string(),
            "${auth_xuid}".to_string(),
            "--clientIdArg".to_string(),
            "${clientid}".to_string(),
            "--props".to_string(),
            "${user_properties}".to_string(),
            "--gameAssets".to_string(),
            "${game_assets}".to_string(),
        ],
        legacy_args: false,
    };
    let fixture = Fixture::new("placeholders", "1.20.1")
        .with_loader(LoaderType::Fabric, loader)
        .with_resolution(800, 600);
    let command = fixture.build();

    assert_no_placeholders(&command.args);
    let classpath = value_after(&command.args, "-cp");
    assert!(
        command.args.contains(&format!("-Dcp={}", classpath)),
        "embedded classpath placeholder must receive the final classpath:\n{:#?}",
        command.args
    );
    assert_eq!(value_after(&command.args, "--user"), TEST_USERNAME);
    assert_eq!(
        value_after(&command.args, "--session"),
        format!("0:{}:{}", TEST_UUID, TEST_USERNAME)
    );
    assert_eq!(value_after(&command.args, "--userTypeArg"), "msa");
    assert_eq!(value_after(&command.args, "--versionTypeArg"), "release");
    assert_eq!(value_after(&command.args, "--xuidArg"), "0");
    assert_eq!(value_after(&command.args, "--clientIdArg"), "");
    assert_eq!(value_after(&command.args, "--props"), "{}");
    assert_eq!(
        value_after(&command.args, "--gameAssets"),
        fixture
            .data_dir()
            .join("assets")
            .to_string_lossy()
            .to_string()
    );
}

#[test]
fn loader_jvm_classpath_placeholder_is_expanded_to_final_classpath() {
    // KI-5 regression: old Forge-era profiles ship
    // `-DlegacyClassPath=${classpath}`. The literal token must never reach
    // Java, and the value must be the *final* composed classpath (entry JAR,
    // loader/vanilla order and SRG handling already applied).
    let loader = LoaderProfile {
        main_class: "net.minecraft.launchwrapper.Launch".to_string(),
        libraries: Vec::new(),
        jvm_args: vec!["-DlegacyClassPath=${classpath}".to_string()],
        game_args: Vec::new(),
        legacy_args: false,
    };
    let fixture = Fixture::new("loader-classpath", "1.20.1").with_loader(LoaderType::Forge, loader);
    let command = fixture.build();

    let classpath = value_after(&command.args, "-cp").to_string();
    assert!(
        !command.args.iter().any(|a| a.contains("${classpath}")),
        "unsubstituted ${{classpath}} reached the command line:\n{:#?}",
        command.args
    );
    assert!(
        command
            .args
            .iter()
            .any(|a| *a == format!("-DlegacyClassPath={}", classpath)),
        "expected -DlegacyClassPath to receive the composed classpath:\n{:#?}",
        command.args
    );
}

#[test]
fn manifest_jvm_classpath_placeholder_is_expanded() {
    let fixture = Fixture::new("manifest-classpath", "1.20.1");
    let mut version = fixture.version.clone();
    version
        .arguments
        .as_mut()
        .expect("1.20.1 arguments")
        .jvm
        .push(serde_json::json!("-Dmanifest.classpath=${classpath}"));

    let command = build_launch_command(
        &fixture.config,
        &fixture.instance,
        &version,
        &auth(),
        None,
        None,
        fixture.java_path(),
        fixture.java_major(),
    )
    .expect("build command");

    let classpath = value_after(&command.args, "-cp").to_string();
    assert_no_placeholders(&command.args);
    assert!(command
        .args
        .contains(&format!("-Dmanifest.classpath={}", classpath)));
}

#[test]
fn literal_dollar_brace_in_instance_path_is_not_a_placeholder() {
    // Instance names are free-form apart from path-safety rules, so a valid
    // name may contain `${...}` — even a known token name. The expanded
    // --gameDir must be treated as data, not as an unresolved placeholder.
    let mut fixture = Fixture::new("literal-dollar", "1.20.1");
    fixture.instance.name = "${game_directory}".to_string();
    let command = fixture.build();

    let expected_game_dir = fixture
        .config
        .instances_dir()
        .join("${game_directory}")
        .join(".minecraft")
        .to_string_lossy()
        .to_string();
    assert_eq!(value_after(&command.args, "--gameDir"), expected_game_dir);
    assert_eq!(value_after(&command.args, "--username"), TEST_USERNAME);
}

#[test]
fn unknown_metadata_placeholder_is_still_rejected() {
    let loader = LoaderProfile {
        main_class: "example.UnknownPlaceholderMain".to_string(),
        libraries: Vec::new(),
        jvm_args: vec!["-Dunknown=${not_a_known_token}".to_string()],
        game_args: Vec::new(),
        legacy_args: false,
    };
    let fixture =
        Fixture::new("unknown-placeholder", "1.20.1").with_loader(LoaderType::Fabric, loader);
    let err = fixture.build_err();

    assert!(
        err.contains("Unresolved JVM placeholder"),
        "expected a JVM placeholder error, got: {}",
        err
    );
    assert!(
        err.contains("${not_a_known_token}"),
        "error must identify the unresolved token, got: {}",
        err
    );
}

#[test]
fn manifest_gc_flags_are_stripped_and_custom_args_follow_preset() {
    let fixture = Fixture::new("gc-strip", "1.20.1");
    let mut version = fixture.version.clone();
    let jvm = &mut version.arguments.as_mut().expect("1.20.1 arguments").jvm;
    jvm.insert(0, serde_json::json!("-XX:+UseZGC"));
    jvm.insert(0, serde_json::json!("-XX:+UseG1GC"));

    let mut instance = fixture.instance.clone();
    instance.gc_preset = Some("g1gc".to_string());
    instance.jvm_args = Some(vec!["-XX:+UseZGC".to_string(), "-Duser.arg=1".to_string()]);

    let command = build_launch_command(
        &fixture.config,
        &instance,
        &version,
        &auth(),
        None,
        None,
        fixture.java_path(),
        17,
    )
    .expect("build command");

    // Only the preset may select a GC; manifest and custom selectors are gone.
    assert_eq!(count_args(&command.args, "-XX:+UseG1GC"), 1);
    assert!(!command.args.iter().any(|a| a == "-XX:+UseZGC"));
    // Custom args are appended after the preset so they can override it.
    let preset = index_of(&command.args, "-XX:+UseG1GC");
    let custom = index_of(&command.args, "-Duser.arg=1");
    assert!(
        custom > preset,
        "custom JVM args must follow the preset:\n{:#?}",
        command.args
    );
}

#[test]
fn srg_jar_from_another_minecraft_version_keeps_client_jar() {
    let profile = load_loader_profile("neoforge-21.1.172-1.21.4");
    let fixture =
        Fixture::new("srg-other-version", "1.21.4").with_loader(LoaderType::NeoForge, profile);

    // A processor output for a different Minecraft version must not affect
    // this instance: old Forge versions still need their own client.jar.
    write_empty(
        &fixture
            .config
            .libraries_dir()
            .join("net/minecraft/client/1.20.1-test/client-1.20.1-test-srg.jar"),
    );

    let entries = split_classpath(&fixture.build());
    assert!(
        entries.contains(&fixture.client_jar().to_string_lossy().to_string()),
        "client.jar must survive an SRG JAR from another MC version:\n{:#?}",
        entries
    );
}

// ---------------------------------------------------------------------------
// Failure modes
// ---------------------------------------------------------------------------

#[test]
fn missing_vanilla_library_fails_before_java_starts() {
    let fixture = Fixture::new("missing-vanilla-lib", "1.20.1");
    std::fs::remove_file(
        fixture
            .config
            .libraries_dir()
            .join("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar"),
    )
    .expect("remove library");

    let err = fixture.build_err();
    assert!(
        err.contains("Required library is missing"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn missing_loader_library_fails_before_java_starts() {
    let profile = load_loader_profile("fabric-0.16.9-1.20.1");
    let fixture =
        Fixture::new("missing-loader-lib", "1.20.1").with_loader(LoaderType::Fabric, profile);
    std::fs::remove_file(
        fixture
            .config
            .libraries_dir()
            .join("net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"),
    )
    .expect("remove loader library");

    let err = fixture.build_err();
    assert!(
        err.contains("Mod loader library is missing"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn missing_client_jar_fails_before_java_starts() {
    let fixture = Fixture::new("missing-client", "1.20.1");
    std::fs::remove_file(fixture.client_jar()).expect("remove client jar");

    let err = fixture.build_err();
    assert!(
        err.contains("Client JAR is missing"),
        "unexpected error: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// Golden snapshots (regression net; semantic tests above are the oracle)
// ---------------------------------------------------------------------------

#[test]
fn golden_argv_snapshots() {
    let vanilla_1201 = Fixture::new("golden-vanilla-1.20.1", "1.20.1");
    assert_golden("vanilla-1.20.1", &vanilla_1201, &vanilla_1201.build().args);

    let vanilla_1122 = Fixture::new("golden-vanilla-1.12.2", "1.12.2");
    assert_golden("vanilla-1.12.2", &vanilla_1122, &vanilla_1122.build().args);

    let vanilla_2612 = Fixture::new("golden-vanilla-26.1.2", "26.1.2");
    assert_golden("vanilla-26.1.2", &vanilla_2612, &vanilla_2612.build().args);

    let fabric = Fixture::new("golden-fabric", "1.20.1").with_loader(
        LoaderType::Fabric,
        load_loader_profile("fabric-0.16.9-1.20.1"),
    );
    assert_golden("fabric-1.20.1", &fabric, &fabric.build().args);

    let neoforge = Fixture::new("golden-neoforge", "1.21.4").with_loader(
        LoaderType::NeoForge,
        load_loader_profile("neoforge-21.1.172-1.21.4"),
    );
    assert_golden("neoforge-1.21.4", &neoforge, &neoforge.build().args);

    let forge = Fixture::new("golden-forge", "1.12.2").with_loader(
        LoaderType::Forge,
        load_loader_profile("forge-14.23.5.2860-1.12.2"),
    );
    assert_golden("forge-1.12.2", &forge, &forge.build().args);

    let quickplay = Fixture::new("golden-quickplay", "1.20.1").with_resolution(1920, 1080);
    let session = quickplay.data_dir().join("logs/game/session.log");
    let session_str = session.to_string_lossy().to_string();
    let command = quickplay.build_with(Some(("play.example.com", Some(25565))), Some(&session_str));
    assert_golden("vanilla-1.20.1-quickplay", &quickplay, &command.args);
}

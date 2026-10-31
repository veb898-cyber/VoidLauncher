//! Shared harness for the integration tests in `src-tauri/tests/`.
//!
//! Every fixture lives entirely inside a per-test temporary directory: no test
//! touches the real VoidLauncher data dir, the network, or a real JVM.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use voidlauncher_lib::config::AppConfig;
use voidlauncher_lib::instances::{Instance, LoaderType, Resolution};
use voidlauncher_lib::launch::{
    build_launch_command, prepare_and_build_launch_profile, LaunchAuth, LaunchCommand,
    LaunchProfile,
};
use voidlauncher_lib::modloaders::LoaderProfile;
use voidlauncher_lib::versions::{maven_to_path, VersionInfo};

/// Deterministic values every test asserts against.
pub const TEST_USERNAME: &str = "TestPlayer";
pub const TEST_UUID: &str = "00000000-0000-0000-0000-000000000001";
pub const TEST_TOKEN: &str = "test-access-token";
pub const TEST_MEMORY_MB: u32 = 4096;

/// Temporary data directory removed on drop (unless `KEEP_TEST_DATA=1`).
pub struct TempData {
    root: PathBuf,
}

impl TempData {
    pub fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "voidlauncher-tests-{}-{}-{}",
            std::process::id(),
            label,
            n
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp dir");
        Self { root }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempData {
    fn drop(&mut self) {
        if std::env::var_os("KEEP_TEST_DATA").is_some() {
            eprintln!("keeping test data at {}", self.root.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

pub fn load_version(id: &str) -> VersionInfo {
    let path = fixtures_dir().join("versions").join(format!("{}.json", id));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {}", path.display(), e));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse fixture {}: {}", path.display(), e))
}

pub fn load_loader_profile(name: &str) -> LoaderProfile {
    let path = fixtures_dir()
        .join("loader_profiles")
        .join(format!("{}.json", name));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {}", path.display(), e));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse fixture {}: {}", path.display(), e))
}

pub fn write_empty(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, b"").expect("write empty file");
}

/// A valid empty ZIP. Native classifier JARs are actually opened by
/// `extract_natives`, so the fixtures must not hand it plain empty files.
pub fn write_empty_zip(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    let file = std::fs::File::create(path).expect("create zip file");
    zip::ZipWriter::new(file)
        .finish()
        .expect("finish empty zip");
}

/// Create an empty file for every artifact the fixture declares, so
/// `build_classpath` sees a complete (but byte-empty) library tree. Tests that
/// care about *which* entries are used assert on the classpath itself.
pub fn materialize_libraries(version: &VersionInfo, libraries_dir: &Path) {
    for lib in &version.libraries {
        match &lib.downloads {
            Some(downloads) => {
                if let Some(artifact) = &downloads.artifact {
                    write_empty(&libraries_dir.join(&artifact.path));
                }
                if let Some(classifiers) = &downloads.classifiers {
                    if let Some(map) = classifiers.as_object() {
                        for value in map.values() {
                            if let Some(path) = value.get("path").and_then(|p| p.as_str()) {
                                // `extract_natives` opens these; a plain empty
                                // file is not a valid archive.
                                write_empty_zip(&libraries_dir.join(path));
                            }
                        }
                    }
                }
            }
            None => write_empty(&libraries_dir.join(maven_to_path(&lib.name))),
        }
    }
}

/// Create an empty file for every library a persisted loader profile declares.
pub fn materialize_loader_libraries(profile: &LoaderProfile, libraries_dir: &Path) {
    for lib in &profile.libraries {
        write_empty(&libraries_dir.join(&lib.path));
    }
}

pub fn write_client_jar(config: &AppConfig, version_id: &str) {
    let dir = config.versions_dir().join(version_id);
    std::fs::create_dir_all(&dir).expect("create version dir");
    std::fs::write(dir.join("client.jar"), b"").expect("write client.jar");
}

/// Config with every machine-dependent default pinned to a test constant.
pub fn test_config(data_dir: &Path) -> AppConfig {
    AppConfig {
        data_dir: data_dir.to_path_buf(),
        default_memory_mb: TEST_MEMORY_MB,
        max_memory_mb: TEST_MEMORY_MB * 2,
        default_gc_preset: "g1gc".to_string(),
        default_jvm_args: vec!["-XX:+DisableExplicitGC".to_string()],
        java_path: None,
        ..AppConfig::default()
    }
}

pub fn test_instance(mc_version: &str) -> Instance {
    Instance::new(
        "Test",
        mc_version,
        TEST_MEMORY_MB,
        "g1gc",
        LoaderType::Vanilla,
        None,
    )
}

pub fn auth() -> LaunchAuth<'static> {
    LaunchAuth {
        access_token: TEST_TOKEN,
        uuid: TEST_UUID,
        username: TEST_USERNAME,
    }
}

/// One fully materialised launch fixture.
pub struct Fixture {
    pub temp: TempData,
    pub config: AppConfig,
    pub instance: Instance,
    pub version: VersionInfo,
}

impl Fixture {
    pub fn new(label: &str, version_id: &str) -> Self {
        let temp = TempData::new(label);
        let config = test_config(&temp.path().join("data"));
        let version = load_version(version_id);

        materialize_libraries(&version, &config.libraries_dir());
        write_client_jar(&config, version_id);

        Self {
            temp,
            config,
            instance: test_instance(version_id),
            version,
        }
    }

    /// Attach a loader profile and materialise its libraries. The loader type
    /// matters for Forge/NeoForge (SRG client.jar handling), so it is explicit.
    pub fn with_loader(mut self, loader: LoaderType, profile: LoaderProfile) -> Self {
        materialize_loader_libraries(&profile, &self.config.libraries_dir());
        self.instance.loader_profile = Some(profile);
        self.instance.loader = loader;
        self
    }

    pub fn with_resolution(mut self, width: u32, height: u32) -> Self {
        self.instance.resolution = Some(Resolution { width, height });
        self
    }

    pub fn data_dir(&self) -> &Path {
        &self.config.data_dir
    }

    pub fn build(&self) -> LaunchCommand {
        self.build_with(None, None)
    }

    pub fn build_with(
        &self,
        server: Option<(&str, Option<u16>)>,
        session_log: Option<&str>,
    ) -> LaunchCommand {
        build_launch_command(
            &self.config,
            &self.instance,
            &self.version,
            &auth(),
            server,
            session_log,
            self.java_path(),
            self.java_major(),
        )
        .unwrap_or_else(|e| panic!("build launch command: {}", e))
    }

    /// Build the pure semantic profile (after preparing launch-owned files).
    pub fn profile(&self) -> LaunchProfile {
        self.profile_with(None, None)
    }

    pub fn profile_with(
        &self,
        server: Option<(&str, Option<u16>)>,
        session_log: Option<&str>,
    ) -> LaunchProfile {
        prepare_and_build_launch_profile(
            &self.config,
            &self.instance,
            &self.version,
            &auth(),
            server,
            session_log,
            self.java_major(),
        )
        .unwrap_or_else(|e| panic!("build launch profile: {}", e))
        .0
    }

    pub fn build_err(&self) -> String {
        match build_launch_command(
            &self.config,
            &self.instance,
            &self.version,
            &auth(),
            None,
            None,
            self.java_path(),
            self.java_major(),
        ) {
            Ok(_) => panic!("expected launch command build to fail"),
            Err(e) => e.to_string(),
        }
    }

    pub fn java_path(&self) -> PathBuf {
        PathBuf::from(r"C:\fake\jdk\bin\java.exe")
    }

    pub fn java_major(&self) -> u32 {
        self.version.required_java_major()
    }

    pub fn entry_jar(&self) -> PathBuf {
        self.config
            .versions_dir()
            .join(&self.version.id)
            .join("entry")
            .join("VoidLauncherEntry.jar")
    }

    pub fn client_jar(&self) -> PathBuf {
        self.config
            .versions_dir()
            .join(&self.version.id)
            .join("client.jar")
    }

    /// Split the classpath argument into entries.
    pub fn classpath(&self) -> Vec<String> {
        split_classpath(&self.build())
    }
}

pub fn split_classpath(command: &LaunchCommand) -> Vec<String> {
    let index = command
        .args
        .iter()
        .position(|a| a == "-cp")
        .expect("classpath flag");
    command.args[index + 1]
        .split(';')
        .map(|s| s.to_string())
        .collect()
}

/// Index of the first occurrence of `needle`, panicking with the full argv on
/// failure so a broken composition is debuggable from CI logs.
pub fn index_of(args: &[String], needle: &str) -> usize {
    args.iter()
        .position(|a| a == needle)
        .unwrap_or_else(|| panic!("argument {:?} not found in argv:\n{:#?}", needle, args))
}

/// Value that follows the first occurrence of `flag`.
pub fn value_after<'a>(args: &'a [String], flag: &str) -> &'a str {
    let index = index_of(args, flag);
    args.get(index + 1)
        .unwrap_or_else(|| panic!("argument {:?} has no value in argv:\n{:#?}", flag, args))
}

/// Every value that follows an occurrence of `flag` (for duplicated flags such
/// as `--add-opens`).
pub fn values_after<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
    args.iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == flag)
        .filter_map(|(i, _)| args.get(i + 1).map(|s| s.as_str()))
        .collect()
}

pub fn count_args(args: &[String], needle: &str) -> usize {
    args.iter().filter(|a| a.as_str() == needle).count()
}

pub fn assert_no_placeholders(args: &[String]) {
    for arg in args {
        assert!(
            !arg.contains("${"),
            "unsubstituted placeholder in argument {:?}\nargv:\n{:#?}",
            arg,
            args
        );
    }
}

/// Normalise machine-specific paths so golden files are stable across machines.
pub fn normalize(args: &[String], data_dir: &Path) -> Vec<String> {
    let data = data_dir.to_string_lossy().replace('\\', "/");
    args.iter()
        .map(|a| {
            let a = a.replace('\\', "/").replace(&data, "<DATA>");
            // The package version changes on every release; a golden file must
            // not force a test rewrite for a version bump.
            match a.strip_prefix("-Dminecraft.launcher.version=") {
                Some(_) => "-Dminecraft.launcher.version=<VERSION>".to_string(),
                None => a,
            }
        })
        .collect()
}

/// Compare argv against a committed golden file. `UPDATE_GOLDEN=1` rewrites it.
pub fn assert_golden(name: &str, fixture: &Fixture, args: &[String]) {
    let normalized = normalize(args, fixture.data_dir());
    let text = normalized.join("\n") + "\n";

    let path = fixtures_dir()
        .join("golden")
        .join(format!("{}.args.txt", name));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create golden dir");
        }
        std::fs::write(&path, text.as_bytes()).expect("write golden file");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read golden {}: {}\nrun with UPDATE_GOLDEN=1 to generate it",
            path.display(),
            e
        )
    });
    assert_eq!(
        text,
        expected,
        "golden argv mismatch for {} ({}).\nrun with UPDATE_GOLDEN=1 after reviewing the change",
        name,
        path.display()
    );
}

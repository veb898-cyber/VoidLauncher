//! Known behavioural defects that are **recorded, not accepted**.
//!
//! Every test here is `#[ignore]`d on purpose. It documents the *correct*
//! expectation (the side that currently fails), so the suite stays green while
//! the bug is real. When a bug is fixed, running `cargo test -- --ignored`
//! turns the corresponding test green and forces the entry to be removed from
//! `tests/KNOWN_ISSUES.md` instead of silently becoming a regression guard for
//! buggy behaviour.
//!
//! These tests must never be un-ignored to "make CI pass" — they encode
//! behaviour the launcher is supposed to have.

#![cfg(target_os = "windows")]

mod support;

use std::io::Write;
use std::path::Path;

use support::*;
use voidlauncher_lib::versions::{
    build_classpath, extract_natives, get_jvm_arguments, VersionInfo,
};

fn write_native_jar(path: &Path, entries: &[(&str, &[u8])]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create native jar dir");
    }
    let file = std::fs::File::create(path).expect("create native jar");
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in entries {
        writer.start_file(*name, options).expect("start zip entry");
        writer.write_all(bytes).expect("write zip entry");
    }
    writer.finish().expect("finish native jar");
}

fn synthetic_version(libraries: serde_json::Value, jvm: serde_json::Value) -> VersionInfo {
    serde_json::from_value(serde_json::json!({
        "id": "synthetic",
        "type": "release",
        "mainClass": "synthetic.Main",
        "arguments": { "game": [], "jvm": jvm },
        "libraries": libraries,
        "downloads": { "client": { "sha1": "", "size": 0, "url": "" } },
        "assetIndex": { "id": "1", "sha1": "", "size": 0, "totalSize": 0, "url": "" },
        "assets": "1"
    }))
    .expect("synthetic version JSON")
}

fn native_library_json() -> serde_json::Value {
    serde_json::json!([{
        "name": "org.example:native:1.0",
        "natives": { "windows": "natives-windows-${arch}" },
        "downloads": {
            "artifact": {
                "path": "org/example/native/1.0/native-1.0.jar",
                "sha1": "", "size": 0, "url": ""
            },
            "classifiers": {
                "natives-windows-64": {
                    "path": "org/example/native/1.0/native-1.0-natives-windows-64.jar",
                    "sha1": "", "size": 0, "url": ""
                },
                "natives-windows-arm64": {
                    "path": "org/example/native/1.0/native-1.0-natives-windows-arm64.jar",
                    "sha1": "", "size": 0, "url": ""
                }
            }
        }
    }])
}

// ---------------------------------------------------------------------------
// KI-1: stale native files are never refreshed
// ---------------------------------------------------------------------------

#[test]
#[ignore = "known issue KI-1: extract_natives skips existing files, so a stale/corrupt DLL survives updates"]
fn native_extraction_replaces_existing_file() {
    let fixture = Fixture::new("known-stale-native", "1.12.2");
    let native_jar = fixture.config.libraries_dir().join(
        "org/lwjgl/lwjgl/lwjgl-platform/2.9.4-nightly-20150209/\
         lwjgl-platform-2.9.4-nightly-20150209-natives-windows.jar",
    );
    write_native_jar(&native_jar, &[("lwjgl.dll", b"new-build")]);

    let natives_dir = fixture.data_dir().join("versions/1.12.2/natives");
    std::fs::create_dir_all(&natives_dir).expect("create natives dir");
    let extracted = natives_dir.join("lwjgl.dll");
    std::fs::write(&extracted, b"old-build").expect("write stale file");

    extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &natives_dir,
    )
    .expect("extract natives");

    assert_eq!(
        std::fs::read(&extracted).expect("read extracted"),
        b"new-build".to_vec(),
        "a native file left over from an older download must be overwritten"
    );
}

// ---------------------------------------------------------------------------
// KI-2: a corrupt native JAR is silently ignored
// ---------------------------------------------------------------------------

#[test]
#[ignore = "known issue KI-2: a present but corrupt native JAR is skipped without an error"]
fn corrupt_native_jar_is_reported() {
    let fixture = Fixture::new("known-corrupt-native", "1.12.2");
    let native_jar = fixture.config.libraries_dir().join(
        "org/lwjgl/lwjgl/lwjgl-platform/2.9.4-nightly-20150209/\
         lwjgl-platform-2.9.4-nightly-20150209-natives-windows.jar",
    );
    std::fs::write(&native_jar, b"this is not a zip archive").expect("write corrupt jar");

    let result = extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &fixture.data_dir().join("versions/1.12.2/natives"),
    );

    assert!(
        result.is_err(),
        "extraction of a required native JAR that exists but cannot be opened must fail loudly"
    );
}

// ---------------------------------------------------------------------------
// KI-3: `${arch}` is hardcoded to the x64 classifier
// ---------------------------------------------------------------------------

#[test]
#[ignore = "known issue KI-3: ${arch} becomes '64' regardless of host architecture"]
fn native_classifier_uses_the_real_host_architecture() {
    let temp = TempData::new("known-arch-classifier");
    let libraries = temp.path().join("libraries");
    let client = temp.path().join("client.jar");
    std::fs::write(&client, b"").expect("client jar");
    for path in [
        "org/example/native/1.0/native-1.0.jar",
        "org/example/native/1.0/native-1.0-natives-windows-64.jar",
        "org/example/native/1.0/native-1.0-natives-windows-arm64.jar",
    ] {
        write_empty(&libraries.join(path));
    }

    let version = synthetic_version(native_library_json(), serde_json::json!([]));
    let classpath = build_classpath(&version, &libraries, &client).expect("classpath");

    // On x64 the current hardcoding happens to be right; the bug only shows on
    // Windows ARM64, where Mojang expects the `arm64` classifier.
    let expected = if cfg!(target_arch = "aarch64") {
        "native-1.0-natives-windows-arm64.jar"
    } else {
        "native-1.0-natives-windows-64.jar"
    };
    assert!(
        classpath.contains(expected),
        "expected the {} classifier, classpath was:\n{}",
        expected,
        classpath
    );
}

// ---------------------------------------------------------------------------
// KI-4: Mojang `arm64` argument rules never match
// ---------------------------------------------------------------------------

#[test]
#[ignore = "known issue KI-4: rules with arch 'arm64' never match (host arch is reported as 'aarch64')"]
fn arm64_argument_rule_matches_on_arm64_hosts() {
    let version = synthetic_version(
        serde_json::json!([]),
        serde_json::json!([{
            "rules": [{ "action": "allow", "os": { "name": "windows", "arch": "arm64" } }],
            "value": "-Darm64.rule.matched=true"
        }]),
    );
    let args = get_jvm_arguments(&version);

    // On x64 the rule must stay off; on Windows ARM64 it must turn on.
    let should_match = cfg!(target_arch = "aarch64");
    assert_eq!(
        args.iter().any(|a| a == "-Darm64.rule.matched=true"),
        should_match,
        "arm64 rule handling is wrong for this host (args: {:?})",
        args
    );
}

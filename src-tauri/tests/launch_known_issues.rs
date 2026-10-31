//! Regression tests for launch-composition defects recorded in
//! `tests/KNOWN_ISSUES.md`.
//!
//! KI-1 and KI-2 are fixed: their tests now run with the normal suite. KI-3 and
//! KI-4 are still open and therefore `#[ignore]`d — they encode the *correct*
//! expectation, so `cargo test -- --ignored` shows the remaining gap without
//! turning the bug into an accepted baseline.
//!
//! Tests must never be un-ignored to "make CI pass" while the defect is real;
//! they encode behaviour the launcher is supposed to have.

#![cfg(target_os = "windows")]

mod support;

use std::io::{Read, Write};
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

fn fixture_native_jar(fixture: &Fixture) -> std::path::PathBuf {
    fixture.config.libraries_dir().join(
        "org/lwjgl/lwjgl/lwjgl-platform/2.9.4-nightly-20150209/\
         lwjgl-platform-2.9.4-nightly-20150209-natives-windows.jar",
    )
}

/// Flip the CRC stored for `entry` in the central directory, leaving the ZIP
/// structurally valid. Opening the archive and locating the entry still work;
/// the failure appears only when its bytes are read to EOF.
fn corrupt_central_directory_crc(path: &Path, entry: &str) {
    let mut bytes = std::fs::read(path).expect("read native jar");
    let eocd = bytes
        .windows(4)
        .rposition(|signature| signature == [0x50, 0x4b, 0x05, 0x06])
        .expect("end of central directory");
    let entries = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let mut offset = u32::from_le_bytes(bytes[eocd + 16..eocd + 20].try_into().unwrap()) as usize;

    for _ in 0..entries {
        assert_eq!(
            &bytes[offset..offset + 4],
            &[0x50, 0x4b, 0x01, 0x02],
            "malformed central directory"
        );
        let name_len = u16::from_le_bytes([bytes[offset + 28], bytes[offset + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[offset + 30], bytes[offset + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[offset + 32], bytes[offset + 33]]) as usize;
        let name_start = offset + 46;
        let name_end = name_start + name_len;
        assert!(name_end <= bytes.len(), "malformed central directory");

        if &bytes[name_start..name_end] == entry.as_bytes() {
            let crc = u32::from_le_bytes(bytes[offset + 16..offset + 20].try_into().unwrap());
            bytes[offset + 16..offset + 20].copy_from_slice(&(crc ^ 0xFFFF_FFFF).to_le_bytes());
            std::fs::write(path, bytes).expect("write corrupted native jar");
            return;
        }
        offset = name_end + extra_len + comment_len;
    }

    panic!("central directory entry not found: {}", entry);
}

fn staging_leftovers(natives_dir: &Path) -> Vec<String> {
    std::fs::read_dir(natives_dir.parent().unwrap())
        .expect("read version dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".natives-staging-"))
        .collect()
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
fn native_extraction_replaces_existing_file() {
    let fixture = Fixture::new("known-stale-native", "1.12.2");
    let native_jar = fixture_native_jar(&fixture);
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
fn corrupt_native_jar_is_reported() {
    let fixture = Fixture::new("known-corrupt-native", "1.12.2");
    let native_jar = fixture_native_jar(&fixture);
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

#[test]
fn native_extraction_removes_files_from_a_previous_set() {
    let fixture = Fixture::new("native-stale-set", "1.12.2");
    let native_jar = fixture_native_jar(&fixture);
    let natives_dir = fixture.data_dir().join("versions/1.12.2/natives");

    write_native_jar(
        &native_jar,
        &[("lwjgl.dll", b"build-1"), ("removed.dll", b"old-only")],
    );
    extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &natives_dir,
    )
    .expect("extract first native set");

    write_native_jar(
        &native_jar,
        &[("lwjgl.dll", b"build-2"), ("added.dll", b"new-only")],
    );
    extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &natives_dir,
    )
    .expect("extract second native set");

    assert_eq!(
        std::fs::read(natives_dir.join("lwjgl.dll")).expect("read replaced native"),
        b"build-2".to_vec(),
        "the current native must replace the previous build"
    );
    assert!(
        natives_dir.join("added.dll").is_file(),
        "a file shipped by the new native set must be extracted"
    );
    assert!(
        !natives_dir.join("removed.dll").exists(),
        "a file that is no longer shipped must be removed"
    );
}

#[test]
fn failed_native_extraction_does_not_touch_existing_natives() {
    let fixture = Fixture::new("native-failure-preserves", "1.12.2");
    let native_jar = fixture_native_jar(&fixture);
    let natives_dir = fixture.data_dir().join("versions/1.12.2/natives");

    write_native_jar(&native_jar, &[("lwjgl.dll", b"good-build")]);
    extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &natives_dir,
    )
    .expect("extract valid native set");

    std::fs::write(&native_jar, b"this is not a zip archive").expect("corrupt native jar");
    let err = extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &natives_dir,
    )
    .expect_err("a corrupt native archive must fail preparation")
    .to_string();

    assert!(
        err.contains("lwjgl-platform") && err.contains("not a valid archive"),
        "the error must identify the archive and the reason, got: {}",
        err
    );
    assert_eq!(
        std::fs::read(natives_dir.join("lwjgl.dll")).expect("read preserved native"),
        b"good-build".to_vec(),
        "a failed extraction must not remove or rewrite the last good natives"
    );
    assert!(
        staging_leftovers(&natives_dir).is_empty(),
        "failed extraction must clean its staging directory, found: {:?}",
        staging_leftovers(&natives_dir)
    );
}

#[test]
fn native_extraction_rolls_back_when_entry_data_is_corrupt() {
    let fixture = Fixture::new("native-corrupt-entry", "1.12.2");
    let native_jar = fixture_native_jar(&fixture);
    let natives_dir = fixture.data_dir().join("versions/1.12.2/natives");

    write_native_jar(&native_jar, &[("previous.dll", b"previous-build")]);
    extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &natives_dir,
    )
    .expect("extract previous native set");

    let manifest_path = natives_dir.join(".voidlauncher-natives.json");
    let manifest_before = std::fs::read(&manifest_path).expect("read manifest");

    write_native_jar(
        &native_jar,
        &[
            // A normal entry is extracted first, so the failure happens with
            // partially staged content already on disk.
            ("good.dll", b"new-good"),
            ("corrupt.dll", b"corrupt-payload"),
        ],
    );
    corrupt_central_directory_crc(&native_jar, "corrupt.dll");

    // The archive itself must still open: the error has to come from reading
    // the corrupted entry, not from `ZipArchive::new`.
    {
        let file = std::fs::File::open(&native_jar).expect("open corrupted native jar");
        let mut archive =
            zip::ZipArchive::new(file).expect("corrupted CRC must leave the archive readable");
        let mut entry = archive
            .by_name("corrupt.dll")
            .expect("corrupted entry must be listed");
        let mut sink = Vec::new();
        let read_err = entry
            .read_to_end(&mut sink)
            .expect_err("reading the corrupted entry must fail");
        assert_eq!(read_err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            read_err.to_string().contains("Invalid checksum"),
            "unexpected read error: {}",
            read_err
        );
    }

    let err = extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &natives_dir,
    )
    .expect_err("corrupt entry data must fail preparation")
    .to_string();

    assert!(
        err.contains("Failed to extract native file")
            && err.contains("org.lwjgl.lwjgl:lwjgl-platform")
            && err.contains(&native_jar.display().to_string()),
        "the error must name the library, archive path and extraction stage, got: {}",
        err
    );
    assert_eq!(
        std::fs::read(natives_dir.join("previous.dll")).expect("read previous native"),
        b"previous-build".to_vec(),
        "the previous native set must survive a mid-extraction failure"
    );
    assert!(
        !natives_dir.join("good.dll").exists() && !natives_dir.join("corrupt.dll").exists(),
        "no partially staged file may reach the live natives directory"
    );
    assert_eq!(
        std::fs::read(&manifest_path).expect("read manifest after failure"),
        manifest_before,
        "a failed extraction must not rewrite the manifest"
    );
    assert!(
        staging_leftovers(&natives_dir).is_empty(),
        "failed extraction must clean its staging directory, found: {:?}",
        staging_leftovers(&natives_dir)
    );
}

// This test file is Windows-only; `set_readonly(false)` restores the normal
// DOS attribute and does not make anything world-writable.
#[allow(clippy::permissions_set_readonly_false)]
#[test]
fn native_extraction_reports_a_native_file_that_cannot_be_replaced() {
    let fixture = Fixture::new("native-readonly", "1.12.2");
    let native_jar = fixture_native_jar(&fixture);
    let natives_dir = fixture.data_dir().join("versions/1.12.2/natives");

    write_native_jar(&native_jar, &[("lwjgl.dll", b"build-1")]);
    extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &natives_dir,
    )
    .expect("extract first native set");

    let target = natives_dir.join("lwjgl.dll");
    let mut permissions = std::fs::metadata(&target)
        .expect("stat native")
        .permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&target, permissions).expect("make native read-only");

    write_native_jar(
        &native_jar,
        &[("lwjgl.dll", b"build-2"), ("added.dll", b"new-only")],
    );
    let result = extract_natives(
        &fixture.version,
        &fixture.config.libraries_dir(),
        &natives_dir,
    );

    let mut permissions = std::fs::metadata(&target)
        .expect("stat native")
        .permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(&target, permissions).expect("restore native permissions");

    let err = result
        .expect_err("a native file that cannot be replaced must fail preparation")
        .to_string();
    assert!(
        err.contains("Cannot replace native file") && err.contains("lwjgl.dll"),
        "the error must name the blocked file and explain the lock, got: {}",
        err
    );
    assert_eq!(
        std::fs::read(&target).expect("read blocked native"),
        b"build-1".to_vec(),
        "a blocked replacement must leave the old file untouched"
    );
    assert!(
        !natives_dir.join("added.dll").exists(),
        "preflight must fail before any file from the new set is installed"
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

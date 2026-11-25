pub(crate) mod artifact;
#[allow(dead_code)]
pub mod fabric;
pub mod forge;
pub mod neoforge;
pub(crate) mod prism_meta;
pub(crate) mod processor_cache;

use crate::error::Result;
use crate::integrity::HashSpec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Information about a mod loader version
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoaderVersion {
    pub version: String,
    pub stable: bool,
}

/// A page of loader versions plus the total count of matching
/// versions across all pages. The wizard uses this for infinite
/// scroll: it shows `versions` immediately, then asks for the next
/// `PAGE_SIZE` items starting at `versions.length` until the
/// accumulator reaches `total`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoaderVersionPage {
    pub versions: Vec<LoaderVersion>,
    pub total: usize,
}

/// Profile data that modifies the launch configuration
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoaderProfile {
    /// Main class override
    pub main_class: String,
    /// Additional libraries to add to classpath
    pub libraries: Vec<LoaderLibrary>,
    /// Additional JVM arguments
    pub jvm_args: Vec<String>,
    /// Additional game arguments
    pub game_args: Vec<String>,
    /// True when game_args came from the legacy `minecraftArguments` string
    /// (MC <= 1.12.2 style) and therefore REPLACE the vanilla game
    /// arguments instead of being appended to them.
    #[serde(default)]
    pub legacy_args: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoaderLibrary {
    pub name: String,
    pub url: String,
    pub path: String,
    #[serde(default)]
    pub sha1: Option<String>,
    /// Stronger checksums are optional so profiles written by older builds
    /// remain readable. Fabric metadata currently publishes SHA-1 only; the
    /// repository fallback may supply SHA-256/SHA-512.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha512: Option<String>,
    /// "metadata" or "repository" when known. This is informational; the
    /// checksum itself is still validated by `HashSpec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_source: Option<String>,
    pub size: Option<u64>,
}

impl LoaderLibrary {
    /// Strongest available checksum for this library, if one is recorded.
    /// Empty values mean "absent"; a malformed non-empty value is an error.
    pub fn checksum(&self) -> Result<Option<HashSpec>> {
        let mut hashes = HashMap::new();
        for (key, value) in [
            ("sha1", self.sha1.as_deref()),
            ("sha256", self.sha256.as_deref()),
            ("sha512", self.sha512.as_deref()),
        ] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                hashes.insert(key.to_string(), value.to_string());
            }
        }
        HashSpec::from_metadata(&hashes)
    }
}

/// Validate version strings that loader code later uses in URLs, temp file
/// names and library paths. Pack manifests are remote input, so `..`,
/// separators and drive prefixes must be rejected before any join.
pub(crate) fn validate_loader_request(mc_version: &str, loader_version: &str) -> Result<()> {
    for (value, label) in [
        (mc_version, "Minecraft version"),
        (loader_version, "loader version"),
    ] {
        if crate::import::check_safe_file_name(value).is_err() {
            return Err(crate::error::LauncherError::ModLoader(format!(
                "Invalid {}: {:?}",
                label, value
            )));
        }
    }
    Ok(())
}

/// Get the loader profile for a given loader type
#[allow(dead_code)]
pub async fn get_profile(
    loader: &str,
    mc_version: &str,
    loader_version: &str,
) -> Result<LoaderProfile> {
    validate_loader_request(mc_version, loader_version)?;
    match loader {
        "Fabric" => fabric::get_profile(mc_version, loader_version).await,
        "Forge" => forge::get_profile(mc_version, loader_version).await,
        "NeoForge" => neoforge::get_profile(mc_version, loader_version).await,
        _ => Err(crate::error::LauncherError::ModLoader(format!(
            "Unknown loader: {}",
            loader
        ))),
    }
}

/// Install a mod loader and return its profile
#[allow(dead_code)]
pub async fn install_loader(
    loader: &str,
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
    app: Option<&tauri::AppHandle>,
) -> Result<LoaderProfile> {
    validate_loader_request(mc_version, loader_version)?;
    // Forge/NeoForge need the vanilla client JAR on disk to patch it into
    // the loader version. Download it first when it's missing, so installing
    // a loader works even right after a pack import (before the first launch).
    if loader == "Forge" || loader == "NeoForge" {
        ensure_client_jar(mc_version, libraries_dir, versions_dir).await?;
    }
    match loader {
        "Fabric" => fabric::install(mc_version, loader_version, libraries_dir).await,
        "Forge" => {
            forge::install(mc_version, loader_version, libraries_dir, versions_dir, app).await
        }
        "NeoForge" => {
            neoforge::install(mc_version, loader_version, libraries_dir, versions_dir, app).await
        }
        _ => Err(crate::error::LauncherError::ModLoader(format!(
            "Unknown loader: {}",
            loader
        ))),
    }
}

/// Download the vanilla client JAR (and its libraries) for `mc_version` so a
/// subsequent Forge/NeoForge install has the file it needs to patch. The JAR
/// is normally fetched at first launch; importing a pack must not depend on
/// that, and neither should an auto-loader-install at launch.
async fn ensure_client_jar(
    mc_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
) -> Result<()> {
    let vd = versions_dir.join(mc_version);
    if vd.join("client.jar").exists() || vd.join(format!("{}.jar", mc_version)).exists() {
        return Ok(());
    }
    tracing::info!(
        target: "launcher",
        "Vanilla client for {} not on disk — downloading before loader install...",
        mc_version
    );
    let manifest = crate::versions::fetch_version_manifest().await?;
    let url = manifest
        .versions
        .iter()
        .find(|v| v.id == mc_version)
        .map(|v| v.url.clone())
        .ok_or_else(|| {
            crate::error::LauncherError::ModLoader(format!(
                "Version {} not found in manifest",
                mc_version
            ))
        })?;
    let info = crate::versions::fetch_version_info(&url).await?;
    let libraries_dir_buf = libraries_dir.to_path_buf();
    let versions_dir_buf = versions_dir.to_path_buf();
    let files = crate::versions::collect_downloads(&info, &libraries_dir_buf, &versions_dir_buf)?;
    let missing: Vec<_> = files
        .into_iter()
        .filter(|(_, path, _, _)| !path.exists())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    crate::download::download_files(
        missing,
        |completed, total, _bytes_done, _bytes_total, _msg| {
            tracing::info!(
                target: "launcher",
                "Vanilla client download {}/{}",
                completed,
                total
            );
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_request_accepts_real_version_strings() {
        for (mc, loader) in [
            ("1.20.1", "0.15.11"),
            ("1.21.4", "21.4.153"),
            ("1.20.1", "47.4.0"),
            ("24w14a", "0.15.0-beta.1"),
        ] {
            assert!(
                validate_loader_request(mc, loader).is_ok(),
                "{} / {} must be accepted",
                mc,
                loader
            );
        }
    }

    const LEGACY_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

    fn legacy_profile_fixture(loader: &str) -> serde_json::Value {
        let (main_class, name, url, path, size) = match loader {
            "Fabric" => (
                "net.fabricmc.loader.impl.launch.knot.KnotClient",
                "org.ow2.asm:asm:9.6",
                "https://maven.fabricmc.net/",
                "org/ow2/asm/asm/9.6/asm-9.6.jar",
                122_535,
            ),
            "Forge" => (
                "cpw.mods.bootstraplauncher.BootstrapLauncher",
                "net.minecraftforge:forge:1.20.1-47.2.0:universal",
                "https://maven.minecraftforge.net/",
                "net/minecraftforge/forge/1.20.1-47.2.0/forge-1.20.1-47.2.0-universal.jar",
                8_112_112,
            ),
            "NeoForge" => (
                "cpw.mods.bootstraplauncher.BootstrapLauncher",
                "net.neoforged:neoforge:20.4.237:universal",
                "https://maven.neoforged.net/releases/",
                "net/neoforged/neoforge/20.4.237/neoforge-20.4.237-universal.jar",
                9_223_372,
            ),
            other => panic!("unknown loader fixture {other}"),
        };
        serde_json::json!({
            "main_class": main_class,
            "libraries": [{
                "name": name,
                "url": url,
                "path": path,
                "sha1": LEGACY_SHA1,
                "size": size
            }],
            "jvm_args": [],
            "game_args": [],
            "legacy_args": false
        })
    }

    #[test]
    fn legacy_fabric_forge_and_neoforge_profiles_round_trip_without_new_fields() {
        for loader in ["Fabric", "Forge", "NeoForge"] {
            let profile: LoaderProfile =
                serde_json::from_value(legacy_profile_fixture(loader)).unwrap();
            let lib = &profile.libraries[0];
            assert_eq!(lib.sha1.as_deref(), Some(LEGACY_SHA1), "{loader}");
            assert_eq!(lib.sha256, None, "{loader}");
            assert_eq!(lib.sha512, None, "{loader}");
            assert_eq!(lib.checksum_source, None, "{loader}");
            assert!(lib.size.is_some(), "{loader}");
            assert_eq!(
                lib.checksum().unwrap().unwrap().algorithm(),
                "sha1",
                "{loader} must keep using its persisted SHA-1"
            );

            let saved = serde_json::to_value(&profile).unwrap();
            let saved_lib = &saved["libraries"][0];
            assert_eq!(saved_lib["sha1"], LEGACY_SHA1, "{loader}");
            assert!(saved_lib["size"].as_u64().is_some(), "{loader}");
            assert!(saved_lib.get("sha256").is_none(), "{loader}");
            assert!(saved_lib.get("sha512").is_none(), "{loader}");
            assert!(saved_lib.get("checksum_source").is_none(), "{loader}");

            let round_tripped: LoaderProfile = serde_json::from_value(saved).unwrap();
            assert_eq!(
                round_tripped.libraries[0].sha1.as_deref(),
                Some(LEGACY_SHA1),
                "{loader}"
            );
        }
    }

    #[test]
    fn legacy_null_sha1_stays_null_and_does_not_gain_a_checksum() {
        let mut fixture = legacy_profile_fixture("Forge");
        fixture["libraries"][0]["sha1"] = serde_json::Value::Null;
        let profile: LoaderProfile = serde_json::from_value(fixture).unwrap();
        assert!(profile.libraries[0].checksum().unwrap().is_none());
        let saved = serde_json::to_value(&profile).unwrap();
        assert!(saved["libraries"][0]["sha1"].is_null());
        assert!(saved["libraries"][0].get("sha256").is_none());
        assert!(saved["libraries"][0].get("sha512").is_none());
        assert!(saved["libraries"][0].get("checksum_source").is_none());
    }

    #[test]
    fn malformed_new_checksum_does_not_fall_back_to_legacy_sha1() {
        let mut fixture = legacy_profile_fixture("NeoForge");
        fixture["libraries"][0]["sha512"] = serde_json::json!("not-a-hash");
        let profile: LoaderProfile = serde_json::from_value(fixture).unwrap();
        assert!(
            profile.libraries[0].checksum().is_err(),
            "a malformed non-empty SHA-512 must be an error, not a SHA-1 fallback"
        );
    }

    #[test]
    fn loader_library_checksum_uses_the_strongest_persisted_value() {
        let base = LoaderLibrary {
            name: "example:artifact:1.0".to_string(),
            url: "https://maven.fabricmc.net/".to_string(),
            path: "example/artifact/1.0/artifact-1.0.jar".to_string(),
            sha1: Some("a".repeat(40)),
            sha256: Some("b".repeat(64)),
            sha512: Some("c".repeat(128)),
            checksum_source: Some("metadata".to_string()),
            size: Some(1),
        };
        assert_eq!(base.checksum().unwrap().unwrap().algorithm(), "sha512");

        let sha256_only = LoaderLibrary {
            sha1: None,
            sha512: None,
            ..base.clone()
        };
        assert_eq!(
            sha256_only.checksum().unwrap().unwrap().algorithm(),
            "sha256"
        );

        let sha1_only = LoaderLibrary {
            sha256: None,
            sha512: None,
            ..base.clone()
        };
        assert_eq!(sha1_only.checksum().unwrap().unwrap().algorithm(), "sha1");

        let none = LoaderLibrary {
            sha1: None,
            sha256: None,
            sha512: None,
            ..base.clone()
        };
        assert!(none.checksum().unwrap().is_none());

        let malformed = LoaderLibrary {
            sha512: Some("not-a-hash".to_string()),
            ..base
        };
        assert!(malformed.checksum().is_err());
    }

    #[test]
    fn loader_request_rejects_path_components() {
        assert!(validate_loader_request("../escape", "1.0").is_err());
        assert!(validate_loader_request("1.20.1", "..\\escape").is_err());
        assert!(validate_loader_request("1.20.1", "C:evil").is_err());
        assert!(validate_loader_request("1.20.1", "").is_err());
    }
}

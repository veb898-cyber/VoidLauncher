//! LiteLoader mod loader support.
//!
//! LiteLoader is a legacy mod loader that targets Minecraft 1.12.x and
//! older. The upstream project has been abandoned since 2017 — the
//! last entry in the Prism metadata mirror is `1.12.2-SNAPSHOT` from
//! November 2017. We include it anyway for parity with Prism Launcher
//! (which still lists it in its loader picker) but the wizard's
//! install path is gated behind a "not supported" toast, see
//! `cmd_install_liteloader` in `lib.rs`.

use crate::error::{LauncherError, Result};
use crate::modloaders::{LoaderLibrary, LoaderProfile, LoaderVersionPage};
use crate::versions::maven_to_path;
use serde::Deserialize;
use std::path::Path;

/// One entry in `<uid>/<version>.json` (the per-version install
/// profile served by the Prism mirror).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LiteLoaderInstallProfile {
    id: String,
    #[serde(rename = "mainClass")]
    main_class: String,
    #[serde(default)]
    libraries: Vec<LiteLoaderLibrary>,
    /// LiteLoader's `LaunchWrapper` tweaker — has to be added to the
    /// JVM `--tweakClass` argument list at launch time. Stored here
    /// so `launch.rs` can pick it up later.
    #[serde(rename = "+tweakers", default)]
    tweakers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LiteLoaderLibrary {
    name: String,
    /// Optional Maven repository override. The Prism JSON sets
    /// `url: "http://dl.liteloader.com/versions/"` for the
    /// `com.mumfrey:liteloader:1.12.2-SNAPSHOT` artifact (which no
    /// longer resolves), and `url: "http://repo.liteloader.com/"`
    /// for `org.ow2.asm:asm-all:5.2`. The former is dead — see
    /// the install-path gate in `lib.rs::cmd_install_liteloader`.
    #[serde(default)]
    url: Option<String>,
    /// Prism hint. `"always-stale"` means "always re-download
    /// because the artifact's bytes change between releases even
    /// when the version string doesn't". Ignored by us — the
    /// launcher always re-checks the download URL.
    #[serde(rename = "MMC-hint", default)]
    #[allow(dead_code)]
    mmc_hint: Option<String>,
}

/// Fetch a page of available LiteLoader versions for a MC version.
///
/// All real LiteLoader releases target 1.12.x; passing a newer MC
/// version will simply yield an empty page. We delegate to
/// `prism_meta` so the fetch + error-logging + caching policy stays
/// uniform with the other loaders.
pub async fn get_loader_versions(
    mc_version: &str,
    offset: usize,
    limit: usize,
) -> Result<LoaderVersionPage> {
    super::prism_meta::fetch_loader_versions("com.mumfrey.liteloader", Some(mc_version), offset, limit).await
}

/// Get the LiteLoader launch profile for a given MC version +
/// loader version. Reads the per-version JSON from the Prism mirror
/// (`https://meta.prismlauncher.org/v1/com.mumfrey.liteloader/<version>.json`).
pub async fn get_profile(mc_version: &str, lite_version: &str) -> Result<LoaderProfile> {
    let client = crate::download::global_http_client();
    let url = format!(
        "https://meta.prismlauncher.org/v1/com.mumfrey.liteloader/{}.json",
        lite_version
    );

    let profile: LiteLoaderInstallProfile = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to fetch LiteLoader profile: {}", e)))?
        .json()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse LiteLoader profile: {}", e)))?;

    let libraries = profile
        .libraries
        .into_iter()
        .map(|lib| {
            let path = maven_to_path(&lib.name);
            let url = lib.url.unwrap_or_else(|| "https://repo.maven.apache.org/maven/".into());
            let base = if url.ends_with('/') { url } else { format!("{}/", url) };
            LoaderLibrary {
                name: lib.name,
                url: format!("{}{}", base, path),
                path,
                sha1: None,
                size: None,
            }
        })
        .collect();

    // The "+tweakers" array from Prism's JSON is added to the JVM
    // `--tweakClass` argument list at launch time. We expose it via
    // `jvm_args` so the existing `launch.rs` argument-flattening
    // picks it up without changes.
    let jvm_args = profile
        .tweakers
        .into_iter()
        .map(|t| format!("--tweakClass={}", t))
        .collect();

    // mc_version is kept in the signature for parity with the other
    // loaders' get_profile functions; LiteLoader doesn't need it.
    let _ = mc_version;

    Ok(LoaderProfile {
        main_class: profile.main_class,
        libraries,
        jvm_args,
        game_args: Vec::new(),
    })
}

/// Install LiteLoader — download all libraries and return profile.
///
/// LiteLoader is a legacy mod loader for MC 1.12.x and the upstream
/// download URLs (e.g. `dl.liteloader.com`) no longer resolve. We
/// still implement the install path so a user with a local mirror
/// can wire it up later, but the wizard gates this with a
/// user-visible "not supported" toast — see
/// `cmd_install_liteloader` in `lib.rs`.
pub async fn install(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
) -> Result<LoaderProfile> {
    let profile = get_profile(mc_version, loader_version).await?;

    for lib in &profile.libraries {
        let lib_path = libraries_dir.join(&lib.path);
        if !lib_path.exists() {
            crate::download::download_file(&lib.url, &lib_path, "").await?;
        }
    }

    Ok(profile)
}

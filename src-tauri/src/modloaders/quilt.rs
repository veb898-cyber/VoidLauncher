use serde::Deserialize;
use crate::error::{LauncherError, Result};
use crate::modloaders::{LoaderLibrary, LoaderProfile, LoaderVersionPage};
use crate::versions::maven_to_path;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QuiltLoaderVersion {
    separator: String,
    build: u32,
    maven: String,
    version: String,
    /// Quilt's loader-versions endpoint does NOT include a `stable` field
    /// (unlike Fabric's). Without `#[serde(default)]` the entire array
    /// fails to deserialize and `cmd_get_quilt_versions` returns an error,
    /// which the wizard silently swallows — leaving the Quilt version list
    /// empty. We treat every Quilt release as stable by default.
    #[serde(default)]
    stable: bool,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QuiltProfile {
    id: String,
    #[serde(rename = "mainClass")]
    main_class: String,
    libraries: Vec<QuiltLibrary>,
}

#[derive(Debug, Deserialize)]
struct QuiltLibrary {
    name: String,
    url: String,
}

/// Fetch a page of available Quilt loader versions.
///
/// We delegate to the Prism metadata mirror (`prism_meta`) rather than
/// hitting `meta.quiltmc.org` directly: the mirror is faster, more
/// reliable, and returns the same data in a uniform shape. Quilt
/// Loader versions are universal across MC versions (they only pin
/// `net.fabricmc.intermediary`, not a specific MC), so we pass `None`
/// for the MC filter.
///
/// The wizard drives this with infinite scroll: it asks for
/// `PAGE_SIZE` items, appends them, and asks for the next page at
/// `accumulator.length`. The underlying `prism_meta` call caches
/// the parsed index on first request so every page after the first
/// is instant.
///
/// On fetch/parse failure the underlying `prism_meta` call logs the
/// error and returns an empty page with `total = 0` — see
/// `prism_meta::fetch_loader_versions` for details.
pub async fn get_loader_versions(offset: usize, limit: usize) -> Result<LoaderVersionPage> {
    super::prism_meta::fetch_loader_versions("org.quiltmc.quilt-loader", None, offset, limit).await
}

/// Get Quilt profile for a specific MC version + loader version
pub async fn get_profile(mc_version: &str, loader_version: &str) -> Result<LoaderProfile> {
    let client = crate::download::global_http_client();
    let url = format!(
        "https://meta.quiltmc.org/v3/versions/loader/{}/{}/profile/json",
        mc_version, loader_version
    );

    let profile: QuiltProfile = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to fetch Quilt profile: {}", e)))?
        .json()
        .await
        .map_err(|e| LauncherError::ModLoader(format!("Failed to parse Quilt profile: {}", e)))?;

    let libraries = profile
        .libraries
        .into_iter()
        .map(|lib| {
            let path = maven_to_path(&lib.name);
            LoaderLibrary {
                name: lib.name,
                url: lib.url,
                path,
                sha1: None,
                size: None,
            }
        })
        .collect();

    Ok(LoaderProfile {
        main_class: profile.main_class,
        libraries,
        jvm_args: Vec::new(),
        game_args: Vec::new(),
    })
}

/// Install Quilt for an instance
pub async fn install(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &std::path::Path,
) -> Result<LoaderProfile> {
    let profile = get_profile(mc_version, loader_version).await?;

    for lib in &profile.libraries {
        let lib_path = libraries_dir.join(&lib.path);
        if !lib_path.exists() {
            let url = format!("{}{}", lib.url, lib.path);
            crate::download::download_file(&url, &lib_path, "").await?;
        }
    }

    Ok(profile)
}

use serde::Deserialize;
use crate::error::{LauncherError, Result};
use crate::modloaders::{LoaderLibrary, LoaderProfile, LoaderVersion};
use crate::versions::maven_to_path;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QuiltLoaderVersion {
    separator: String,
    build: u32,
    maven: String,
    version: String,
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

/// Fetch available Quilt loader versions
pub async fn get_loader_versions() -> Result<Vec<LoaderVersion>> {
    let client = crate::download::global_http_client();
    let versions: Vec<QuiltLoaderVersion> = client
        .get("https://meta.quiltmc.org/v3/versions/loader")
        .send()
        .await?
        .json()
        .await?;

    Ok(versions
        .into_iter()
        .map(|v| LoaderVersion {
            version: v.version,
            stable: true,
        })
        .collect())
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

use serde::{Deserialize, Serialize};
use crate::error::Result;

const BASE_URL: &str = "https://api.modrinth.com/v2";
const USER_AGENT: &str = "VoidLauncher/0.1.4 (github.com/voidlauncher)";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModrinthSearchResult {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub project_id: String,
    pub author: String,
    pub downloads: u64,
    pub icon_url: Option<String>,
    pub categories: Vec<String>,
    pub versions: Vec<String>,
    pub project_type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModrinthSearchResponse {
    pub hits: Vec<ModrinthSearchResult>,
    pub offset: u32,
    pub limit: u32,
    pub total_hits: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModrinthProject {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub body: Option<String>,
    pub id: String,
    pub downloads: u64,
    pub icon_url: Option<String>,
    pub categories: Vec<String>,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub versions: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModrinthVersion {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub files: Vec<ModrinthFile>,
    pub dependencies: Vec<ModrinthDependency>,
    pub date_published: String,
    pub downloads: u64,
    pub version_type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModrinthFile {
    pub hashes: std::collections::HashMap<String, String>,
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModrinthDependency {
    pub version_id: Option<String>,
    pub project_id: Option<String>,
    pub file_name: Option<String>,
    pub dependency_type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModrinthVersionResponse {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub files: Vec<ModrinthFile>,
    pub dependencies: Vec<ModrinthDependency>,
}

async fn api_get<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let client = crate::download::global_http_client();
    let response = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(crate::error::LauncherError::Download(
            format!("Modrinth API error ({}): {}", status, text)
        ));
    }

    let text = response.text().await.map_err(|e| {
        crate::error::LauncherError::Download(format!("Failed to read response body: {}", e))
    })?;

    serde_json::from_str::<T>(&text).map_err(|e| {
        crate::error::LauncherError::Download(format!(
            "Failed to decode Modrinth response: {}. Body preview: {}",
            e,
            &text.chars().take(500).collect::<String>()
        ))
    })
}

pub async fn search_mods(
    query: &str,
    project_type: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
    offset: u32,
    limit: u32,
) -> Result<ModrinthSearchResponse> {
    let mut facets = vec![format!(r#"["project_type:{}"]"#, project_type)];

    if let Some(v) = mc_version {
        facets.push(format!(r#"["versions:{}"]"#, v));
    }
    if project_type == "mod" {
        if let Some(l) = loader {
            if l.to_lowercase() != "vanilla" && !l.is_empty() {
                facets.push(format!(r#"["categories:{}"]"#, l.to_lowercase()));
            }
        }
    }

    let facets_str = format!("[{}]", facets.join(","));
    let url = format!(
        "{}/search?query={}&facets={}&index=relevance&limit={}&offset={}",
        BASE_URL,
        urlencoding::encode(query),
        urlencoding::encode(&facets_str),
        limit,
        offset
    );

    api_get(&url).await
}

pub async fn get_project(id: &str) -> Result<ModrinthProject> {
    let url = format!("{}/project/{}", BASE_URL, id);
    api_get(&url).await
}

pub async fn get_versions(
    project_id: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
) -> Result<Vec<ModrinthVersion>> {
    let mut params = Vec::new();
    if let Some(v) = mc_version {
        params.push(format!("game_versions={}", urlencoding::encode(&format!(r#"["{}"]"#, v))));
    }
    if let Some(l) = loader {
        params.push(format!("loaders={}", urlencoding::encode(&format!(r#"["{}"]"#, l.to_lowercase()))));
    }

    let query = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };

    let url = format!("{}/project/{}/version{}", BASE_URL, project_id, query);
    api_get(&url).await
}

pub async fn get_version_by_id(version_id: &str) -> Result<ModrinthVersionResponse> {
    let url = format!("{}/version/{}", BASE_URL, version_id);
    api_get(&url).await
}

/// Look up a version by file hash using Modrinth's /version_file/{hash} endpoint.
pub async fn get_version_by_hash(hash: &str, algorithm: &str) -> Result<ModrinthVersionResponse> {
    let url = format!("{}/version_file/{}?algorithm={}", BASE_URL, hash, algorithm);
    api_get(&url).await
}

/// Get all versions for a Modrinth project.
pub async fn get_project_versions(project_id: &str) -> Result<Vec<ModrinthVersionResponse>> {
    let url = format!("{}/project/{}/version", BASE_URL, project_id);
    api_get(&url).await
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VersionFilesUpdateRequest {
    pub hashes: Vec<String>,
    pub algorithm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaders: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_versions: Option<Vec<String>>,
}

/// Check for updates by sending file hashes to Modrinth's /version_files/update endpoint.
/// Returns a map from hash to the latest version (or null if already up-to-date).
pub async fn check_version_updates(
    hashes: Vec<String>,
    algorithm: &str,
    loaders: Option<Vec<String>>,
    game_versions: Option<Vec<String>>,
) -> Result<std::collections::HashMap<String, Option<ModrinthVersion>>> {
    let client = crate::download::global_http_client();
    let body = VersionFilesUpdateRequest {
        hashes,
        algorithm: algorithm.to_string(),
        loaders,
        game_versions,
    };
    let response = client
        .post(format!("{}/version_files/update", BASE_URL))
        .header("User-Agent", USER_AGENT)
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(crate::error::LauncherError::Download(
            format!("Modrinth update API error ({}): {}", status, text)
        ));
    }
    let text = response.text().await.map_err(|e| {
        crate::error::LauncherError::Download(format!("Failed to read update response: {}", e))
    })?;
    serde_json::from_str::<std::collections::HashMap<String, Option<ModrinthVersion>>>(&text).map_err(|e| {
        crate::error::LauncherError::Download(format!(
            "Failed to decode update response: {}. Body preview: {}",
            e,
            &text.chars().take(500).collect::<String>()
        ))
    })
}

pub async fn popular_mods(
    project_type: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<ModrinthSearchResponse> {
    let mut facets = vec![format!(r#"["project_type:{}"]"#, project_type)];

    if let Some(v) = mc_version {
        facets.push(format!(r#"["versions:{}"]"#, v));
    }
    if project_type == "mod" {
        if let Some(l) = loader {
            if l.to_lowercase() != "vanilla" && !l.is_empty() {
                facets.push(format!(r#"["categories:{}"]"#, l.to_lowercase()));
            }
        }
    }

    let facets_str = format!("[{}]", facets.join(","));
    let url = format!(
        "{}/search?query=&facets={}&index=downloads&limit={}&offset={}",
        BASE_URL,
        urlencoding::encode(&facets_str),
        limit,
        offset,
    );

    api_get(&url).await
}

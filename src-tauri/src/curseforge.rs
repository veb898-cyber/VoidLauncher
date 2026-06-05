use serde::{Deserialize, Serialize};
use crate::error::Result;

const BASE_URL: &str = "https://api.curseforge.com";
const MINECRAFT_GAME_ID: u32 = 432;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfSearchResult {
    pub id: u64,
    pub name: String,
    pub slug: String,
    pub summary: String,
    #[serde(rename = "downloadCount")]
    pub download_count: u64,
    pub logo: Option<CfLogo>,
    pub authors: Vec<CfAuthor>,
    #[serde(rename = "dateModified")]
    pub date_modified: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfLogo {
    #[serde(rename = "thumbnailUrl")]
    pub thumbnail_url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfAuthor {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfSearchResponse {
    pub data: Vec<CfSearchResult>,
    pub pagination: CfPagination,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfPagination {
    pub index: u32,
    #[serde(rename = "pageSize")]
    pub page_size: u32,
    #[serde(rename = "resultCount")]
    pub result_count: u32,
    #[serde(rename = "totalCount")]
    pub total_count: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfDataResponse<T> {
    pub data: T,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfModDetail {
    pub id: u64,
    pub name: String,
    pub slug: String,
    pub summary: String,
    pub description: Option<String>,
    pub logo: Option<CfLogo>,
    pub authors: Vec<CfAuthor>,
    pub links: Option<CfLinks>,
    pub categories: Vec<CfCategory>,
    #[serde(rename = "downloadCount")]
    pub download_count: u64,
    #[serde(rename = "dateModified")]
    pub date_modified: String,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfCategory {
    pub id: u64,
    pub name: String,
    pub slug: String,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfLinks {
    #[serde(rename = "websiteUrl")]
    pub website_url: Option<String>,
    #[serde(rename = "wikiUrl")]
    pub wiki_url: Option<String>,
    #[serde(rename = "sourceUrl")]
    pub source_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfFile {
    pub id: u64,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "downloadUrl")]
    pub download_url: Option<String>,
    #[serde(rename = "fileLength")]
    pub file_length: u64,
    #[serde(rename = "fileDate")]
    pub file_date: String,
    #[serde(rename = "gameVersions")]
    pub game_versions: Vec<String>,
    pub dependencies: Vec<CfDependency>,
    #[serde(rename = "releaseType")]
    pub release_type: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfDependency {
    #[serde(rename = "modId")]
    pub mod_id: u64,
    #[serde(rename = "relationType")]
    pub relation_type: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfFilesResponse {
    pub data: Vec<CfFile>,
    pub pagination: CfPagination,
}

// CurseForge ModLoaderType enum values
pub fn loader_type_id(loader: &str) -> Option<u32> {
    match loader.to_lowercase().as_str() {
        "forge" => Some(1),
        "fabric" => Some(4),
        "quilt" => Some(5),
        "neoforge" => Some(6),
        _ => None,
    }
}

async fn api_get<T: serde::de::DeserializeOwned>(url: &str, api_key: &str) -> Result<T> {
    let client = crate::download::global_http_client();
    let response = client
        .get(url)
        .header("x-api-key", api_key)
        .header("Accept", "application/json")
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(crate::error::LauncherError::Download(
            format!("CurseForge API error ({}): {}", status, text)
        ));
    }

    let text = response.text().await.map_err(|e| {
        crate::error::LauncherError::Download(format!("Failed to read response body: {}", e))
    })?;

    serde_json::from_str::<T>(&text).map_err(|e| {
        crate::error::LauncherError::Download(format!(
            "Failed to decode CurseForge response: {}. Body preview: {}",
            e,
            &text.chars().take(500).collect::<String>()
        ))
    })
}

pub async fn search_mods(
    query: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
    offset: u32,
    limit: u32,
    api_key: &str,
) -> Result<CfSearchResponse> {
    let mut params = vec![
        format!("gameId={}", MINECRAFT_GAME_ID),
        format!("searchFilter={}", urlencoding::encode(query)),
        format!("pageSize={}", limit.min(50)),
        format!("index={}", offset),
        "sortField=2".to_string(),
        "sortOrder=desc".to_string(),
    ];

    if let Some(v) = mc_version {
        params.push(format!("gameVersion={}", urlencoding::encode(v)));
    }
    if let Some(l) = loader {
        if let Some(id) = loader_type_id(l) {
            params.push(format!("modLoaderType={}", id));
        }
    }

    let url = format!("{}/v1/mods/search?{}", BASE_URL, params.join("&"));
    api_get(&url, api_key).await
}

pub async fn get_mod(mod_id: u64, api_key: &str) -> Result<CfModDetail> {
    let url = format!("{}/v1/mods/{}", BASE_URL, mod_id);
    let resp: CfDataResponse<CfModDetail> = api_get(&url, api_key).await?;
    Ok(resp.data)
}

pub async fn get_mod_files(
    mod_id: u64,
    mc_version: Option<&str>,
    loader: Option<&str>,
    api_key: &str,
) -> Result<CfFilesResponse> {
    let mut params = vec![
        format!("pageSize={}", 50),
    ];

    if let Some(v) = mc_version {
        params.push(format!("gameVersion={}", urlencoding::encode(v)));
    }
    if let Some(l) = loader {
        if let Some(id) = loader_type_id(l) {
            params.push(format!("modLoaderType={}", id));
        }
    }

    let url = format!("{}/v1/mods/{}/files?{}", BASE_URL, mod_id, params.join("&"));
    api_get(&url, api_key).await
}

pub async fn popular_mods(
    mc_version: Option<&str>,
    loader: Option<&str>,
    limit: u32,
    api_key: &str,
) -> Result<CfSearchResponse> {
    let mut params = vec![
        format!("gameId={}", MINECRAFT_GAME_ID),
        format!("searchFilter="),
        format!("pageSize={}", limit.min(50)),
        format!("index=0"),
        "sortField=2".to_string(),
        "sortOrder=desc".to_string(),
    ];

    if let Some(v) = mc_version {
        params.push(format!("gameVersion={}", urlencoding::encode(v)));
    }
    if let Some(l) = loader {
        if let Some(id) = loader_type_id(l) {
            params.push(format!("modLoaderType={}", id));
        }
    }

    let url = format!("{}/v1/mods/search?{}", BASE_URL, params.join("&"));
    api_get(&url, api_key).await
}

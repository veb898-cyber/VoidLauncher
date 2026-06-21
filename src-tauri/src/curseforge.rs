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
pub struct CfHash {
    pub value: String,
    pub algo: u32,
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
    #[serde(default)]
    pub hashes: Vec<CfHash>,
}

impl CfFile {
    pub fn sha1_hash(&self) -> Option<&str> {
        self.hashes
            .iter()
            .find(|h| h.algo == 1)
            .map(|h| h.value.as_str())
    }
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

pub async fn get_mod_file(mod_id: u64, file_id: u64, api_key: &str) -> Result<CfFile> {
    let url = format!("{}/v1/mods/{}/files/{}", BASE_URL, mod_id, file_id);
    let resp: CfDataResponse<CfFile> = api_get(&url, api_key).await?;
    Ok(resp.data)
}

/// Official signed download URL from the CurseForge API (most reliable).
pub async fn get_mod_file_download_url(mod_id: u64, file_id: u64, api_key: &str) -> Result<String> {
    let url = format!("{}/v1/mods/{}/files/{}/download-url", BASE_URL, mod_id, file_id);
    let resp: CfDataResponse<String> = api_get(&url, api_key).await?;
    Ok(resp.data)
}

fn cf_cdn_urls(file_id: u64, file_name: &str) -> Vec<String> {
    let prefix = file_id / 1000;
    let suffix = file_id % 1000;
    let encoded = urlencoding::encode(file_name);
    let mut urls = Vec::new();
    for host in ["edge.forgecdn.net", "mediafilez.forgecdn.net", "media.forgecdn.net"] {
        urls.push(format!(
            "https://{}/files/{}/{}/{}",
            host, prefix, suffix, file_name
        ));
        if encoded != file_name {
            urls.push(format!(
                "https://{}/files/{}/{}/{}",
                host, prefix, suffix, encoded
            ));
        }
    }
    urls
}

/// Download a CurseForge mod file, trying the signed API URL then CDN fallbacks.
pub async fn download_mod_file(
    mod_id: u64,
    file_id: u64,
    cf_file: &CfFile,
    api_key: &str,
    dest_path: &std::path::Path,
) -> Result<()> {
    let mut urls: Vec<String> = Vec::new();

    if let Ok(signed) = get_mod_file_download_url(mod_id, file_id, api_key).await {
        if !signed.is_empty() {
            urls.push(signed);
        }
    }

    if let Some(u) = cf_file.download_url.as_ref().filter(|u| !u.is_empty()) {
        if !urls.iter().any(|x| x == u) {
            urls.push(u.clone());
        }
    }

    for cdn in cf_cdn_urls(file_id, &cf_file.file_name) {
        if !urls.iter().any(|x| x == &cdn) {
            urls.push(cdn);
        }
    }

    if urls.is_empty() {
        return Err(crate::error::LauncherError::Download(format!(
            "No download URL for {} ({})",
            cf_file.display_name, cf_file.file_name
        )));
    }

    let dest_buf = dest_path.to_path_buf();
    let expected_sha1 = cf_file.sha1_hash().unwrap_or("");
    let mut last_err = String::new();

    for url in urls {
        match crate::download::download_file_sized(
            &url,
            &dest_buf,
            expected_sha1,
            cf_file.file_length,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e.to_string(),
        }
    }

    Err(crate::error::LauncherError::Download(format!(
        "Failed to download {} ({}): {}",
        cf_file.display_name, cf_file.file_name, last_err
    )))
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

use crate::error::Result;
use crate::integrity::HashSpec;
use serde::{Deserialize, Serialize};

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
    /// Small square preview. Null/empty for some older packs; `url` then
    /// carries the full-size image and is used as a fallback.
    #[serde(rename = "thumbnailUrl")]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

impl CfLogo {
    /// Best available image URL (thumbnail preferred, full image as fallback).
    pub fn best_url(&self) -> Option<&str> {
        self.thumbnail_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .or(self.url.as_deref().filter(|u| !u.is_empty()))
    }
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
    /// CurseForge project class (`6` mod, `12` resource pack, `6552` shader).
    /// Absent/unknown must never be treated as a mod by import/repair.
    #[serde(rename = "classId", default)]
    pub class_id: Option<u32>,
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
    /// Project id the file belongs to. `default` keeps old fixtures/cached
    /// payloads parseable; content installation requires an exact match.
    #[serde(rename = "modId", default)]
    pub mod_id: u64,
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
    /// `false` marks deleted/broken files the API still lists; they must
    /// never be offered as versions nor downloaded. Absent means available.
    #[serde(rename = "isAvailable")]
    pub is_available: Option<bool>,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfFileIndex {
    #[serde(rename = "gameVersion", default)]
    pub game_version: String,
    #[serde(rename = "fileId", default)]
    pub file_id: u64,
    #[serde(default)]
    pub filename: String,
    #[serde(rename = "releaseType", default)]
    pub release_type: u32,
    #[serde(rename = "gameVersionTypeId", default)]
    pub game_version_type_id: Option<u32>,
    #[serde(rename = "modLoader", default)]
    pub mod_loader: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CfBulkMod {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    /// Project class from the bulk metadata endpoint. Missing means the API
    /// did not resolve the project; callers must report it instead of guessing.
    #[serde(rename = "classId", default)]
    pub class_id: Option<u32>,
    #[serde(rename = "latestFiles", default)]
    pub latest_files: Vec<CfFile>,
    #[serde(rename = "latestFilesIndexes", default)]
    pub latest_files_indexes: Vec<CfFileIndex>,
}

// CurseForge ModLoaderType enum values
pub fn loader_type_id(loader: &str) -> Option<u32> {
    match loader.to_lowercase().as_str() {
        "forge" => Some(1),
        "fabric" => Some(4),
        "neoforge" => Some(6),
        _ => None,
    }
}

/// Content classes VoidLauncher can install from a CurseForge modpack.
/// Unknown classes are deliberately not mapped to `mods/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfContentClass {
    Mod,
    ResourcePack,
    ShaderPack,
}

impl CfContentClass {
    pub fn from_class_id(class_id: u32) -> Option<Self> {
        match class_id {
            6 => Some(Self::Mod),
            12 => Some(Self::ResourcePack),
            6552 => Some(Self::ShaderPack),
            _ => None,
        }
    }

    pub fn folder(self) -> &'static str {
        match self {
            Self::Mod => "mods",
            Self::ResourcePack => "resourcepacks",
            Self::ShaderPack => "shaderpacks",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Mod => "mod",
            Self::ResourcePack => "resourcepack",
            Self::ShaderPack => "shader",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mod => "mod",
            Self::ResourcePack => "resource pack",
            Self::ShaderPack => "shader pack",
        }
    }
}

/// Resolve a CurseForge project class from bulk project metadata. A missing
/// entry is not evidence that the project is a mod.
pub fn content_class_from_bulk(projects: &[CfBulkMod], project_id: u64) -> Option<CfContentClass> {
    projects
        .iter()
        .find(|project| project.id == project_id)
        .and_then(|project| project.class_id)
        .and_then(CfContentClass::from_class_id)
}

/// CurseForge classId for a VoidLauncher project type (verified against
/// `GET /v1/categories?gameId=432`: mods=6, resourcepacks=12,
/// shaders=6552, modpacks=4471). Unknown types return `None` — the search
/// then spans all classes instead of silently forcing "mods".
pub fn class_id_for_project_type(project_type: &str) -> Option<u32> {
    match project_type.trim().to_ascii_lowercase().as_str() {
        "mod" | "mods" => Some(6),
        "resourcepack" | "resourcepacks" | "resource-pack" | "resource-packs" => Some(12),
        "shader" | "shaders" | "shaderpack" | "shaderpacks" => Some(6552),
        "modpack" | "modpacks" => Some(4471),
        "world" | "worlds" => Some(17),
        "datapack" | "datapacks" => Some(6945),
        _ => None,
    }
}

/// Map the frontend sort selection onto CF `sortField`/`sortOrder`:
/// downloads → total downloads (6) desc, newest/oldest → created date (11).
/// The official enum is 1 Featured, 2 Popularity, 3 LastUpdated,
/// 6 TotalDownloads, 11 ReleasedDate. Field 11 is the project's creation
/// date (the API orders by `dateCreated`), which is what "newest/oldest"
/// means for a catalog; field 3 would sort by the last file update instead,
/// pushing old projects to the top whenever they receive a new version.
/// `sortField=11 asc` works, so no last-page emulation is needed for CF.
pub fn sort_field_for(index: Option<&str>) -> (u32, &'static str) {
    match index.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("newest") => (11, "desc"),
        Some("oldest") => (11, "asc"),
        _ => (6, "desc"),
    }
}

/// Newest-first ordering + availability filter shared by every file list:
/// the raw API order is not guaranteed and deleted files must never surface.
pub fn sort_files_newest_first(files: &mut Vec<CfFile>) {
    files.retain(|f| f.is_available.unwrap_or(true));
    files.sort_by(|a, b| b.file_date.cmp(&a.file_date));
}

/// Host of an API URL. Authenticated requests must carry their credential to
/// this exact host only; redirects are otherwise refused.
fn api_host(url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| {
        crate::error::LauncherError::Download(format!("Invalid CurseForge URL: {e}"))
    })?;
    parsed
        .host_str()
        .map(|host| host.to_ascii_lowercase())
        .ok_or_else(|| {
            crate::error::LauncherError::Download("CurseForge URL has no host".to_string())
        })
}

async fn api_get<T: serde::de::DeserializeOwned>(url: &str, api_key: &str) -> Result<T> {
    crate::download::ensure_proxy_resolved().await;
    let host = api_host(url)?;
    let client = crate::download::http_client_for_exact_host(&host)?;
    let response = crate::download::get_with_retry_same_host(
        client
            .get(url)
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            // API replies are small; a 120s default timeout would let a
            // silent connection block the whole install for minutes.
            .timeout(std::time::Duration::from_secs(15)),
        "CurseForge",
        4,
        &[500, 1000, 2000],
        &host,
    )
    .await?;

    let text = response
        .text()
        .await
        .map_err(crate::error::LauncherError::Network)?;
    serde_json::from_str::<T>(&text).map_err(|e| {
        crate::error::LauncherError::Download(format!(
            "Failed to decode CurseForge response: {}. Body preview: {}",
            e,
            text.chars().take(500).collect::<String>()
        ))
    })
}

/// Single-attempt API request with a short timeout. Used for optional calls
/// like the signed download URL, where a fallback (CDN URL) exists — no need
/// to burn 4 retries on a stalled connection.
async fn api_get_once<T: serde::de::DeserializeOwned>(
    url: &str,
    api_key: &str,
    timeout_secs: u64,
) -> Result<T> {
    let host = api_host(url)?;
    let client = crate::download::http_client_for_exact_host(&host)?;
    let response = crate::download::send_with_fallback_same_host(
        client
            .get(url)
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(timeout_secs)),
        &host,
    )
    .await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(crate::error::LauncherError::Download(format!(
            "CurseForge API error ({}): {}",
            status, text
        )));
    }
    let text = response.text().await?;
    serde_json::from_str::<T>(&text).map_err(|e| {
        crate::error::LauncherError::Download(format!(
            "Failed to decode CurseForge response: {}",
            e
        ))
    })
}

async fn api_post<T: serde::de::DeserializeOwned, B: serde::Serialize + ?Sized>(
    url: &str,
    api_key: &str,
    body: &B,
) -> Result<T> {
    crate::download::ensure_proxy_resolved().await;
    let host = api_host(url)?;
    let client = crate::download::http_client_for_exact_host(&host)?;
    let response = crate::download::get_with_retry_same_host(
        client
            .post(url)
            .header("x-api-key", api_key)
            .header("Accept", "application/json")
            .json(body)
            .timeout(std::time::Duration::from_secs(15)),
        "CurseForge",
        2,
        &[500],
        &host,
    )
    .await?;

    let text = response
        .text()
        .await
        .map_err(crate::error::LauncherError::Network)?;
    serde_json::from_str::<T>(&text).map_err(|e| {
        crate::error::LauncherError::Download(format!(
            "Failed to decode CurseForge response: {}. Body preview: {}",
            e,
            text.chars().take(500).collect::<String>()
        ))
    })
}

/// Filter set for [`search_mods_filtered`]. `index` selects the sort order
/// (`downloads`/`newest`/`oldest`, see [`sort_field_for`]).
pub struct CfSearchQuery<'a> {
    pub query: &'a str,
    pub mc_version: Option<&'a str>,
    pub loader: Option<&'a str>,
    pub class_id: Option<u32>,
    pub category_id: Option<u32>,
    pub index: Option<&'a str>,
    pub offset: u32,
    pub limit: u32,
}

/// Constructs the search query URL for CurseForge API.
///
/// For categories that do not use mod loaders (such as Resource Packs [class 12],
/// Shaders [class 6552], Worlds [class 17], Data Packs [class 6945]), the `modLoaderType`
/// filter is omitted even if provided, because CurseForge will return zero results
/// when `modLoaderType` is applied to non-mod categories.
pub fn build_search_url(query: &CfSearchQuery<'_>) -> String {
    let CfSearchQuery {
        query,
        mc_version,
        loader,
        class_id,
        category_id,
        index,
        offset,
        limit,
    } = *query;
    let (sort_field, sort_order) = sort_field_for(index);
    let mut params = vec![
        format!("gameId={}", MINECRAFT_GAME_ID),
        format!("searchFilter={}", urlencoding::encode(query)),
        format!("pageSize={}", limit.min(50)),
        format!("index={}", offset),
        format!("sortField={}", sort_field),
        format!("sortOrder={}", sort_order),
    ];

    if let Some(c) = class_id {
        params.push(format!("classId={}", c));
    }
    if let Some(c) = category_id {
        params.push(format!("categoryId={}", c));
    }

    if let Some(v) = mc_version {
        params.push(format!("gameVersion={}", urlencoding::encode(v)));
    }

    // Resource packs (12), shaders (6552), worlds (17), datapacks (6945) do not have a mod loader.
    // Specifying modLoaderType for these classes causes CurseForge API to return 0 hits.
    let effective_loader = match class_id {
        Some(12) | Some(6552) | Some(17) | Some(6945) => None,
        _ => loader,
    };

    if let Some(l) = effective_loader {
        if let Some(id) = loader_type_id(l) {
            params.push(format!("modLoaderType={}", id));
        }
    }

    format!("{}/v1/mods/search?{}", BASE_URL, params.join("&"))
}

/// Search with optional classId (e.g. 4471 = modpacks), categoryId and sort.
pub async fn search_mods_filtered(
    query: &CfSearchQuery<'_>,
    api_key: &str,
) -> Result<CfSearchResponse> {
    let url = build_search_url(query);
    api_get(&url, api_key).await
}

pub async fn get_mod(mod_id: u64, api_key: &str) -> Result<CfModDetail> {
    let url = format!("{}/v1/mods/{}", BASE_URL, mod_id);
    let resp: CfDataResponse<CfModDetail> = api_get(&url, api_key).await?;
    Ok(resp.data)
}

/// File list for a project, newest-first and with unavailable files
/// filtered out (see [`sort_files_newest_first`]). `offset` pages the list
/// for the version browser (the API caps `pageSize` at 50).
pub async fn get_mod_files(
    mod_id: u64,
    mc_version: Option<&str>,
    loader: Option<&str>,
    offset: u32,
    api_key: &str,
) -> Result<CfFilesResponse> {
    let mut params = vec![format!("pageSize={}", 50), format!("index={}", offset)];

    if let Some(v) = mc_version {
        params.push(format!("gameVersion={}", urlencoding::encode(v)));
    }
    if let Some(l) = loader {
        if let Some(id) = loader_type_id(l) {
            params.push(format!("modLoaderType={}", id));
        }
    }

    let url = format!("{}/v1/mods/{}/files?{}", BASE_URL, mod_id, params.join("&"));
    let mut resp: CfFilesResponse = api_get(&url, api_key).await?;
    sort_files_newest_first(&mut resp.data);
    Ok(resp)
}

pub async fn get_mod_file(mod_id: u64, file_id: u64, api_key: &str) -> Result<CfFile> {
    let url = format!("{}/v1/mods/{}/files/{}", BASE_URL, mod_id, file_id);
    let resp: CfDataResponse<CfFile> = api_get(&url, api_key).await?;
    Ok(resp.data)
}

pub async fn get_mods_by_ids(mod_ids: &[u64], api_key: &str) -> Result<Vec<CfBulkMod>> {
    if mod_ids.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("{}/v1/mods", BASE_URL);
    let body = serde_json::json!({ "modIds": mod_ids });
    let resp: CfDataResponse<Vec<CfBulkMod>> = api_post(&url, api_key, &body).await?;
    Ok(resp.data)
}

/// Bulk metadata in bounded batches. CurseForge rejects very large id arrays,
/// and one failed batch must not silently downgrade every project to "mod".
pub async fn get_mods_by_ids_chunked(mod_ids: &[u64], api_key: &str) -> Result<Vec<CfBulkMod>> {
    let mut out = Vec::new();
    for chunk in mod_ids.chunks(100) {
        out.extend(get_mods_by_ids(chunk, api_key).await?);
    }
    Ok(out)
}

pub async fn get_files_by_ids(file_ids: &[u64], api_key: &str) -> Result<Vec<CfFile>> {
    if file_ids.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("{}/v1/mods/files", BASE_URL);
    let body = serde_json::json!({ "fileIds": file_ids });
    let resp: CfDataResponse<Vec<CfFile>> = api_post(&url, api_key, &body).await?;
    Ok(resp.data)
}

/// Official signed download URL from the CurseForge API (most reliable).
/// Single fast attempt — on failure the installer falls back to CDN URLs.
pub async fn get_mod_file_download_url(mod_id: u64, file_id: u64, api_key: &str) -> Result<String> {
    let url = format!(
        "{}/v1/mods/{}/files/{}/download-url",
        BASE_URL, mod_id, file_id
    );
    let resp: CfDataResponse<String> = api_get_once(&url, api_key, 8).await?;
    Ok(resp.data)
}

pub(crate) fn cf_cdn_urls(file_id: u64, file_name: &str) -> Vec<String> {
    let prefix = file_id / 1000;
    let suffix = file_id % 1000;
    let encoded = urlencoding::encode(file_name);
    let mut urls = Vec::new();
    for host in [
        "edge.forgecdn.net",
        "mediafilez.forgecdn.net",
        "media.forgecdn.net",
    ] {
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

/// Validate the API metadata before a CurseForge file is downloaded.
///
/// The caller already fetched `CfFile` by `(mod_id, file_id)`, but a stale or
/// mismatched cache/fixture must not be able to swap the project, file or hash.
/// Only `algo == 1` is SHA-1; MD5 and unknown algorithms are ignored.
fn validate_cf_download_metadata(cf_file: &CfFile, mod_id: u64, file_id: u64) -> Result<HashSpec> {
    if mod_id == 0 || file_id == 0 {
        return Err(crate::error::LauncherError::Download(
            "Invalid CurseForge project or file id".to_string(),
        ));
    }
    if cf_file.id != file_id {
        return Err(crate::error::LauncherError::Download(format!(
            "CurseForge metadata is for file {}, not the requested {file_id}",
            cf_file.id
        )));
    }
    if cf_file.mod_id != mod_id {
        return Err(crate::error::LauncherError::Download(format!(
            "CurseForge file {} does not belong to project {mod_id}",
            cf_file.id
        )));
    }
    if cf_file.is_available == Some(false) {
        return Err(crate::error::LauncherError::Download(format!(
            "CurseForge file {} is not available",
            cf_file.id
        )));
    }
    if cf_file.file_name.trim().is_empty() {
        return Err(crate::error::LauncherError::Download(format!(
            "CurseForge file {file_id} has no file name"
        )));
    }
    let sha1 = cf_file.sha1_hash().ok_or_else(|| {
        crate::error::LauncherError::Download(format!(
            "CurseForge file {} ({}) has no SHA-1 checksum; refusing to download it",
            cf_file.id, cf_file.file_name
        ))
    })?;
    HashSpec::sha1(sha1)
}

/// Download a CurseForge mod file, trying the signed API URL then CDN fallbacks.
pub async fn download_mod_file(
    mod_id: u64,
    file_id: u64,
    cf_file: &CfFile,
    api_key: &str,
    dest_path: &std::path::Path,
) -> Result<()> {
    // Validate and resolve the publisher checksum before any network request.
    let expected = validate_cf_download_metadata(cf_file, mod_id, file_id)?;
    let mut urls = download_url_candidates(mod_id, file_id, &cf_file.file_name, api_key).await;
    if let Some(u) = cf_file.download_url.as_ref().filter(|u| !u.is_empty()) {
        if !urls.iter().any(|x| x == u) {
            // Official API link — right after the signed URL, before the mirrors.
            let at = if api_key.is_empty() {
                0
            } else {
                1.min(urls.len())
            };
            urls.insert(at, u.clone());
        }
    }

    if urls.is_empty() {
        return Err(crate::error::LauncherError::Download(format!(
            "No download URL for {} ({})",
            cf_file.display_name, cf_file.file_name
        )));
    }

    let dest_buf = dest_path.to_path_buf();
    let mut last_err = String::new();

    for url in urls {
        let result = if cf_file.file_length > 0 {
            crate::download::download_file_sized_verified(
                &url,
                &dest_buf,
                &expected,
                cf_file.file_length,
            )
            .await
        } else {
            crate::download::download_file_verified(&url, &dest_buf, &expected).await
        };
        match result {
            Ok(()) => return Ok(()),
            // Pause stops the whole install — never fall to the next mirror.
            Err(e @ crate::error::LauncherError::Paused) => return Err(e),
            Err(e) => last_err = e.to_string(),
        }
    }

    Err(crate::error::LauncherError::Download(format!(
        "Failed to download {} ({}): {}",
        cf_file.display_name, cf_file.file_name, last_err
    )))
}

/// Ordered download candidates for a CF file: the official signed URL
/// first (needs an API key), then the CDN URL pattern derived from the
/// file id. The CDN candidates never need a key, so the list is non-empty
/// for any valid file even without configured credentials.
pub async fn download_url_candidates(
    mod_id: u64,
    file_id: u64,
    file_name: &str,
    api_key: &str,
) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    if !api_key.is_empty() {
        if let Ok(signed) = get_mod_file_download_url(mod_id, file_id, api_key).await {
            if signed.starts_with("https://") && !urls.contains(&signed) {
                urls.push(signed);
            }
        }
    }
    for cdn in cf_cdn_urls(file_id, file_name) {
        if !urls.contains(&cdn) {
            urls.push(cdn);
        }
    }
    urls
}

/// Download a CurseForge file from (project_id, file_id) without needing a
/// direct URL up front — the plan for CF content carries an empty `url`,
/// and this resolves the signed/CDN chain at download time. Candidates are
/// tried in order; the resume `.part` of a failed candidate is deleted
/// before the next host is tried so two different bodies never mix.
pub async fn download_cf_native(
    mod_id: u64,
    file_id: u64,
    file_name: &str,
    expected: &HashSpec,
    expected_size: Option<u64>,
    dest: &std::path::Path,
    api_key: &str,
) -> Result<()> {
    // Already on disk with the wanted hash (interrupted-apply resume).
    if dest.exists() && expected.verify_file(dest)? {
        return Ok(());
    }
    let candidates = download_url_candidates(mod_id, file_id, file_name, api_key).await;
    if candidates.is_empty() {
        return Err(crate::error::LauncherError::Download(format!(
            "No download URL for CurseForge file {} ({})",
            file_id, file_name
        )));
    }
    let mut last_err = String::new();
    for (i, url) in candidates.iter().enumerate() {
        if i > 0 {
            let mut part = dest.as_os_str().to_os_string();
            part.push(".part");
            let part = std::path::PathBuf::from(part);
            let _ = std::fs::remove_file(&part);
            // The identity sidecar belongs to the previous host's URL; leaving
            // it would make the next attempt look like a resume of a different
            // artifact (harmless, but noisy and confusing in logs).
            let mut meta = dest.as_os_str().to_os_string();
            meta.push(".part.meta");
            let _ = std::fs::remove_file(std::path::PathBuf::from(meta));
        }
        let timeout = expected_size
            .map(crate::download::timeout_for_size)
            .unwrap_or_else(crate::download::timeout_for_unknown_size);
        match crate::download::download_to_part_verified(
            url,
            dest,
            expected,
            expected_size,
            Some(timeout),
        )
        .await
        {
            Ok(()) => return Ok(()),
            // A paused install must stop immediately, not fall through to
            // another mirror (the global pause flag applies to all of them).
            Err(e @ crate::error::LauncherError::Paused) => return Err(e),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(crate::error::LauncherError::Download(format!(
        "Failed to download {} ({}): {}",
        file_name, file_id, last_err
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
        "searchFilter=".to_string(),
        format!("pageSize={}", limit.min(50)),
        "index=0".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cf_file(id: u64, name: &str, date: &str, available: Option<bool>) -> CfFile {
        CfFile {
            id,
            mod_id: 0,
            display_name: name.to_string(),
            file_name: format!("{}.jar", name),
            download_url: None,
            file_length: 1,
            file_date: date.to_string(),
            game_versions: vec![],
            dependencies: vec![],
            release_type: 1,
            hashes: vec![],
            is_available: available,
        }
    }

    fn cf_download_file() -> CfFile {
        let mut file = cf_file(42, "mod", "2026-01-01T00:00:00Z", Some(true));
        file.mod_id = 7;
        file.hashes = vec![CfHash {
            value: "a".repeat(40),
            algo: 1,
        }];
        file
    }

    #[test]
    fn cf_download_metadata_accepts_matching_algo1_hash() {
        let file = cf_download_file();
        let expected = validate_cf_download_metadata(&file, 7, 42).unwrap();
        assert_eq!(expected.algorithm(), "sha1");
        assert_eq!(expected.hex(), "a".repeat(40));
    }

    #[test]
    fn cf_download_metadata_rejects_identity_and_availability_errors() {
        let file = cf_download_file();
        assert!(validate_cf_download_metadata(&file, 8, 42).is_err());
        assert!(validate_cf_download_metadata(&file, 7, 43).is_err());

        let unavailable = CfFile {
            is_available: Some(false),
            ..cf_download_file()
        };
        assert!(validate_cf_download_metadata(&unavailable, 7, 42).is_err());

        let mut unnamed = cf_download_file();
        unnamed.file_name.clear();
        assert!(validate_cf_download_metadata(&unnamed, 7, 42).is_err());
    }

    #[test]
    fn cf_download_metadata_rejects_missing_or_unknown_sha1() {
        let mut missing = cf_download_file();
        missing.hashes.clear();
        assert!(validate_cf_download_metadata(&missing, 7, 42).is_err());

        let mut unknown = cf_download_file();
        unknown.hashes = vec![CfHash {
            value: "b".repeat(40),
            algo: 2,
        }];
        assert!(validate_cf_download_metadata(&unknown, 7, 42).is_err());

        let mut corrupt = cf_download_file();
        corrupt.hashes[0].value = "not-a-sha1".to_string();
        assert!(validate_cf_download_metadata(&corrupt, 7, 42).is_err());
    }

    #[test]
    fn build_search_url_omits_mod_loader_for_resourcepacks_and_shaders() {
        // Class 6 (mods): modLoaderType should be included
        let mod_q = CfSearchQuery {
            query: "sodium",
            mc_version: Some("1.21.1"),
            loader: Some("fabric"),
            class_id: Some(6),
            category_id: None,
            index: None,
            offset: 0,
            limit: 20,
        };
        let mod_url = build_search_url(&mod_q);
        assert!(
            mod_url.contains("modLoaderType=4"),
            "Mods must have modLoaderType"
        );
        assert!(mod_url.contains("classId=6"));

        // Class 12 (resourcepack): modLoaderType must be omitted even if loader is specified
        let rp_q = CfSearchQuery {
            query: "faithful",
            mc_version: Some("1.21.1"),
            loader: Some("fabric"),
            class_id: Some(12),
            category_id: None,
            index: None,
            offset: 0,
            limit: 20,
        };
        let rp_url = build_search_url(&rp_q);
        assert!(
            !rp_url.contains("modLoaderType"),
            "Resource packs must NOT include modLoaderType"
        );
        assert!(rp_url.contains("classId=12"));

        // Class 6552 (shader): modLoaderType must be omitted
        let shader_q = CfSearchQuery {
            query: "complementary",
            mc_version: Some("1.21.1"),
            loader: Some("forge"),
            class_id: Some(6552),
            category_id: None,
            index: None,
            offset: 0,
            limit: 20,
        };
        let shader_url = build_search_url(&shader_q);
        assert!(
            !shader_url.contains("modLoaderType"),
            "Shaders must NOT include modLoaderType"
        );
        assert!(shader_url.contains("classId=6552"));
    }

    #[test]
    fn class_id_for_project_type_matches_live_categories() {
        assert_eq!(class_id_for_project_type("mod"), Some(6));
        assert_eq!(class_id_for_project_type("Mods"), Some(6));
        assert_eq!(class_id_for_project_type("resourcepack"), Some(12));
        assert_eq!(class_id_for_project_type("shader"), Some(6552));
        assert_eq!(class_id_for_project_type("modpack"), Some(4471));
        assert_eq!(class_id_for_project_type("datapack"), Some(6945));
        assert_eq!(class_id_for_project_type("world"), Some(17));
        // Unknown → no classId filter (search all classes).
        assert_eq!(class_id_for_project_type("bogus"), None);
        assert_eq!(class_id_for_project_type(""), None);
    }

    #[test]
    fn sort_field_for_frontend_index() {
        assert_eq!(sort_field_for(None), (6, "desc"));
        assert_eq!(sort_field_for(Some("downloads")), (6, "desc"));
        assert_eq!(sort_field_for(Some("newest")), (11, "desc"));
        assert_eq!(sort_field_for(Some("oldest")), (11, "asc"));
        assert_eq!(sort_field_for(Some("unknown")), (6, "desc"));
    }

    #[test]
    fn file_list_is_sorted_newest_first_and_drops_unavailable() {
        let mut files = vec![
            cf_file(1, "old", "2024-01-01T00:00:00Z", Some(true)),
            cf_file(2, "broken", "2025-06-01T00:00:00Z", Some(false)),
            cf_file(3, "new", "2025-01-01T00:00:00Z", None),
            cf_file(4, "removed-no-flag", "2023-05-01T00:00:00Z", None),
        ];
        sort_files_newest_first(&mut files);
        let ids: Vec<u64> = files.iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![3, 1, 4]);
    }

    #[test]
    fn cdn_urls_use_file_id_shards_and_encoding() {
        let urls = cf_cdn_urls(1_234_567, "My Mod 1.0.jar");
        assert!(urls[0].starts_with("https://edge.forgecdn.net/files/1234/567/"));
        assert!(urls.iter().all(|u| u.contains("/files/1234/567/")));
        assert_eq!(
            urls.iter()
                .filter(|u| u.contains("mediafilez.forgecdn.net"))
                .count(),
            2
        );
        // Spaces are percent-encoded in the second variant.
        assert!(urls.iter().any(|u| u.contains("My%20Mod%201.0.jar")));
    }

    #[tokio::test]
    async fn download_candidates_without_api_key_are_pure_cdn() {
        let urls = download_url_candidates(42, 7, "mod.jar", "").await;
        assert!(!urls.is_empty());
        assert!(urls
            .iter()
            .all(|u| u.starts_with("https://") && u.contains("forgecdn.net")));
        // Deduplicated.
        let mut dedup = urls.clone();
        dedup.dedup();
        assert_eq!(urls, dedup);
    }

    #[test]
    fn bulk_project_row_parses_latest_file_indexes() {
        let json = r#"{
            "data": [{
                "id": 238222,
                "name": "Just Enough Items (JEI)",
                "latestFiles": [],
                "latestFilesIndexes": [{
                    "gameVersion": "1.20.1",
                    "fileId": 9205046,
                    "filename": "jei-1.20.1-forge-15.71.0.224.jar",
                    "releaseType": 2,
                    "gameVersionTypeId": 75125,
                    "modLoader": 1
                }]
            }]
        }"#;
        let resp: CfDataResponse<Vec<CfBulkMod>> = serde_json::from_str(json).unwrap();
        let project = &resp.data[0];
        assert_eq!(project.id, 238222);
        assert_eq!(project.name, "Just Enough Items (JEI)");
        let index = &project.latest_files_indexes[0];
        assert_eq!(index.game_version, "1.20.1");
        assert_eq!(index.file_id, 9205046);
        assert_eq!(index.release_type, 2);
        assert_eq!(index.mod_loader, Some(1));
    }

    #[test]
    fn bulk_file_row_parses_hash_and_availability() {
        let json = r#"{
            "data": [{
                "id": 9254760,
                "modId": 238222,
                "displayName": "19.69.0.456 for NeoForge 1.21.1",
                "fileName": "jei-1.21.1-neoforge-19.69.0.456.jar",
                "downloadUrl": "https://edge.forgecdn.net/files/9254/760/jei.jar",
                "fileLength": 1234,
                "fileDate": "2026-01-01T00:00:00Z",
                "gameVersions": ["1.21.1", "NeoForge"],
                "dependencies": [],
                "releaseType": 1,
                "isAvailable": true,
                "hashes": [{"value": "abc", "algo": 1}]
            }]
        }"#;
        let resp: CfDataResponse<Vec<CfFile>> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data[0].mod_id, 238222);
        assert_eq!(resp.data[0].sha1_hash(), Some("abc"));
        assert_eq!(resp.data[0].is_available, Some(true));
    }

    #[test]
    fn api_host_matches_the_configured_api_origin() {
        assert_eq!(
            api_host(&format!("{BASE_URL}/v1/mods/1")).expect("API URL"),
            "api.curseforge.com"
        );
        assert_eq!(
            api_host("https://API.CurseForge.com/v1/mods").expect("API URL"),
            "api.curseforge.com"
        );
        assert!(api_host("not a url").is_err());
        assert!(api_host("mailto:test@example.com").is_err());
    }
}

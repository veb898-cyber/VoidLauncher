use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::Semaphore;

use crate::error::{LauncherError, Result};
use crate::instances::{self, Instance, LoaderType};
use crate::modloaders;

pub const ATL_DOWNLOAD_BASE: &str = "https://download.nodecdn.net/containers/atl/";
pub const ATL_INDEX_URL: &str =
    "https://download.nodecdn.net/containers/atl/launcher/json/packsnew.json";
/// Pack icons are served as static files keyed by the pack's safe name
/// (lowercased); the packs index itself carries no icon field.
pub const ATL_IMAGES_BASE: &str =
    "https://download.nodecdn.net/containers/atl/launcher/images";

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct AtPackVersion {
    pub version: String,
    pub minecraft: String,
    #[serde(default)]
    pub is_recommended: bool,
    #[serde(default)]
    pub can_update: bool,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct AtPack {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Pack icon from the ATL index (base64 PNG, may include the data: prefix).
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub versions: Vec<AtPackVersion>,
}

impl AtPack {
    pub fn safe_name(&self) -> String {
        self.name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect()
    }

    /// CDN URL of the pack icon (mirrors ATLauncher's own `<safe name>.png`
    /// layout). Not every pack has an image on the CDN; the UI falls back to
    /// a placeholder when the URL 404s.
    pub fn icon_url(&self) -> String {
        format!(
            "{}/{}.png",
            ATL_IMAGES_BASE,
            self.safe_name().to_lowercase()
        )
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct AtLoaderMetadata {
    pub minecraft: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub loader: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AtLoader {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub metadata: Option<AtLoaderMetadata>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct AtMod {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub md5: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub filesize: Option<u64>,
    #[serde(rename = "type", default)]
    pub type_: String,
    #[serde(default)]
    pub download: String,
    #[serde(default)]
    pub client: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub recommended: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub library: bool,
    #[serde(default)]
    pub extract_to: Option<String>,
    #[serde(default)]
    pub extract_folder: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AtConfigsZip {
    #[serde(default)]
    pub filesize: Option<u64>,
    #[serde(default)]
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AtPackVersionDetail {
    pub version: String,
    pub minecraft: String,
    #[serde(default)]
    pub loader: Option<AtLoader>,
    #[serde(default)]
    pub mods: Vec<AtMod>,
    #[serde(default)]
    pub configs: Option<AtConfigsZip>,
    #[allow(dead_code)]
    #[serde(default)]
    pub main_class: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub memory: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AtModpackEntry {
    pub id: u64,
    pub name: String,
    pub safe_name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub versions: Vec<AtPackVersion>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AtVersionDetailEntry {
    pub version: String,
    pub minecraft: String,
    pub loader: Option<String>,
    pub loader_version: Option<String>,
    pub mods: Vec<AtMod>,
    pub has_configs: bool,
}

static PACKS_CACHE: OnceLock<tokio::sync::Mutex<Option<Vec<AtPack>>>> = OnceLock::new();
static ATL_CACHE_DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn packs_cache() -> &'static tokio::sync::Mutex<Option<Vec<AtPack>>> {
    PACKS_CACHE.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// Set the data dir used to persist the packs index (called at startup).
pub fn set_atl_cache_dir(dir: PathBuf) {
    *ATL_CACHE_DIR
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = Some(dir);
}

fn atl_cache_file() -> Option<PathBuf> {
    ATL_CACHE_DIR
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .as_ref()
        .map(|d| d.join("atl_packs_cache.json"))
}

fn read_packs_from_disk() -> Option<Vec<AtPack>> {
    let path = atl_cache_file()?;
    let data = std::fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn write_packs_to_disk(packs: &[AtPack]) {
    if let Some(path) = atl_cache_file() {
        if let Ok(json) = serde_json::to_string(packs) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, json);
        }
    }
}

const ATL_RETRY_DELAYS_MS: [u64; 3] = [500, 1000, 2000];

pub async fn fetch_packs() -> Result<Vec<AtPack>> {
    {
        let cache = packs_cache().lock().await;
        if let Some(packs) = cache.as_ref() {
            return Ok(packs.clone());
        }
    }
    crate::download::ensure_proxy_resolved().await;
    let client = crate::download::global_http_client();
    let mut last_err = None;
    for attempt in 0..=ATL_RETRY_DELAYS_MS.len() {
        match crate::download::send_with_fallback(
            client
                .get(ATL_INDEX_URL)
                .timeout(std::time::Duration::from_secs(20)),
        )
        .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    last_err = Some(LauncherError::Download(format!(
                        "ATLauncher packs index returned {}",
                        resp.status()
                    )));
                } else {
                    match resp.json::<Vec<AtPack>>().await {
                        Ok(packs) => {
                            let packs = with_icon_urls(packs);
                            write_packs_to_disk(&packs);
                            let mut cache = packs_cache().lock().await;
                            *cache = Some(packs.clone());
                            return Ok(packs);
                        }
                        Err(e) => last_err = Some(LauncherError::Network(e)),
                    }
                }
            }
            Err(e) => last_err = Some(LauncherError::Network(e)),
        }
        if attempt < ATL_RETRY_DELAYS_MS.len() {
            crate::events::emit_fetch_retry(
                "atlauncher",
                attempt + 2,
                ATL_RETRY_DELAYS_MS.len() + 1,
                "Retrying ATLauncher packs index",
            );
            tokio::time::sleep(std::time::Duration::from_millis(ATL_RETRY_DELAYS_MS[attempt])).await;
        }
    }
    if let Some(cached) = read_packs_from_disk() {
        let cached = with_icon_urls(cached);
        let mut cache = packs_cache().lock().await;
        *cache = Some(cached.clone());
        return Ok(cached);
    }
    Err(last_err.unwrap_or_else(|| {
        LauncherError::Download("ATLauncher packs index download failed".to_string())
    }))
}

fn with_icon_urls(mut packs: Vec<AtPack>) -> Vec<AtPack> {
    for pack in packs.iter_mut() {
        if pack.icon.is_none() {
            pack.icon = Some(pack.icon_url());
        }
    }
    packs
}

pub async fn fetch_version_detail(safe_name: &str, version: &str) -> Result<AtPackVersionDetail> {
    let url = format!(
        "{}packs/{}/versions/{}/Configs.json",
        ATL_DOWNLOAD_BASE,
        safe_name,
        version
    );
    crate::download::ensure_proxy_resolved().await;
    let client = crate::download::global_http_client();
    let mut last_err = None;
    for attempt in 0..=ATL_RETRY_DELAYS_MS.len() {
        match crate::download::send_with_fallback(
            client
                .get(&url)
                .timeout(std::time::Duration::from_secs(20)),
        )
        .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    last_err = Some(LauncherError::Download(format!(
                        "ATLauncher Configs.json returned {} for {}",
                        resp.status(),
                        url
                    )));
                } else {
                    match resp.json::<AtPackVersionDetail>().await {
                        Ok(detail) => return Ok(detail),
                        Err(e) => last_err = Some(LauncherError::Network(e)),
                    }
                }
            }
            Err(e) => last_err = Some(LauncherError::Network(e)),
        }
        if attempt < ATL_RETRY_DELAYS_MS.len() {
            crate::events::emit_fetch_retry(
                "atlauncher",
                attempt + 2,
                ATL_RETRY_DELAYS_MS.len() + 1,
                "Retrying ATLauncher pack info",
            );
            tokio::time::sleep(std::time::Duration::from_millis(ATL_RETRY_DELAYS_MS[attempt])).await;
        }
    }
    Err(last_err.unwrap_or_else(|| {
        LauncherError::Download(format!("ATLauncher Configs.json download failed: {}", url))
    }))
}

fn loader_enum(type_: &str) -> Option<LoaderType> {
    match type_.to_lowercase().as_str() {
        "fabric" => Some(LoaderType::Fabric),
        "forge" => Some(LoaderType::Forge),
        "neoforge" => Some(LoaderType::NeoForge),
        _ => None,
    }
}

fn loader_display_name(type_: &str) -> &str {
    match type_.to_lowercase().as_str() {
        "fabric" => "Fabric",
        "forge" => "Forge",
        "neoforge" => "NeoForge",
        _ => type_,
    }
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModpackProgressPayload {
    pub stage: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
}

fn emit_progress(app: Option<&AppHandle>, stage: &str, current: usize, total: usize, message: &str) {
    if let Some(app) = app {
        let _ = app.emit("import-progress", ModpackProgressPayload {
            stage: stage.to_string(),
            current,
            total,
            message: message.to_string(),
        });
    }
}

fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "mod".to_string()
    } else {
        trimmed.to_string()
    }
}

fn mod_dest_dir(type_: &str) -> Option<&'static str> {
    match type_.to_lowercase().as_str() {
        "mods" | "dependency" | "ic2lib" | "denlib" | "coremods" | "jar" => Some("mods"),
        "resourcepack" | "texturepack" => Some("resourcepacks"),
        "shaderpack" => Some("shaderpacks"),
        _ => None,
    }
}

fn write_atl_sidecar(mc_dir: &Path, dest: &Path, name: &str, version: Option<&str>) {
    let filename = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name);
    let sidecar = serde_json::json!({
        "provider": "atlauncher",
        "project_name": name,
        "version_number": version,
    });
    let path = instances::sidecar_meta_path(&mc_dir.join("mods"), filename);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, sidecar.to_string());
}

fn verify_md5(path: &Path, expected: &str) -> bool {
    use md5::Digest;
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let hash = format!("{:x}", md5::Md5::digest(&bytes));
    hash == expected.to_lowercase()
}

async fn download_atl_mod(
    m: &AtMod,
    dest: &Path,
    client: &reqwest::Client,
) -> Result<()> {
    let url = match m.download.as_str() {
        "server" => m
            .url
            .as_ref()
            .map(|u| format!("{}{}", ATL_DOWNLOAD_BASE, u)),
        "direct" => m.url.clone(),
        _ => None,
    }
    .ok_or_else(|| LauncherError::Instance(format!("No downloadable URL for {}", m.name)))?;

    if !crate::is_allowed_download_host(&url) {
        return Err(LauncherError::Download(format!(
            "Host not in allowlist: {}",
            url
        )));
    }

    if dest.exists() {
        if let Some(md5) = &m.md5 {
            if !md5.is_empty() && verify_md5(dest, md5) {
                return Ok(());
            }
        }
    }

    let resp = crate::download::send_with_fallback(
        client.get(&url).timeout(std::time::Duration::from_secs(60)),
    )
    .await?;
    if !resp.status().is_success() {
        return Err(LauncherError::Download(format!(
            "Download returned {} for {}",
            resp.status(),
            url
        )));
    }
    let bytes = resp.bytes().await?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, &bytes)?;
    if let Some(md5) = &m.md5 {
        if !md5.is_empty() && !verify_md5(dest, md5) {
            let _ = std::fs::remove_file(dest);
            return Err(LauncherError::Download(format!("MD5 mismatch for {}", m.name)));
        }
    }
    Ok(())
}

/// Install an ATLauncher pack by id + version into a new instance.
pub async fn install_atlauncher_pack(
    instances_dir: &PathBuf,
    libraries_dir: &Path,
    versions_dir: &Path,
    pack_id: u64,
    version: &str,
    instance_name: &str,
    app: Option<&AppHandle>,
) -> Result<Instance> {
    emit_progress(app, "indexing", 0, 1, "Fetching pack index...");
    let packs = fetch_packs().await?;
    let pack = packs
        .iter()
        .find(|p| p.id == pack_id)
        .ok_or_else(|| LauncherError::Instance(format!("Pack {} not found in ATLauncher index", pack_id)))?;
    let safe_name = pack.safe_name();

    emit_progress(app, "reading", 0, 1, &format!("Reading {} {}...", pack.name, version));
    let detail = fetch_version_detail(&safe_name, version).await?;

    let loader_type = detail
        .loader
        .as_ref()
        .map(|l| l.type_.clone())
        .unwrap_or_default();
    let loader_enum = loader_enum(&loader_type);
    let loader_version = detail
        .loader
        .as_ref()
        .and_then(|l| l.metadata.as_ref())
        .and_then(|m| {
            m.version
                .as_deref()
                .or(m.loader.as_deref())
                .map(|s| s.to_string())
        });

    let target_dir = instances_dir.join(instance_name);
    let mc_dir = target_dir.join(".minecraft");
    std::fs::create_dir_all(&mc_dir)?;

    let mut instance = Instance {
        name: instance_name.to_string(),
        mc_version: detail.minecraft.clone(),
        loader: loader_enum.clone().unwrap_or(LoaderType::Vanilla),
        loader_version: loader_version.clone(),
        loader_profile: None,
        memory_mb: Some(crate::config::recommended_memory_mb(crate::config::detect_total_ram_mb())),
        jvm_args: None,
        gc_preset: Some("standard".to_string()),
        java_path: None,
        resolution: None,
        icon: None,
        banner: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        last_played: None,
        play_time_seconds: 0,
        notes: format!("Installed from ATLauncher pack '{}' (v{})", pack.name, version),
    };

    // Install the mod loader (if any) so the pack runs immediately.
    if let (Some(lt), Some(lv)) = (loader_enum, &loader_version) {
        if lt != LoaderType::Vanilla {
            emit_progress(
                app,
                "loader",
                0,
                1,
                &format!("Installing {} {}...", loader_display_name(&loader_type), lv),
            );
            match modloaders::install_loader(
                loader_display_name(&loader_type),
                &detail.minecraft,
                lv,
                libraries_dir,
                versions_dir,
                app.clone(),
            )
            .await
            {
                Ok(profile) => instance.loader_profile = Some(profile),
                Err(e) => {
                    tracing::warn!(target: "launcher", "Loader install failed for ATL pack: {}", e);
                }
            }
        }
    }

    // Configs.zip (pack configs/scripts) — verified by SHA1 from the manifest.
    if let Some(configs) = &detail.configs {
        if let Some(sha1) = &configs.sha1 {
            if !sha1.is_empty() {
                emit_progress(app, "configs", 0, 1, "Downloading pack configs...");
                let zip_path = target_dir.join(".tmp-configs.zip");
                let configs_url = format!(
                    "{}packs/{}/versions/{}/Configs.zip",
                    ATL_DOWNLOAD_BASE,
                    safe_name,
                    version
                );
                let dl = match configs.filesize {
                    Some(size) if size > 0 => {
                        crate::download::download_file_sized(&configs_url, &zip_path, sha1, size)
                            .await
                    }
                    _ => crate::download::download_file(&configs_url, &zip_path, sha1).await,
                };
                match dl {
                    Ok(()) => {
                        if let Err(e) = crate::import::extract_zip_to_dir(&zip_path, &mc_dir) {
                            tracing::warn!(target: "launcher", "Failed to extract ATL configs.zip: {}", e);
                        }
                        let _ = std::fs::remove_file(&zip_path);
                    }
                    Err(e) => {
                        tracing::warn!(target: "launcher", "Failed to download ATL configs.zip: {}", e);
                    }
                }
            }
        }
    }

    // Download mods (client-side, non-hidden, with concurrency 4).
    let client_mods: Vec<&AtMod> = detail
        .mods
        .iter()
        .filter(|m| {
            m.client
                && !m.hidden
                && m.download != "browser"
                && !matches!(m.type_.to_lowercase().as_str(), "forge" | "neoforge" | "fabric")
        })
        .collect();
    let total = client_mods.len();
    emit_progress(app, "downloading-mods", 0, total, &format!("Preparing {} mods...", total));

    let semaphore = Arc::new(Semaphore::new(4));
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::with_capacity(total);
    for m in client_mods {
        let sem = semaphore.clone();
        let completed = completed.clone();
        let mc_dir = mc_dir.clone();
        let app_owned = app.cloned();
        let m = m.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.map_err(|e| format!("Semaphore error: {}", e))?;
            let file_name = m
                .file
                .clone()
                .filter(|f| !f.is_empty())
                .unwrap_or_else(|| {
                    m.url
                        .as_ref()
                        .and_then(|u| u.rsplit('/').next())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| safe_filename(&m.name))
                });
            let file_name = safe_filename(&file_name);
            let dest_dir = mod_dest_dir(&m.type_)
                .map(|d| mc_dir.join(d))
                .unwrap_or_else(|| mc_dir.join("mods"));
            let dest = dest_dir.join(&file_name);
            if dest.exists() && !m.optional {
                let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if let Some(ref a) = app_owned {
                    let _ = a.emit(
                        "import-progress",
                        ModpackProgressPayload {
                            stage: "downloading-mods".into(),
                            current: done,
                            total,
                            message: format!("Skipped {}", m.name),
                        },
                    );
                }
                return Ok::<(), String>(());
            }
            let client = crate::download::global_http_client();
            let result = download_atl_mod(&m, &dest, &client).await;
            match result {
                Ok(()) => {
                    if m.type_.to_lowercase().as_str() == "extract" {
                        if let Err(e) = crate::import::extract_zip_to_dir(&dest, &mc_dir) {
                            tracing::warn!(target: "launcher", "Failed to extract ATL mod {}: {}", m.name, e);
                        }
                        let _ = std::fs::remove_file(&dest);
                    } else {
                        write_atl_sidecar(&mc_dir, &dest, &m.name, m.version.as_deref());
                    }
                    let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if let Some(ref a) = app_owned {
                        let _ = a.emit(
                            "import-progress",
                            ModpackProgressPayload {
                                stage: "downloading-mods".into(),
                                current: done,
                                total,
                                message: format!("Downloaded {}", m.name),
                            },
                        );
                    }
                    Ok(())
                }
                Err(e) => {
                    tracing::warn!(target: "launcher", "ATL mod download failed for {}: {}", m.name, e);
                    let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if let Some(ref a) = app_owned {
                        let _ = a.emit(
                            "import-progress",
                            ModpackProgressPayload {
                                stage: "downloading-mods".into(),
                                current: done,
                                total,
                                message: format!("Failed {}", m.name),
                            },
                        );
                    }
                    Err(format!("{}: {}", m.name, e))
                }
            }
        }));
    }

    for t in tasks {
        let _ = t.await;
    }

    instances::save_instance(instances_dir, &instance, None)?;
    emit_progress(app, "done", 1, 1, "Install complete!");
    tracing::info!(target: "launcher", "Installed ATLauncher pack '{}' as '{}'", pack.name, instance_name);
    Ok(instance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_strips_non_alphanumeric() {
        let pack = AtPack {
            id: 1,
            name: "Ye Olde Times (Pack)".to_string(),
            description: None,
            icon: None,
            versions: vec![],
        };
        assert_eq!(pack.safe_name(), "YeOldeTimesPack");
    }

    #[test]
    fn loader_enum_maps_known_types() {
        assert_eq!(loader_enum("forge"), Some(LoaderType::Forge));
        assert_eq!(loader_enum("neoforge"), Some(LoaderType::NeoForge));
        assert_eq!(loader_enum("fabric"), Some(LoaderType::Fabric));
        assert_eq!(loader_enum("quilt"), None);
        assert_eq!(loader_enum(""), None);
    }

    #[test]
    fn mod_dest_dir_maps_types() {
        assert_eq!(mod_dest_dir("mods"), Some("mods"));
        assert_eq!(mod_dest_dir("dependency"), Some("mods"));
        assert_eq!(mod_dest_dir("ic2lib"), Some("mods"));
        assert_eq!(mod_dest_dir("denlib"), Some("mods"));
        assert_eq!(mod_dest_dir("coremods"), Some("mods"));
        assert_eq!(mod_dest_dir("jar"), Some("mods"));
        assert_eq!(mod_dest_dir("resourcepack"), Some("resourcepacks"));
        assert_eq!(mod_dest_dir("texturepack"), Some("resourcepacks"));
        assert_eq!(mod_dest_dir("shaderpack"), Some("shaderpacks"));
        assert_eq!(mod_dest_dir("forge"), None);
        assert_eq!(mod_dest_dir("extract"), None);
    }

    #[test]
    fn parses_configs_json_detail() {
        let json = r#"{
            "version": "5.0.1",
            "minecraft": "1.21.1",
            "loader": {
                "type": "neoforge",
                "choose": false,
                "metadata": {"minecraft": "1.21.1", "version": "21.1.83", "rawVersion": "21.1.83", "loader": ""},
                "className": "com.atlauncher.data.loaders.neoforge.NeoForgeLoader"
            },
            "configs": {"filesize": 1234, "sha1": "abc123"},
            "mods": [
                {"name": "Example Mod", "version": "1.0", "url": "mods/example.jar", "file": "example.jar",
                 "md5": "d41d8cd98f00b204e9800998ecf8427e", "filesize": 42, "type": "mods",
                 "download": "server", "client": true, "optional": false, "recommended": true, "hidden": false,
                 "library": false}
            ]
        }"#;
        let detail: AtPackVersionDetail = serde_json::from_str(json).unwrap();
        assert_eq!(detail.minecraft, "1.21.1");
        assert_eq!(detail.loader.as_ref().unwrap().type_, "neoforge");
        assert_eq!(
            detail.loader.as_ref().unwrap().metadata.as_ref().unwrap().version.as_deref(),
            Some("21.1.83")
        );
        assert_eq!(detail.configs.as_ref().unwrap().sha1.as_deref(), Some("abc123"));
        assert_eq!(detail.mods.len(), 1);
        let m = &detail.mods[0];
        assert_eq!(m.name, "Example Mod");
        assert_eq!(m.type_, "mods");
        assert_eq!(m.download, "server");
        assert!(m.client);
    }

    #[test]
    fn parses_modern_fabric_loader_metadata() {
        let json = r#"{
            "version": "1.21.11-1",
            "minecraft": "1.21.11",
            "loader": {
                "type": "fabric",
                "metadata": {"minecraft": "1.21.11", "loader": "0.18.4"}
            }
        }"#;
        let detail: AtPackVersionDetail = serde_json::from_str(json).unwrap();
        assert_eq!(detail.loader.as_ref().unwrap().type_, "fabric");
        assert_eq!(
            detail.loader.as_ref().unwrap().metadata.as_ref().unwrap().loader.as_deref(),
            Some("0.18.4")
        );
    }
}
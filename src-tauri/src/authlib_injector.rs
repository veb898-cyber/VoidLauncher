//! Pinned authlib-injector management for in-game Ely.by skins.
//!
//! The agent is a third-party JAR distributed by the authlib-injector
//! project. VoidLauncher deliberately does not follow a remote "latest"
//! pointer at runtime: the exact build, URL, size and SHA-256 below are
//! pinned in the binary. Updating the agent is therefore a launcher release,
//! never an unattended download.
//!
//! The JAR is stored only in the user's runtime cache, never in the project,
//! the instance, `mods/` or the Minecraft classpath. It is loaded exclusively
//! through `-javaagent`, which is the case explicitly covered by the
//! upstream AGPL-3.0 "AUTHLIB-INJECTOR" exception.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

/// Verified upstream release. Keep all four values in sync when bumping.
pub const VERSION: &str = "1.2.8";
pub const BUILD_NUMBER: u64 = 56;
pub const JAR_URL: &str =
    "https://authlib-injector.yushi.moe/artifact/56/authlib-injector-1.2.8.jar";
pub const JAR_SHA256: &str = "9c7f4343e6c82034958ffb48c14a2cb0c85928be7283103ce17da00c6d5a7b10";
pub const JAR_SIZE: u64 = 349_681;

/// The oldest Minecraft family the pinned agent is documented to support.
pub const MIN_MC_VERSION: &str = "1.7.2";

/// Direct, canonical Ely.by API root. It is also the target advertised by the
/// ALI header on `https://ely.by`, but using it directly keeps launch
/// independent from availability of the main website.
pub const ELYBY_API_ROOT: &str = "https://account.ely.by/api/authlib-injector";

const INJECTOR_HOST: &str = "authlib-injector.yushi.moe";
const ELYBY_ORIGIN: &str = "https://ely.by/";
const ELYBY_HOSTS: &[&str] = &["ely.by", "account.ely.by", "authserver.ely.by"];
const MAX_JAR_BYTES: u64 = 2 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 8 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const METADATA_CACHE_TTL_SECS: u64 = 24 * 60 * 60;
/// A stale cache may be used only when the network is temporarily unavailable,
/// and never indefinitely: rotated Ely.by signing keys must eventually be
/// picked up.
const METADATA_MAX_STALE_SECS: u64 = 7 * 24 * 60 * 60;

/// Why preparation failed. The renderer maps this to a localized message;
/// the technical `detail` stays in the developer log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrepareErrorKind {
    /// Ely.by (or the agent host) could not be reached.
    Network,
    /// The metadata endpoint answered with an HTTP/protocol error.
    Metadata,
    /// The agent download failed (HTTP, size, write, install).
    Download,
    /// The downloaded agent's SHA-256 does not match the pinned value.
    Checksum,
    /// The file is not the expected JAR or its manifest/license is wrong.
    CorruptJar,
    /// The metadata JSON is syntactically or semantically invalid.
    InvalidMetadata,
    /// Local cache/path/serialization problem.
    Internal,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrepareError {
    pub kind: PrepareErrorKind,
    pub detail: String,
}

impl PrepareError {
    pub fn new(kind: PrepareErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for PrepareError {}

/// Data handed to the launch profile for the two JVM arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthlibInjector {
    pub jar: PathBuf,
    pub api_root: String,
    /// Base64 of the validated Yggdrasil metadata JSON.
    pub prefetched_metadata: String,
}

/// Everything needed to attach the agent. The agent is only prepared when the
/// active account is Ely.by, the setting is enabled and the MC version is in
/// the documented range.
pub fn should_attach(is_elyby: bool, enabled: bool, mc_version: &str) -> bool {
    is_elyby && enabled && is_supported_mc_version(mc_version)
}

/// `1.7.2` is the first version with the separate Authlib library that the
/// agent transforms. Snapshots use a `major.minor` core (e.g. `26.3-snapshot-2`)
/// and are accepted; alpha/beta ids are not.
pub fn is_supported_mc_version(version: &str) -> bool {
    let core = version.trim().split(['-', '+']).next().unwrap_or("").trim();
    let mut parts = core.split('.');
    let major: u32 = match parts.next().and_then(|p| p.parse().ok()) {
        Some(v) => v,
        None => return false,
    };
    let minor: u32 = match parts.next().and_then(|p| p.parse().ok()) {
        Some(v) => v,
        None => return false,
    };
    let patch: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor, patch) >= (1, 7, 2)
}

/// Ensure the pinned agent is present and verified, then fetch (or reuse) the
/// Ely.by metadata and turn it into the JVM prefetch payload.
pub async fn prepare(data_dir: &Path) -> Result<AuthlibInjector, PrepareError> {
    let jar = ensure_agent(data_dir).await?;
    let (api_root, metadata) = load_or_fetch_metadata(data_dir).await?;
    let bytes = serde_json::to_vec(&metadata).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Internal,
            format!("Cannot serialize Ely.by metadata: {e}"),
        )
    })?;
    Ok(AuthlibInjector {
        jar,
        api_root,
        prefetched_metadata: crate::instances::base64_encode(&bytes),
    })
}

// ==================== Agent download and cache ====================

#[derive(Debug, Clone)]
struct AgentSpec {
    version: String,
    build_number: u64,
    url: String,
    sha256: String,
    size: u64,
}

fn pinned_spec() -> AgentSpec {
    AgentSpec {
        version: VERSION.to_string(),
        build_number: BUILD_NUMBER,
        url: JAR_URL.to_string(),
        sha256: JAR_SHA256.to_string(),
        size: JAR_SIZE,
    }
}

fn agent_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn agent_path(data_dir: &Path, version: &str) -> PathBuf {
    data_dir
        .join("runtime")
        .join("authlib-injector")
        .join(version)
        .join("authlib-injector.jar")
}

type DownloadFuture = Pin<Box<dyn Future<Output = Result<(), PrepareError>> + Send>>;
type Downloader = Arc<dyn Fn(PathBuf) -> DownloadFuture + Send + Sync>;

/// Production entry point: pinned spec plus the real HTTPS downloader.
pub async fn ensure_agent(data_dir: &Path) -> Result<PathBuf, PrepareError> {
    let spec = pinned_spec();
    let url = spec.url.clone();
    let downloader: Downloader = Arc::new(move |part: PathBuf| {
        let url = url.clone();
        Box::pin(async move { download_jar(&url, &part).await })
    });
    ensure_agent_with(data_dir, &spec, downloader).await
}

/// Testable core. The global lock serialises concurrent launches in this
/// process; a unique `.part` name and an atomic rename make separate
/// processes harmless as well (both install the same pinned bytes).
async fn ensure_agent_with(
    data_dir: &Path,
    spec: &AgentSpec,
    downloader: Downloader,
) -> Result<PathBuf, PrepareError> {
    let _guard = agent_lock().lock().await;
    let dest = agent_path(data_dir, &spec.version);

    if validate_jar(&dest, spec).is_ok() {
        return Ok(dest);
    }
    if dest.exists() {
        tracing::warn!(
            target: "launcher",
            "authlib-injector cache is invalid, replacing: {}",
            dest.display()
        );
        let _ = std::fs::remove_file(&dest);
    }

    let parent = dest.parent().ok_or_else(|| {
        PrepareError::new(
            PrepareErrorKind::Internal,
            "Invalid authlib-injector cache path",
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Internal,
            format!("Cannot create authlib-injector cache directory: {e}"),
        )
    })?;

    let part = unique_part_path(&dest);
    if let Err(e) = downloader(part.clone()).await {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    if let Err(e) = install_verified_jar(&part, &dest, spec) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    Ok(dest)
}

fn unique_part_path(dest: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("authlib-injector.jar");
    dest.with_file_name(format!(".{name}.{}.{nanos}.part", std::process::id()))
}

fn install_verified_jar(part: &Path, dest: &Path, spec: &AgentSpec) -> Result<(), PrepareError> {
    validate_jar(part, spec)?;

    // Another process may have finished the same pinned install meanwhile.
    if dest.exists() {
        if validate_jar(dest, spec).is_ok() {
            let _ = std::fs::remove_file(part);
            return Ok(());
        }
        std::fs::remove_file(dest).map_err(|e| {
            PrepareError::new(
                PrepareErrorKind::Download,
                format!("Cannot replace invalid authlib-injector cache: {e}"),
            )
        })?;
    }

    std::fs::rename(part, dest).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Download,
            format!("Cannot install authlib-injector into the cache: {e}"),
        )
    })?;
    // Re-read the installed file, not the pre-rename one: catches a truncated
    // copy caused by disk problems before the agent can ever be launched.
    validate_jar(dest, spec)
}

async fn download_jar(url: &str, part: &Path) -> Result<(), PrepareError> {
    let client = crate::download::http_client_for_hosts(&[INJECTOR_HOST]).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Internal,
            format!("Cannot create authlib-injector HTTP client: {e}"),
        )
    })?;
    let response = client.get(url).send().await.map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Network,
            format!("Cannot download authlib-injector: {e}"),
        )
    })?;

    if !response.status().is_success() {
        return Err(PrepareError::new(
            PrepareErrorKind::Download,
            format!(
                "authlib-injector download failed with HTTP {}",
                response.status()
            ),
        ));
    }
    ensure_injector_url(response.url())
        .map_err(|e| PrepareError::new(PrepareErrorKind::Download, e))?;
    if let Some(length) = response.content_length() {
        if length > MAX_JAR_BYTES {
            return Err(PrepareError::new(
                PrepareErrorKind::Download,
                format!("authlib-injector download is unexpectedly large ({length} bytes)"),
            ));
        }
    }

    let mut file = tokio::fs::File::create(part).await.map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Download,
            format!("Cannot create authlib-injector download file: {e}"),
        )
    })?;
    let mut stream = response.bytes_stream();
    let mut total: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            PrepareError::new(
                PrepareErrorKind::Network,
                format!("authlib-injector download interrupted: {e}"),
            )
        })?;
        total = total.saturating_add(chunk.len() as u64);
        if total > MAX_JAR_BYTES {
            return Err(PrepareError::new(
                PrepareErrorKind::Download,
                format!("authlib-injector download exceeded the {MAX_JAR_BYTES}-byte limit"),
            ));
        }
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| {
                PrepareError::new(
                    PrepareErrorKind::Download,
                    format!("Cannot write authlib-injector download: {e}"),
                )
            })?;
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|e| {
            PrepareError::new(
                PrepareErrorKind::Download,
                format!("Cannot flush authlib-injector download: {e}"),
            )
        })?;
    file.sync_all().await.map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Download,
            format!("Cannot sync authlib-injector download: {e}"),
        )
    })?;
    Ok(())
}

fn ensure_injector_url(url: &Url) -> Result<(), String> {
    let host_ok = url
        .host_str()
        .map(|h| h.eq_ignore_ascii_case(INJECTOR_HOST))
        .unwrap_or(false);
    if url.scheme() != "https" || !host_ok || !url.username().is_empty() || url.password().is_some()
    {
        return Err("authlib-injector download redirected outside the pinned host".to_string());
    }
    Ok(())
}

fn validate_jar(path: &Path, spec: &AgentSpec) -> Result<(), PrepareError> {
    let meta = std::fs::metadata(path).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::CorruptJar,
            format!("Cannot inspect authlib-injector: {e}"),
        )
    })?;
    if !meta.is_file() || meta.len() != spec.size {
        return Err(PrepareError::new(
            PrepareErrorKind::CorruptJar,
            format!(
                "authlib-injector has an unexpected size (expected {}, got {})",
                spec.size,
                meta.len()
            ),
        ));
    }
    let hash_ok = crate::download::verify_sha256(path, &spec.sha256).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::CorruptJar,
            format!("Cannot hash authlib-injector: {e}"),
        )
    })?;
    if !hash_ok {
        return Err(PrepareError::new(
            PrepareErrorKind::Checksum,
            "authlib-injector SHA-256 does not match the pinned value",
        ));
    }
    crate::download::verify_zip_magic(path).map_err(|_| {
        PrepareError::new(
            PrepareErrorKind::CorruptJar,
            "authlib-injector is not a valid JAR archive",
        )
    })?;
    validate_manifest(path, spec)
}

fn validate_manifest(path: &Path, spec: &AgentSpec) -> Result<(), PrepareError> {
    use std::io::Read as _;

    let file = std::fs::File::open(path).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::CorruptJar,
            format!("Cannot open authlib-injector JAR: {e}"),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::CorruptJar,
            format!("Cannot read authlib-injector JAR: {e}"),
        )
    })?;

    let manifest = {
        let mut entry = archive.by_name("META-INF/MANIFEST.MF").map_err(|_| {
            PrepareError::new(
                PrepareErrorKind::CorruptJar,
                "authlib-injector JAR has no META-INF/MANIFEST.MF",
            )
        })?;
        if entry.size() > MAX_MANIFEST_BYTES {
            return Err(PrepareError::new(
                PrepareErrorKind::CorruptJar,
                "authlib-injector manifest is unexpectedly large",
            ));
        }
        let mut text = String::new();
        entry.read_to_string(&mut text).map_err(|e| {
            PrepareError::new(
                PrepareErrorKind::CorruptJar,
                format!("Cannot read authlib-injector manifest: {e}"),
            )
        })?;
        text
    };

    if manifest_field(&manifest, "Premain-Class").as_deref()
        != Some("moe.yushi.authlibinjector.Premain")
    {
        return Err(PrepareError::new(
            PrepareErrorKind::CorruptJar,
            "authlib-injector manifest has an unexpected Premain-Class",
        ));
    }
    if manifest_field(&manifest, "Implementation-Version").as_deref() != Some(spec.version.as_str())
    {
        return Err(PrepareError::new(
            PrepareErrorKind::CorruptJar,
            "authlib-injector manifest version does not match the pinned build",
        ));
    }
    let build_ok = manifest_field(&manifest, "Build-Number")
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|v| v == spec.build_number);
    if !build_ok {
        return Err(PrepareError::new(
            PrepareErrorKind::CorruptJar,
            "authlib-injector manifest build number does not match the pinned build",
        ));
    }

    let license = {
        let entry = archive
            .by_name("META-INF/licenses/authlib-injector.txt")
            .map_err(|_| {
                PrepareError::new(
                    PrepareErrorKind::CorruptJar,
                    "authlib-injector JAR has no license file",
                )
            })?;
        let mut text = String::new();
        entry
            .take(MAX_MANIFEST_BYTES)
            .read_to_string(&mut text)
            .map_err(|e| {
                PrepareError::new(
                    PrepareErrorKind::CorruptJar,
                    format!("Cannot read authlib-injector license: {e}"),
                )
            })?;
        text
    };
    if !license.contains("AUTHLIB-INJECTOR") {
        return Err(PrepareError::new(
            PrepareErrorKind::CorruptJar,
            "authlib-injector license exception is missing",
        ));
    }
    Ok(())
}

fn manifest_field(manifest: &str, key: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case(key) {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

// ==================== Ely.by API discovery and metadata ====================

fn resolve_api_location(base: &Url, header: Option<&str>) -> Result<Url, String> {
    let Some(raw) = header else {
        ensure_elyby_url(base)?;
        return Ok(base.clone());
    };
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 || raw.chars().any(|c| c.is_control()) {
        return Err("Ely.by returned an invalid API Location header".to_string());
    }
    let url = base
        .join(raw)
        .map_err(|_| "Ely.by returned an invalid API Location URL".to_string())?;
    ensure_elyby_url(&url)?;
    Ok(url)
}

fn ensure_elyby_url(url: &Url) -> Result<(), String> {
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let host_ok = host == "ely.by" || host.ends_with(".ely.by");
    let port_ok = url.port_or_known_default() == Some(443);
    if url.scheme() != "https"
        || !host_ok
        || !port_ok
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Ely.by API discovery left the allowed HTTPS hosts".to_string());
    }
    Ok(())
}

fn metadata_http_client() -> Result<reqwest::Client, PrepareError> {
    crate::download::http_client_for_hosts(ELYBY_HOSTS).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Internal,
            format!("Cannot create Ely.by HTTP client: {e}"),
        )
    })
}

/// Direct metadata fetch: the canonical API root, no dependency on the
/// `https://ely.by/` HTML page or its ALI header.
async fn fetch_metadata_direct() -> Result<(String, Value), PrepareError> {
    let client = metadata_http_client()?;
    let api_root = Url::parse(ELYBY_API_ROOT).map_err(|_| {
        PrepareError::new(
            PrepareErrorKind::Internal,
            "Invalid built-in Ely.by API root URL",
        )
    })?;
    let metadata = fetch_metadata_json(&client, &api_root).await?;
    validate_metadata(&metadata)
        .map_err(|e| PrepareError::new(PrepareErrorKind::InvalidMetadata, e))?;
    Ok((ELYBY_API_ROOT.to_string(), metadata))
}

/// ALI discovery fallback for the case where the direct endpoint is blocked
/// but the main Ely.by page still advertises a reachable API root.
async fn fetch_metadata_via_ali() -> Result<(String, Value), PrepareError> {
    let client = metadata_http_client()?;
    let base = Url::parse(ELYBY_ORIGIN).map_err(|_| {
        PrepareError::new(
            PrepareErrorKind::Internal,
            "Invalid built-in Ely.by origin URL",
        )
    })?;

    let response = client.get(base.clone()).send().await.map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Network,
            format!("Cannot reach Ely.by: {e}"),
        )
    })?;
    if !response.status().is_success() {
        return Err(PrepareError::new(
            PrepareErrorKind::Metadata,
            format!(
                "Ely.by API discovery failed with HTTP {}",
                response.status()
            ),
        ));
    }
    ensure_elyby_url(response.url())
        .map_err(|e| PrepareError::new(PrepareErrorKind::Metadata, e))?;
    let header = response
        .headers()
        .get("x-authlib-injector-api-location")
        .map(|v| {
            v.to_str().map_err(|_| {
                PrepareError::new(
                    PrepareErrorKind::Metadata,
                    "Ely.by returned a non-UTF-8 API Location header",
                )
            })
        })
        .transpose()?;
    let api_root = resolve_api_location(response.url(), header)
        .map_err(|e| PrepareError::new(PrepareErrorKind::Metadata, e))?;

    let metadata = fetch_metadata_json(&client, &api_root).await?;
    validate_metadata(&metadata)
        .map_err(|e| PrepareError::new(PrepareErrorKind::InvalidMetadata, e))?;
    Ok((api_root.to_string(), metadata))
}

/// Testable direct/ALI ordering core. The direct API root is always attempted
/// first; ALI is only invoked after its failure.
async fn fetch_metadata_with<D, DF, A, AF>(
    direct: D,
    ali: A,
) -> Result<(String, Value), PrepareError>
where
    D: FnOnce() -> DF,
    DF: Future<Output = Result<(String, Value), PrepareError>>,
    A: FnOnce() -> AF,
    AF: Future<Output = Result<(String, Value), PrepareError>>,
{
    match direct().await {
        Ok(metadata) => Ok(metadata),
        Err(direct_error) => {
            tracing::warn!(
                target: "launcher",
                "Direct Ely.by metadata failed ({:?}): {}; trying ALI discovery",
                direct_error.kind,
                direct_error.detail
            );
            match ali().await {
                Ok(metadata) => Ok(metadata),
                Err(ali_error) => {
                    let kind = if direct_error.kind == PrepareErrorKind::Network
                        && ali_error.kind != PrepareErrorKind::Network
                    {
                        ali_error.kind
                    } else {
                        direct_error.kind
                    };
                    Err(PrepareError::new(
                        kind,
                        format!(
                            "direct API root: {}; ALI discovery: {}",
                            direct_error.detail, ali_error.detail
                        ),
                    ))
                }
            }
        }
    }
}

/// Direct API root first; ALI is only a fallback. A failure of the main
/// `ely.by` page therefore cannot block a healthy direct endpoint.
async fn fetch_metadata_from_network() -> Result<(String, Value), PrepareError> {
    fetch_metadata_with(fetch_metadata_direct, fetch_metadata_via_ali).await
}

async fn fetch_metadata_json(
    client: &reqwest::Client,
    api_root: &Url,
) -> Result<Value, PrepareError> {
    let response = client.get(api_root.clone()).send().await.map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::Network,
            format!("Cannot fetch Ely.by metadata: {e}"),
        )
    })?;
    if !response.status().is_success() {
        return Err(PrepareError::new(
            PrepareErrorKind::Metadata,
            format!(
                "Ely.by metadata request failed with HTTP {}",
                response.status()
            ),
        ));
    }
    ensure_elyby_url(response.url())
        .map_err(|e| PrepareError::new(PrepareErrorKind::Metadata, e))?;
    if let Some(length) = response.content_length() {
        if length > MAX_METADATA_BYTES {
            return Err(PrepareError::new(
                PrepareErrorKind::Metadata,
                format!("Ely.by metadata is unexpectedly large ({length} bytes)"),
            ));
        }
    }

    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            PrepareError::new(
                PrepareErrorKind::Network,
                format!("Ely.by metadata download interrupted: {e}"),
            )
        })?;
        if body.len() as u64 + chunk.len() as u64 > MAX_METADATA_BYTES {
            return Err(PrepareError::new(
                PrepareErrorKind::Metadata,
                format!("Ely.by metadata exceeded the {MAX_METADATA_BYTES}-byte limit"),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    if body.is_empty() {
        return Err(PrepareError::new(
            PrepareErrorKind::Metadata,
            "Ely.by returned empty metadata",
        ));
    }
    serde_json::from_slice(&body).map_err(|e| {
        PrepareError::new(
            PrepareErrorKind::InvalidMetadata,
            format!("Ely.by returned invalid JSON: {e}"),
        )
    })
}

fn validate_metadata(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Ely.by metadata is not a JSON object".to_string())?;

    let domains = object
        .get("skinDomains")
        .and_then(Value::as_array)
        .ok_or_else(|| "Ely.by metadata has no skinDomains array".to_string())?;
    if domains.is_empty() {
        return Err("Ely.by metadata has an empty skinDomains list".to_string());
    }
    let mut has_elyby_domain = false;
    for domain in domains {
        let domain = domain
            .as_str()
            .ok_or_else(|| "Ely.by skinDomains contains a non-string value".to_string())?;
        validate_skin_domain_rule(domain)?;
        let normalized = domain
            .trim()
            .to_ascii_lowercase()
            .trim_start_matches("*.")
            .trim_start_matches('.')
            .to_string();
        if normalized == "ely.by" || normalized.ends_with(".ely.by") {
            has_elyby_domain = true;
        }
    }
    if !has_elyby_domain {
        return Err("Ely.by metadata does not list an Ely.by skin domain".to_string());
    }

    let key = object
        .get("signaturePublickey")
        .and_then(Value::as_str)
        .ok_or_else(|| "Ely.by metadata has no signaturePublickey".to_string())?;
    let key = key.trim();
    if key.len() < 64 || key.len() > 16 * 1024 {
        return Err("Ely.by signaturePublickey has an unexpected length".to_string());
    }
    if !key.starts_with("-----BEGIN PUBLIC KEY-----") || !key.ends_with("-----END PUBLIC KEY-----")
    {
        return Err("Ely.by signaturePublickey is not a PEM public key".to_string());
    }
    let begin = "-----BEGIN PUBLIC KEY-----";
    let end = "-----END PUBLIC KEY-----";
    let body = &key[begin.len()..key.len() - end.len()];
    if body
        .chars()
        .any(|c| c.is_whitespace() && c != '\n' && c != '\r')
    {
        return Err("Ely.by signaturePublickey contains unexpected whitespace".to_string());
    }

    if let Some(meta) = object.get("meta") {
        let meta = meta
            .as_object()
            .ok_or_else(|| "Ely.by metadata.meta is not an object".to_string())?;
        if let Some(name) = meta.get("serverName") {
            let name = name
                .as_str()
                .ok_or_else(|| "Ely.by meta.serverName is not a string".to_string())?;
            if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
                return Err("Ely.by meta.serverName is invalid".to_string());
            }
        }
    }

    reject_insecure_urls(value)
}

/// Validate one `skinDomains` entry using the documented rule format:
/// exact host, a leading-dot suffix rule, or a leading `*.` wildcard. A bare
/// `*` (which disables the texture whitelist entirely) is deliberately not
/// accepted for the Ely.by integration.
fn validate_skin_domain_rule(rule: &str) -> Result<(), String> {
    let rule = rule.trim();
    if rule.is_empty() || rule == "*" || rule.len() > 253 {
        return Err(format!(
            "Ely.by skinDomains contains an invalid domain rule: {rule}"
        ));
    }
    if rule.contains('/') || rule.contains(':') || rule.contains('@') {
        return Err(format!(
            "Ely.by skinDomains contains an invalid domain rule: {rule}"
        ));
    }
    let domain = if let Some(rest) = rule.strip_prefix("*.") {
        if rest.contains('*') {
            return Err(format!(
                "Ely.by skinDomains contains an invalid wildcard: {rule}"
            ));
        }
        rest
    } else if let Some(rest) = rule.strip_prefix('.') {
        if rest.contains('*') {
            return Err(format!(
                "Ely.by skinDomains contains an invalid wildcard: {rule}"
            ));
        }
        rest
    } else {
        if rule.contains('*') {
            return Err(format!(
                "Ely.by skinDomains contains an invalid wildcard: {rule}"
            ));
        }
        rule
    };
    if domain.is_empty()
        || domain.split('.').any(|part| part.is_empty())
        || !domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return Err(format!(
            "Ely.by skinDomains contains an invalid domain: {rule}"
        ));
    }
    Ok(())
}

fn reject_insecure_urls(value: &Value) -> Result<(), String> {
    match value {
        Value::String(text) => {
            if let Some(index) = text.find("://") {
                let scheme = &text[..index];
                if !scheme.eq_ignore_ascii_case("https") {
                    return Err("Ely.by metadata contains a non-HTTPS URL".to_string());
                }
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_insecure_urls(item)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for item in map.values() {
                reject_insecure_urls(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// ==================== Metadata cache ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetadataCache {
    api_root: String,
    fetched_at: u64,
    metadata: Value,
}

fn metadata_cache_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join("runtime")
        .join("authlib-injector")
        .join("elyby-metadata.json")
}

fn load_metadata_cache(data_dir: &Path) -> Option<MetadataCache> {
    let raw = std::fs::read_to_string(metadata_cache_path(data_dir)).ok()?;
    let cache = serde_json::from_str::<MetadataCache>(&raw).ok()?;
    if cache.fetched_at == 0 {
        return None;
    }
    let api_root = Url::parse(&cache.api_root).ok()?;
    ensure_elyby_url(&api_root).ok()?;
    validate_metadata(&cache.metadata).ok()?;
    Some(cache)
}

fn save_metadata_cache(data_dir: &Path, cache: &MetadataCache) -> Result<(), String> {
    let json = serde_json::to_string_pretty(cache)
        .map_err(|e| format!("Cannot serialize Ely.by metadata cache: {e}"))?;
    crate::instances::atomic_write_file(&metadata_cache_path(data_dir), &json)
        .map_err(|e| format!("Cannot write Ely.by metadata cache: {e}"))
}

fn fresh_metadata_cache(data_dir: &Path) -> Option<(String, Value)> {
    let cache = load_metadata_cache(data_dir)?;
    let age = now_unix().saturating_sub(cache.fetched_at);
    (age <= METADATA_CACHE_TTL_SECS).then_some((cache.api_root, cache.metadata))
}

async fn load_or_fetch_metadata_with<F, Fut>(
    data_dir: &Path,
    fetch: F,
) -> Result<(String, Value), PrepareError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(String, Value), PrepareError>>,
{
    if let Some(fresh) = fresh_metadata_cache(data_dir) {
        return Ok(fresh);
    }

    match fetch().await {
        Ok((api_root, metadata)) => {
            let cache = MetadataCache {
                api_root: api_root.clone(),
                fetched_at: now_unix(),
                metadata: metadata.clone(),
            };
            if let Err(e) = save_metadata_cache(data_dir, &cache) {
                tracing::warn!(target: "launcher", "Cannot cache Ely.by metadata: {}", e);
            }
            Ok((api_root, metadata))
        }
        Err(network_error) => {
            if let Some(cache) = load_metadata_cache(data_dir) {
                let age = now_unix().saturating_sub(cache.fetched_at);
                if age <= METADATA_MAX_STALE_SECS {
                    tracing::warn!(
                        target: "launcher",
                        "Using cached Ely.by metadata (age {}s) after a network failure: {}",
                        age,
                        network_error
                    );
                    return Ok((cache.api_root, cache.metadata));
                }
                tracing::warn!(
                    target: "launcher",
                    "Cached Ely.by metadata is too old ({}s); refusing to use it",
                    age
                );
            }
            Err(PrepareError::new(
                network_error.kind,
                format!(
                    "Cannot obtain valid Ely.by metadata: {}",
                    network_error.detail
                ),
            ))
        }
    }
}

async fn load_or_fetch_metadata(data_dir: &Path) -> Result<(String, Value), PrepareError> {
    load_or_fetch_metadata_with(data_dir, fetch_metadata_from_network).await
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("vl-authlib-{tag}-{}-{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn valid_metadata() -> Value {
        serde_json::json!({
            "meta": {"serverName": "Ely.by"},
            "skinDomains": ["ely.by", ".ely.by"],
            "signaturePublickey": format!(
                "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                "A".repeat(128)
            )
        })
    }

    fn make_jar(version: &str, build: u64, license: &str) -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default();
            writer
                .start_file("META-INF/MANIFEST.MF", options)
                .expect("manifest entry");
            writer
                .write_all(
                    format!(
                        "Manifest-Version: 1.0\r\nPremain-Class: moe.yushi.authlibinjector.Premain\r\nImplementation-Version: {version}\r\nBuild-Number: {build}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .expect("manifest");
            writer
                .start_file("META-INF/licenses/authlib-injector.txt", options)
                .expect("license entry");
            writer.write_all(license.as_bytes()).expect("license");
            writer.finish().expect("finish jar");
        }
        cursor.into_inner()
    }

    fn spec_for(version: &str, build: u64, bytes: &[u8]) -> AgentSpec {
        let dir = temp_dir("spec");
        let path = dir.join("agent.jar");
        std::fs::write(&path, bytes).expect("write spec jar");
        let sha256 = crate::download::hash_file_sha256(&path).expect("hash spec jar");
        let _ = std::fs::remove_dir_all(&dir);
        AgentSpec {
            version: version.to_string(),
            build_number: build,
            url: "https://authlib-injector.yushi.moe/artifact/56/authlib-injector-1.2.8.jar"
                .to_string(),
            sha256,
            size: bytes.len() as u64,
        }
    }

    #[test]
    fn sha256_matches_known_digest() {
        let dir = temp_dir("sha");
        let path = dir.join("abc.bin");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            crate::download::hash_file_sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(crate::download::verify_sha256(
            &path,
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        )
        .unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_jar_passes_size_hash_and_manifest_checks() {
        let dir = temp_dir("valid");
        let bytes = make_jar("1.2.8", 56, "AUTHLIB-INJECTOR exception");
        let path = dir.join("agent.jar");
        std::fs::write(&path, &bytes).unwrap();
        let spec = spec_for("1.2.8", 56, &bytes);
        assert!(validate_jar(&path, &spec).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_html_json_and_wrong_size_are_rejected() {
        let dir = temp_dir("corrupt");
        let spec = spec_for("1.2.8", 56, &make_jar("1.2.8", 56, "AUTHLIB-INJECTOR"));
        for (name, bytes) in [
            ("html.jar", b"<!DOCTYPE html><html>404</html>".to_vec()),
            ("json.jar", br#"{"error":"not found"}"#.to_vec()),
            ("tiny.jar", b"PK".to_vec()),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let error = validate_jar(&path, &spec).unwrap_err();
            assert_eq!(error.kind, PrepareErrorKind::CorruptJar, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_mismatch_with_correct_size_is_rejected() {
        let dir = temp_dir("hash-mismatch");
        let bytes = make_jar("1.2.8", 56, "AUTHLIB-INJECTOR");
        let path = dir.join("agent.jar");
        std::fs::write(&path, &bytes).unwrap();
        let mut spec = spec_for("1.2.8", 56, &bytes);
        spec.sha256 = "0".repeat(64);
        let error = validate_jar(&path, &spec).unwrap_err();
        assert_eq!(error.kind, PrepareErrorKind::Checksum);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_version_and_build_are_pinned() {
        let dir = temp_dir("manifest");
        let bytes = make_jar("9.9.9", 1, "AUTHLIB-INJECTOR");
        let path = dir.join("agent.jar");
        std::fs::write(&path, &bytes).unwrap();
        let spec = spec_for("1.2.8", 56, &bytes);
        let error = validate_jar(&path, &spec).unwrap_err();
        assert_eq!(error.kind, PrepareErrorKind::CorruptJar);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cache_is_reused_and_corruption_forces_one_redownload() {
        let dir = temp_dir("cache");
        let bytes = make_jar("1.2.8", 56, "AUTHLIB-INJECTOR");
        let spec = spec_for("1.2.8", 56, &bytes);
        let calls = Arc::new(AtomicUsize::new(0));
        let downloader: Downloader = {
            let calls = Arc::clone(&calls);
            let bytes = bytes.clone();
            Arc::new(move |part: PathBuf| {
                let calls = Arc::clone(&calls);
                let bytes = bytes.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::fs::write(part, bytes)
                        .map_err(|e| PrepareError::new(PrepareErrorKind::Download, e.to_string()))
                })
            })
        };

        let first = ensure_agent_with(&dir, &spec, Arc::clone(&downloader))
            .await
            .expect("install");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let second = ensure_agent_with(&dir, &spec, Arc::clone(&downloader))
            .await
            .expect("cache hit");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first, second);
        assert!(validate_jar(&second, &spec).is_ok());

        std::fs::write(&second, b"corrupted cache").unwrap();
        let third = ensure_agent_with(&dir, &spec, Arc::clone(&downloader))
            .await
            .expect("repair");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(validate_jar(&third, &spec).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn concurrent_preparation_downloads_once() {
        let dir = temp_dir("concurrent");
        let bytes = make_jar("1.2.8", 56, "AUTHLIB-INJECTOR");
        let spec = Arc::new(spec_for("1.2.8", 56, &bytes));
        let calls = Arc::new(AtomicUsize::new(0));
        let downloader: Downloader = {
            let calls = Arc::clone(&calls);
            Arc::new(move |part: PathBuf| {
                let calls = Arc::clone(&calls);
                let bytes = bytes.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    std::fs::write(part, bytes)
                        .map_err(|e| PrepareError::new(PrepareErrorKind::Download, e.to_string()))
                })
            })
        };

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let dir = dir.clone();
            let spec = Arc::clone(&spec);
            let downloader = Arc::clone(&downloader);
            tasks.push(tokio::spawn(async move {
                ensure_agent_with(&dir, &spec, downloader).await
            }));
        }
        for task in tasks {
            assert!(task.await.expect("task").is_ok());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_errors_serialize_with_stable_kinds() {
        let error = PrepareError::new(PrepareErrorKind::Checksum, "bad hash");
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["kind"], "checksum");
        assert_eq!(json["detail"], "bad hash");

        for (kind, expected) in [
            (PrepareErrorKind::Network, "network"),
            (PrepareErrorKind::Metadata, "metadata"),
            (PrepareErrorKind::Download, "download"),
            (PrepareErrorKind::CorruptJar, "corrupt_jar"),
            (PrepareErrorKind::InvalidMetadata, "invalid_metadata"),
            (PrepareErrorKind::Internal, "internal"),
        ] {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::String(expected.to_string())
            );
        }
    }

    #[tokio::test]
    async fn direct_endpoint_is_attempted_before_ali() {
        let direct_calls = Arc::new(AtomicUsize::new(0));
        let ali_calls = Arc::new(AtomicUsize::new(0));
        let result = fetch_metadata_with(
            {
                let calls = Arc::clone(&direct_calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async { Ok((ELYBY_API_ROOT.to_string(), valid_metadata())) }
                }
            },
            {
                let calls = Arc::clone(&ali_calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async {
                        Err(PrepareError::new(
                            PrepareErrorKind::Network,
                            "ALI must not run when direct succeeds",
                        ))
                    }
                }
            },
        )
        .await
        .expect("direct metadata");
        assert_eq!(result.0, ELYBY_API_ROOT);
        assert_eq!(direct_calls.load(Ordering::SeqCst), 1);
        assert_eq!(ali_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn ali_runs_only_after_direct_failure() {
        let direct_calls = Arc::new(AtomicUsize::new(0));
        let ali_calls = Arc::new(AtomicUsize::new(0));
        let result = fetch_metadata_with(
            {
                let calls = Arc::clone(&direct_calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async {
                        Err::<(String, Value), _>(PrepareError::new(
                            PrepareErrorKind::Network,
                            "direct down",
                        ))
                    }
                }
            },
            {
                let calls = Arc::clone(&ali_calls);
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok((
                            "https://account.ely.by/api/authlib-injector".to_string(),
                            valid_metadata(),
                        ))
                    }
                }
            },
        )
        .await
        .expect("ALI metadata");
        assert_eq!(result.0, ELYBY_API_ROOT);
        assert_eq!(direct_calls.load(Ordering::SeqCst), 1);
        assert_eq!(ali_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fresh_cache_prevents_any_network_fetch() {
        let dir = temp_dir("cache-fresh-no-network");
        let cache = MetadataCache {
            api_root: ELYBY_API_ROOT.to_string(),
            fetched_at: now_unix(),
            metadata: valid_metadata(),
        };
        save_metadata_cache(&dir, &cache).unwrap();

        let called = Arc::new(AtomicUsize::new(0));
        let result = load_or_fetch_metadata_with(&dir, {
            let called = Arc::clone(&called);
            move || {
                called.fetch_add(1, Ordering::SeqCst);
                async {
                    Err::<(String, Value), _>(PrepareError::new(
                        PrepareErrorKind::Network,
                        "must not fetch",
                    ))
                }
            }
        })
        .await
        .expect("fresh cache");
        assert_eq!(result.0, ELYBY_API_ROOT);
        assert_eq!(called.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stale_cache_is_used_only_after_network_failure() {
        let dir = temp_dir("cache-stale-network");
        let cache = MetadataCache {
            api_root: ELYBY_API_ROOT.to_string(),
            fetched_at: now_unix().saturating_sub(METADATA_CACHE_TTL_SECS + 1),
            metadata: valid_metadata(),
        };
        save_metadata_cache(&dir, &cache).unwrap();

        let result = load_or_fetch_metadata_with(&dir, || async {
            Err::<(String, Value), _>(PrepareError::new(PrepareErrorKind::Network, "offline"))
        })
        .await
        .expect("stale cache after network error");
        assert_eq!(result.0, ELYBY_API_ROOT);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cache_older_than_seven_days_is_rejected() {
        let dir = temp_dir("cache-too-old");
        let cache = MetadataCache {
            api_root: ELYBY_API_ROOT.to_string(),
            fetched_at: now_unix().saturating_sub(METADATA_MAX_STALE_SECS + 1),
            metadata: valid_metadata(),
        };
        save_metadata_cache(&dir, &cache).unwrap();

        let error = load_or_fetch_metadata_with(&dir, || async {
            Err::<(String, Value), _>(PrepareError::new(PrepareErrorKind::Network, "offline"))
        })
        .await
        .expect_err("cache older than seven days must be rejected");
        assert_eq!(error.kind, PrepareErrorKind::Network);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn corrupt_cache_is_ignored_and_never_used() {
        let dir = temp_dir("cache-corrupt-fetch");
        std::fs::create_dir_all(dir.join("runtime").join("authlib-injector")).unwrap();
        std::fs::write(metadata_cache_path(&dir), b"{broken").unwrap();

        let fetched = valid_metadata();
        let result = load_or_fetch_metadata_with(&dir, {
            let fetched = fetched.clone();
            move || async move {
                Ok((
                    "https://account.ely.by/api/authlib-injector".to_string(),
                    fetched,
                ))
            }
        })
        .await
        .expect("network fallback after corrupt cache");
        assert_eq!(result.1, fetched);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn direct_api_root_is_canonical_and_allowed() {
        let root = Url::parse(ELYBY_API_ROOT).unwrap();
        assert_eq!(root.host_str(), Some("account.ely.by"));
        assert!(ensure_elyby_url(&root).is_ok());
        // Direct discovery must not point at the main HTML page.
        assert_ne!(root.host_str(), Some("ely.by"));
    }

    #[tokio::test]
    #[ignore = "network: verifies the direct Ely.by API root"]
    async fn live_direct_elyby_metadata_is_valid() {
        let (api_root, metadata) = fetch_metadata_direct()
            .await
            .expect("direct Ely.by metadata");
        assert_eq!(api_root, ELYBY_API_ROOT);
        assert!(validate_metadata(&metadata).is_ok());
    }

    #[tokio::test]
    #[ignore = "network: prepares the real pinned agent and live metadata"]
    async fn live_prepare_succeeds() {
        let dir = temp_dir("live-prepare");
        let agent = prepare(&dir)
            .await
            .expect("prepare live Ely.by integration");
        assert_eq!(agent.api_root, ELYBY_API_ROOT);
        assert!(agent.jar.is_file());
        assert!(validate_jar(&agent.jar, &pinned_spec()).is_ok());
        assert!(!agent.prefetched_metadata.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[ignore = "network: compares direct and ALI metadata"]
    async fn live_direct_and_ali_metadata_match() {
        let (direct_root, direct) = fetch_metadata_direct().await.expect("direct metadata");
        let (ali_root, ali) = fetch_metadata_via_ali().await.expect("ALI metadata");
        assert_eq!(direct_root, ELYBY_API_ROOT);
        assert_eq!(ali_root, direct_root);
        assert_eq!(direct["skinDomains"], ali["skinDomains"]);
        assert_eq!(direct["signaturePublickey"], ali["signaturePublickey"]);
        assert_eq!(direct["meta"], ali["meta"]);
    }

    #[tokio::test]
    #[ignore = "network: verifies live Ely.by ALI + metadata"]
    async fn live_elyby_metadata_is_valid() {
        let (api_root, metadata) = fetch_metadata_from_network()
            .await
            .expect("live Ely.by metadata");
        assert!(
            api_root.starts_with("https://account.ely.by/")
                || api_root.starts_with("https://authserver.ely.by/"),
            "unexpected Ely.by API root: {api_root}"
        );
        assert!(validate_metadata(&metadata).is_ok());
    }

    #[tokio::test]
    #[ignore = "network: verifies the real pinned production artifact"]
    async fn pinned_agent_downloads_and_verifies() {
        let dir = temp_dir("pinned-live");
        let path = ensure_agent(&dir).await.expect("download pinned agent");
        assert_eq!(path, agent_path(&dir, VERSION));
        assert!(validate_jar(&path, &pinned_spec()).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ali_absolute_relative_and_missing_are_resolved() {
        let base = Url::parse("https://ely.by/").unwrap();
        assert_eq!(resolve_api_location(&base, None).unwrap(), base);
        assert_eq!(
            resolve_api_location(
                &base,
                Some("https://authserver.ely.by/api/authlib-injector")
            )
            .unwrap()
            .as_str(),
            "https://authserver.ely.by/api/authlib-injector"
        );
        assert_eq!(
            resolve_api_location(&base, Some("/api/authlib-injector"))
                .unwrap()
                .as_str(),
            "https://ely.by/api/authlib-injector"
        );
    }

    #[test]
    fn ali_rejects_http_foreign_hosts_and_control_characters() {
        let base = Url::parse("https://ely.by/").unwrap();
        for header in [
            "http://authserver.ely.by/api/authlib-injector",
            "https://evil.example/api",
            "https://authserver.ely.by/api\nInjected: yes",
        ] {
            assert!(
                resolve_api_location(&base, Some(header)).is_err(),
                "header must be rejected: {header:?}"
            );
        }
    }

    #[test]
    fn metadata_requires_elyby_skin_domain_and_public_key() {
        let valid = serde_json::json!({
            "serverName": "Ely.by",
            "skinDomains": ["ely.by", "skinsystem.ely.by"],
            "signaturePublickey": format!(
                "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                "A".repeat(128)
            ),
            "features": {"legacy_skin_api": true}
        });
        assert!(
            validate_metadata(&valid).is_ok(),
            "{:?}",
            validate_metadata(&valid)
        );

        for rule in [".ely.by", "*.ely.by", "skinsystem.ely.by"] {
            let with_rule = serde_json::json!({
                "skinDomains": [rule],
                "signaturePublickey": format!(
                    "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                    "A".repeat(128)
                )
            });
            assert!(validate_metadata(&with_rule).is_ok(), "{rule}");
        }

        let no_domains = serde_json::json!({"signaturePublickey": "x".repeat(128)});
        assert!(validate_metadata(&no_domains).is_err());

        for rule in ["*", "ely.by/path", "https://ely.by"] {
            let bad_rule = serde_json::json!({
                "skinDomains": [rule],
                "signaturePublickey": format!(
                    "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                    "A".repeat(128)
                )
            });
            assert!(validate_metadata(&bad_rule).is_err(), "{rule}");
        }

        let not_pem = serde_json::json!({
            "skinDomains": ["ely.by"],
            "signaturePublickey": "A".repeat(128)
        });
        assert!(validate_metadata(&not_pem).is_err());

        let empty_domains =
            serde_json::json!({"skinDomains": [], "signaturePublickey": "x".repeat(128)});
        assert!(validate_metadata(&empty_domains).is_err());

        let foreign = serde_json::json!({
            "skinDomains": ["example.com"],
            "signaturePublickey": "x".repeat(128)
        });
        assert!(validate_metadata(&foreign).is_err());

        let short_key = serde_json::json!({
            "skinDomains": ["ely.by"],
            "signaturePublickey": "short"
        });
        assert!(validate_metadata(&short_key).is_err());

        let insecure = serde_json::json!({
            "skinDomains": ["ely.by"],
            "signaturePublickey": format!(
                "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                "A".repeat(128)
            ),
            "unexpected": "http://ely.by/skin.png"
        });
        assert!(validate_metadata(&insecure).is_err());
    }

    #[test]
    fn real_elyby_metadata_shape_is_accepted() {
        // Public metadata returned by https://account.ely.by/api/authlib-injector
        // (the ALI target advertised by https://ely.by). This pins the actual
        // shape: meta.serverName, "ely.by" + ".ely.by" skin domains, and a PEM
        // signature key with line breaks.
        let raw = r#"{"meta":{"serverName":"Ely.by","implementationName":"Account Ely.by adapter for the authlib-injector library","implementationVersion":"1.0.0","feature.no_mojang_namespace":true,"feature.enable_profile_key":true,"links":{"homepage":"https://ely.by","register":"https://account.ely.by/register"}},"skinDomains":["ely.by",".ely.by"],"signaturePublickey":"-----BEGIN PUBLIC KEY-----\nMIICIjANBgkqhkiG9w0BAQEFAAOCAg8AMIICCgKCAgEAxgFJRb0e9fRyVG5+JlCg\nh0hccRIcgO5yxEVkMJajAI12Ev/Pc7lpTt6OtKTEcUNfjYgBnEhIKbdLD0Z+B5Bx\nSg9DQmozgzZcesScpASQb4Kt6P8itowdbgbUm4v+6x1QUKJjjmhHq93m9OIEbxQL\nCq+SrEMZpDrXRgd9DhNPjZv/95ximP8otvh7+bmEl8jwINgfJx0PAeJFYlceQcsh\niYh+LHtaIwzbTTqkDibDm7QiEc+/qGab3mABtVTpqw/refwFoR0M8+xkWF+1/D8k\nH0WFa+rBhdjLyLG+2hdOpKXoH/fMH0tQMPHU78J17JVKWwIWCwEWXp8HiWSbIt3a\ncmBYtyW3tqarFFMMECx2wmJP6FVOvYVThZxq9qc9/f3yeTGz3g7zU1YljHSVRP16\niEbEnHQBKxmrj2cdZgosJej4YppV7f3iZ8o8PF6UY51LSqvaCteXuWeYSJJESGAs\nUoV7ihJfWL8DymHamywB2Cahx7EiDGS3/iBcQUmpk4TTg2FrZPuKGItn1QfIRieO\nknnj9CPKiWdfOtJBr3i1FXLEfExgcJhQ00Y6B08QVvgiCzUF3t+VAG3Ef2YINYyG\nAXcW0TIgMalwwgGzdhQRhItODXptWigy0DNTUAgKQT9PS8N09yPBGxIq64T9A3/z\nFqC/k2bMLWUSVtIlilIItn0CAwEAAQ==\n-----END PUBLIC KEY-----\n"}"#;
        let value: Value = serde_json::from_str(raw).expect("real Ely.by metadata JSON");
        validate_metadata(&value).expect("real Ely.by metadata must validate");
        assert_eq!(value["meta"]["serverName"], "Ely.by");
        assert!(value["skinDomains"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == ".ely.by"));
    }

    #[test]
    fn metadata_cache_roundtrip_and_invalid_cache_is_ignored() {
        let dir = temp_dir("metadata-cache");
        let cache = MetadataCache {
            api_root: "https://authserver.ely.by/api/authlib-injector".to_string(),
            fetched_at: now_unix(),
            metadata: serde_json::json!({
                "skinDomains": ["ely.by"],
                "signaturePublickey": format!(
                    "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                    "A".repeat(128)
                )
            }),
        };
        save_metadata_cache(&dir, &cache).expect("save");
        let loaded = load_metadata_cache(&dir).expect("load");
        assert_eq!(loaded.api_root, cache.api_root);
        assert_eq!(loaded.metadata, cache.metadata);

        std::fs::write(metadata_cache_path(&dir), b"{broken").unwrap();
        assert!(load_metadata_cache(&dir).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn metadata_cache_respects_its_ttl() {
        let dir = temp_dir("metadata-ttl");
        let mut cache = MetadataCache {
            api_root: "https://authserver.ely.by/api/authlib-injector".to_string(),
            fetched_at: now_unix(),
            metadata: serde_json::json!({
                "skinDomains": ["ely.by"],
                "signaturePublickey": format!(
                    "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                    "A".repeat(128)
                )
            }),
        };
        save_metadata_cache(&dir, &cache).unwrap();
        assert!(fresh_metadata_cache(&dir).is_some());

        cache.fetched_at = now_unix().saturating_sub(METADATA_CACHE_TTL_SECS + 1);
        save_metadata_cache(&dir, &cache).unwrap();
        assert!(fresh_metadata_cache(&dir).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn urls_must_stay_on_https_and_the_expected_hosts() {
        for url in ["http://ely.by/", "https://evil.example/"] {
            assert!(ensure_elyby_url(&Url::parse(url).unwrap()).is_err());
        }
        assert!(ensure_elyby_url(&Url::parse("https://authserver.ely.by/").unwrap()).is_ok());
        assert!(ensure_elyby_url(
            &Url::parse("https://account.ely.by/api/authlib-injector").unwrap()
        )
        .is_ok());
        for url in [
            "http://authlib-injector.yushi.moe/x.jar",
            "https://evil.example/x.jar",
        ] {
            assert!(ensure_injector_url(&Url::parse(url).unwrap()).is_err());
        }
        assert!(ensure_injector_url(
            &Url::parse("https://authlib-injector.yushi.moe/artifact/56/x.jar").unwrap()
        )
        .is_ok());
    }

    #[test]
    fn version_gate_matches_the_documented_minimum() {
        for supported in ["1.7.2", "1.7.10", "1.12.2", "1.20.1", "26.3-snapshot-2"] {
            assert!(is_supported_mc_version(supported), "{supported}");
        }
        for unsupported in ["1.6.4", "1.7", "a1.2.6", "b1.7.3", ""] {
            assert!(!is_supported_mc_version(unsupported), "{unsupported}");
        }
    }

    #[test]
    fn integration_is_limited_to_enabled_supported_elyby_accounts() {
        assert!(should_attach(true, true, "1.20.1"));
        assert!(!should_attach(false, true, "1.20.1"));
        assert!(!should_attach(true, false, "1.20.1"));
        assert!(!should_attach(true, true, "1.6.4"));
    }

    #[test]
    fn prefetched_metadata_is_standard_base64() {
        assert_eq!(crate::instances::base64_encode(b"abc"), "YWJj");
        assert_eq!(crate::instances::base64_encode(b"{}"), "e30=");
    }
}

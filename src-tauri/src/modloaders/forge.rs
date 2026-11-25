use super::processor_cache::{
    checked_relative, file_digest, relative_identity, relative_to, sha256_bytes, ExpectedOutput,
    GenerationInspection, GenerationIssue, GenerationManifest, GenerationState, InstallerIdentity,
    OutputIdentity, OutputSpec, ProcessorCache, ProcessorIdentity, ProcessorLibraryIdentity,
    TransactionState,
};
use super::{LoaderLibrary, LoaderProfile, LoaderVersionPage};
use crate::error::{LauncherError, Result};
use crate::integrity::HashSpec;
use crate::versions::maven_to_path;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

/// Forge installer profile
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeInstallProfile {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "mainClass", default)]
    main_class: Option<String>,
    #[serde(default)]
    libraries: Vec<ForgeLibrary>,
    #[serde(default)]
    arguments: Option<ForgeArguments>,
    #[serde(rename = "minecraftArguments", default)]
    minecraft_arguments: Option<String>,
    #[serde(default)]
    inherits_from: Option<String>,
    #[serde(default)]
    json: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeLibrary {
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    downloads: Option<ForgeDownloads>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeDownloads {
    artifact: Option<ForgeArtifact>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeArtifact {
    path: Option<String>,
    url: Option<String>,
    sha1: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    sha512: Option<String>,
    size: Option<u64>,
}

/// The three distinct meanings of an installer artifact's `url` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForgeArtifactUrl<'a> {
    /// `url: Some("")` — explicitly embedded in the verified installer.
    Embedded,
    /// `url: Some(valid URL)` — an ordinary remote artifact.
    Remote(&'a str),
    /// `url: None` — the historical Maven fallback. It is never embedded.
    Missing,
}

fn classify_artifact_url(url: Option<&str>) -> Result<ForgeArtifactUrl<'_>> {
    match url {
        Some("") => Ok(ForgeArtifactUrl::Embedded),
        Some(url) if url.trim().is_empty() => Err(LauncherError::ModLoader(
            "Forge artifact URL is whitespace-only; it is neither embedded nor a valid Maven URL"
                .into(),
        )),
        Some(url) => Ok(ForgeArtifactUrl::Remote(url)),
        None => Ok(ForgeArtifactUrl::Missing),
    }
}

fn classify_artifact_value_url(value: Option<&serde_json::Value>) -> Result<ForgeArtifactUrl<'_>> {
    match value {
        None => Ok(ForgeArtifactUrl::Missing),
        Some(serde_json::Value::String(url)) => classify_artifact_url(Some(url)),
        Some(_) => Err(LauncherError::ModLoader(
            "Forge artifact URL is not a string".into(),
        )),
    }
}

impl ForgeArtifact {
    fn url_kind(&self) -> Result<ForgeArtifactUrl<'_>> {
        classify_artifact_url(self.url.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ForgeArguments {
    #[serde(default)]
    game: Vec<serde_json::Value>,
    #[serde(default)]
    jvm: Vec<serde_json::Value>,
}

/// An artifact that Forge metadata explicitly marks as embedded in the
/// installer (`downloads.artifact.url` is exactly empty).
#[derive(Debug, Clone)]
struct EmbeddedLibrarySpec {
    name: String,
    relative_path: String,
    sha1: Option<String>,
    sha256: Option<String>,
    sha512: Option<String>,
    size: Option<u64>,
}

impl EmbeddedLibrarySpec {
    fn checksum(&self) -> Result<Option<HashSpec>> {
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

const MAX_EMBEDDED_LIBRARY_BYTES: u64 = 256 * 1024 * 1024;

fn merge_embedded_spec(
    existing: &mut EmbeddedLibrarySpec,
    incoming: &EmbeddedLibrarySpec,
) -> Result<()> {
    for (label, old, new) in [
        ("SHA-1", &mut existing.sha1, incoming.sha1.as_ref()),
        ("SHA-256", &mut existing.sha256, incoming.sha256.as_ref()),
        ("SHA-512", &mut existing.sha512, incoming.sha512.as_ref()),
    ] {
        if let Some(new) = new {
            if let Some(existing_value) = old.as_mut() {
                if !existing_value.eq_ignore_ascii_case(new) {
                    return Err(LauncherError::ModLoader(format!(
                        "Embedded Forge library {} is declared with conflicting {label} checksums",
                        incoming.relative_path
                    )));
                }
            } else {
                *old = Some(new.clone());
            }
        }
    }
    if existing.size.is_none() {
        existing.size = incoming.size;
    } else if let (Some(old), Some(new)) = (existing.size, incoming.size) {
        if old != new {
            return Err(LauncherError::ModLoader(format!(
                "Embedded Forge library {} is declared with conflicting sizes",
                incoming.relative_path
            )));
        }
    }
    Ok(())
}

fn embedded_specs_from_libraries(libraries: &[ForgeLibrary]) -> Result<Vec<EmbeddedLibrarySpec>> {
    let mut specs: Vec<EmbeddedLibrarySpec> = Vec::new();
    for library in libraries {
        let Some(artifact) = library
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.artifact.as_ref())
        else {
            continue;
        };
        // Only an explicitly empty URL means "embedded in this installer".
        // A missing URL is the historical Maven fallback and must not be
        // guessed into an archive entry.
        if artifact.url_kind()? != ForgeArtifactUrl::Embedded {
            continue;
        }
        let relative_path = artifact
            .path
            .as_deref()
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| maven_to_path(&library.name));
        crate::import::check_safe_remote_path(&relative_path).map_err(|e| {
            LauncherError::ModLoader(format!(
                "Unsafe embedded Forge library path {:?}: {e}",
                relative_path
            ))
        })?;
        let spec = EmbeddedLibrarySpec {
            name: library.name.clone(),
            relative_path,
            sha1: artifact.sha1.clone().filter(|value| !value.is_empty()),
            sha256: artifact.sha256.clone().filter(|value| !value.is_empty()),
            sha512: artifact.sha512.clone().filter(|value| !value.is_empty()),
            size: artifact.size,
        };
        // Validate the metadata classification here, once. A malformed
        // non-empty checksum is an integrity issue, not "no checksum".
        spec.checksum()?;
        if let Some(existing) = specs
            .iter_mut()
            .find(|existing| existing.relative_path == spec.relative_path)
        {
            merge_embedded_spec(existing, &spec)?;
            continue;
        }
        specs.push(spec);
    }
    Ok(specs)
}

fn collect_embedded_libraries(
    install_profile: &serde_json::Value,
    version_json: Option<&str>,
) -> Result<Vec<EmbeddedLibrarySpec>> {
    let mut specs = Vec::new();
    let install_profile: ForgeInstallProfile = serde_json::from_value(install_profile.clone())
        .map_err(|error| {
            LauncherError::ModLoader(format!(
                "Failed to parse install_profile.json for embedded libraries: {error}"
            ))
        })?;
    specs.extend(embedded_specs_from_libraries(&install_profile.libraries)?);
    if let Some(version_json) = version_json {
        let version_profile: ForgeInstallProfile =
            serde_json::from_str(version_json).map_err(|error| {
                LauncherError::ModLoader(format!(
                    "Failed to parse version.json for embedded libraries: {error}"
                ))
            })?;
        for spec in embedded_specs_from_libraries(&version_profile.libraries)? {
            if let Some(existing) = specs
                .iter_mut()
                .find(|existing| existing.relative_path == spec.relative_path)
            {
                merge_embedded_spec(existing, &spec)?;
            } else {
                specs.push(spec);
            }
        }
    }
    specs.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(specs)
}

fn embedded_target(libraries_dir: &Path, relative_path: &str) -> Result<PathBuf> {
    crate::import::check_safe_remote_path(relative_path).map_err(|e| {
        LauncherError::ModLoader(format!(
            "Unsafe embedded Forge library path {:?}: {e}",
            relative_path
        ))
    })?;
    let target = libraries_dir.join(relative_path);
    if !target.starts_with(libraries_dir) || target == libraries_dir {
        return Err(LauncherError::ModLoader(format!(
            "Embedded Forge library path escapes the libraries directory: {relative_path}"
        )));
    }
    crate::import::ensure_no_reparse_points(libraries_dir, &target)?;
    Ok(target)
}

fn read_installer_entry_bytes(installer_path: &Path, relative_path: &str) -> Result<Vec<u8>> {
    let entry_name = format!("maven/{relative_path}");
    if crate::download::is_unsafe_archive_path(relative_path)
        || crate::download::is_unsafe_archive_path(&entry_name)
    {
        return Err(LauncherError::ModLoader(format!(
            "Refusing unsafe installer entry name: {entry_name}"
        )));
    }
    let bytes = std::fs::read(installer_path).map_err(|e| {
        LauncherError::ModLoader(format!(
            "Failed to read verified installer {}: {e}",
            installer_path.display()
        ))
    })?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid installer JAR: {e}")))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| LauncherError::ModLoader(format!("Invalid installer entry: {e}")))?;
        let Some(name) = entry.name().ok().map(|name| name.replace('\\', "/")) else {
            continue;
        };
        if name != entry_name {
            continue;
        }
        if entry.size() > MAX_EMBEDDED_LIBRARY_BYTES {
            return Err(LauncherError::ModLoader(format!(
                "Embedded installer entry is too large: {entry_name} ({} bytes)",
                entry.size()
            )));
        }
        let mut payload = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut payload).map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to read embedded installer entry {entry_name}: {e}"
            ))
        })?;
        if payload.len() as u64 != entry.size() {
            return Err(LauncherError::ModLoader(format!(
                "Embedded installer entry has an unexpected size: {entry_name}"
            )));
        }
        return Ok(payload);
    }
    Err(LauncherError::ModLoader(format!(
        "Installer does not contain the embedded Forge entry {entry_name}"
    )))
}

fn verify_embedded_bytes(spec: &EmbeddedLibrarySpec, bytes: &[u8]) -> Result<()> {
    if let Some(size) = spec.size.filter(|size| *size > 0) {
        if bytes.len() as u64 != size {
            return Err(LauncherError::ModLoader(format!(
                "Embedded Forge library {} has size {} but metadata declares {size}",
                spec.relative_path,
                bytes.len()
            )));
        }
    }
    if let Some(hash) = spec.checksum()? {
        if !hash.verify_bytes(bytes) {
            return Err(LauncherError::ModLoader(format!(
                "Embedded Forge library {} does not match its metadata checksum",
                spec.relative_path
            )));
        }
    }
    Ok(())
}

fn existing_embedded_matches(spec: &EmbeddedLibrarySpec, target: &Path, bytes: &[u8]) -> bool {
    if !target.is_file() {
        return false;
    }
    if let Some(size) = spec.size.filter(|size| *size > 0) {
        if std::fs::metadata(target).map(|m| m.len()).ok() != Some(size) {
            return false;
        }
    }
    match spec.checksum() {
        Ok(Some(hash)) => return hash.verify_file(target).unwrap_or(false),
        Err(_) => return false,
        Ok(None) => {}
    }
    file_digest(target)
        .map(|digest| digest.sha256 == sha256_bytes(bytes))
        .unwrap_or(false)
}

/// Ensure every artifact explicitly marked embedded in the Forge metadata is
/// present and verified against the verified installer. The caller must hold
/// the per-cache operation lock; all three call sites do so through
/// `ensure_processor_jars`.
pub(crate) fn ensure_embedded_forge_libraries(
    installer_path: &Path,
    install_profile: &serde_json::Value,
    version_json: Option<&str>,
    libraries_dir: &Path,
) -> Result<usize> {
    let specs = collect_embedded_libraries(install_profile, version_json)?;
    let mut changed = 0usize;
    for spec in specs {
        let target = embedded_target(libraries_dir, &spec.relative_path)?;
        let bytes = read_installer_entry_bytes(installer_path, &spec.relative_path)?;
        verify_embedded_bytes(&spec, &bytes)?;
        if existing_embedded_matches(&spec, &target, &bytes) {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
            crate::import::ensure_no_reparse_points(libraries_dir, parent)?;
        }
        crate::instances::write_file_atomic(&target, &bytes)?;
        if !existing_embedded_matches(&spec, &target, &bytes) {
            return Err(LauncherError::ModLoader(format!(
                "Published embedded Forge library failed verification: {}",
                target.display()
            )));
        }
        changed += 1;
        tracing::info!(
            target: "launcher",
            "Recovered embedded Forge library {} from the verified installer",
            spec.relative_path
        );
    }
    Ok(changed)
}

/// Read-only counterpart used by Verify. It never extracts or repairs; it
/// reports missing/corrupt embedded artifacts and installer-entry mismatches.
pub(crate) fn inspect_embedded_forge_libraries(
    installer_path: &Path,
    install_profile: &serde_json::Value,
    version_json: Option<&str>,
    libraries_dir: &Path,
) -> Vec<GenerationIssue> {
    let mut issues = Vec::new();
    let specs = match collect_embedded_libraries(install_profile, version_json) {
        Ok(specs) => specs,
        Err(error) => {
            issues.push(GenerationIssue {
                path: "embedded-forge-libraries".to_string(),
                kind: "invalid_embedded_metadata".to_string(),
                detail: Some(error.to_string()),
                repairable: false,
            });
            return issues;
        }
    };
    for spec in specs {
        let target = match embedded_target(libraries_dir, &spec.relative_path) {
            Ok(target) => target,
            Err(error) => {
                issues.push(GenerationIssue {
                    path: spec.relative_path,
                    kind: "unsafe_embedded_library".to_string(),
                    detail: Some(error.to_string()),
                    repairable: false,
                });
                continue;
            }
        };
        if !target.is_file() {
            issues.push(GenerationIssue {
                path: spec.relative_path,
                kind: "missing_embedded_library".to_string(),
                detail: Some(format!("embedded Forge library {} is missing", spec.name)),
                repairable: true,
            });
            continue;
        }
        if let Some(size) = spec.size.filter(|size| *size > 0) {
            if std::fs::metadata(&target).map(|m| m.len()).ok() != Some(size) {
                issues.push(GenerationIssue {
                    path: spec.relative_path,
                    kind: "corrupt_embedded_library".to_string(),
                    detail: Some("embedded Forge library size does not match metadata".to_string()),
                    repairable: true,
                });
                continue;
            }
        }
        let checksum = match spec.checksum() {
            Ok(checksum) => checksum,
            Err(error) => {
                issues.push(GenerationIssue {
                    path: spec.relative_path,
                    kind: "invalid_embedded_metadata".to_string(),
                    detail: Some(format!(
                        "embedded Forge library has malformed checksum metadata: {error}"
                    )),
                    repairable: false,
                });
                continue;
            }
        };
        if let Some(hash) = checksum {
            match hash.verify_file(&target) {
                Ok(true) => {}
                Ok(false) => issues.push(GenerationIssue {
                    path: spec.relative_path,
                    kind: "corrupt_embedded_library".to_string(),
                    detail: Some("embedded Forge library failed its metadata checksum".to_string()),
                    repairable: true,
                }),
                Err(error) => issues.push(GenerationIssue {
                    path: spec.relative_path,
                    kind: "unreadable_embedded_library".to_string(),
                    detail: Some(error.to_string()),
                    repairable: true,
                }),
            }
            continue;
        }
        match read_installer_entry_bytes(installer_path, &spec.relative_path) {
            Ok(bytes) => {
                let actual = file_digest(&target).map(|digest| digest.sha256).ok();
                if actual.as_deref() != Some(sha256_bytes(&bytes).as_str()) {
                    issues.push(GenerationIssue {
                        path: spec.relative_path,
                        kind: "corrupt_embedded_library".to_string(),
                        detail: Some(
                            "embedded Forge library does not match the verified installer entry"
                                .to_string(),
                        ),
                        repairable: true,
                    });
                }
            }
            Err(error) => issues.push(GenerationIssue {
                path: spec.relative_path,
                kind: "installer_entry_missing".to_string(),
                detail: Some(error.to_string()),
                repairable: false,
            }),
        }
    }
    issues
}

/// Fetch a page of available Forge versions for a MC version.
pub async fn get_loader_versions(
    mc_version: &str,
    offset: usize,
    limit: usize,
) -> Result<LoaderVersionPage> {
    super::prism_meta::fetch_loader_versions("net.minecraftforge", Some(mc_version), offset, limit)
        .await
}

async fn download_installer_cached(
    mc_version: &str,
    forge_version: &str,
    cache: &ProcessorCache,
) -> Result<PathBuf> {
    download_installer_cached_inner(mc_version, forge_version, cache, false).await
}

/// Вариант для `ensure_processor_jars`, который уже владеет cache operation
/// lock. Installer archive и его identity публикуются в одной critical section,
/// поэтому два instance не смогут одновременно опубликовать один loader cache.
async fn download_installer_cached_locked(
    mc_version: &str,
    forge_version: &str,
    cache: &ProcessorCache,
) -> Result<PathBuf> {
    download_installer_cached_inner(mc_version, forge_version, cache, true).await
}

async fn download_installer_cached_inner(
    mc_version: &str,
    forge_version: &str,
    cache: &ProcessorCache,
    locked: bool,
) -> Result<PathBuf> {
    super::validate_loader_request(mc_version, forge_version)?;
    let full_version = full_forge_version(mc_version, forge_version);

    // Legacy Forge (MC 1.8.x and older) publishes some builds under the
    // `{mc}-{build}-{mc}` maven version (e.g. 1.8.9-11.15.1.2318-1.8.9)
    // instead of `{mc}-{build}`.
    let legacy_full_version = if forge_version.contains('-') {
        forge_version.to_string()
    } else {
        format!("{}-{}-{}", mc_version, forge_version, mc_version)
    };

    let installer_urls = vec![
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            full_version, full_version
        ),
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            legacy_full_version, legacy_full_version
        ),
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            full_version, forge_version
        ),
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{}/forge-{}-installer.jar",
            forge_version, forge_version
        ),
    ];

    // The persistent cache keeps both the verified JAR and its provenance
    // across %TEMP% cleanup. A cached file without a matching provenance
    // record is re-downloaded and re-verified.
    if locked {
        cache
            .ensure_installer_locked(
                &installer_urls,
                super::artifact::FORGE_HOSTS,
                &format!("Forge installer {full_version}"),
            )
            .await?;
    } else {
        cache
            .ensure_installer(
                &installer_urls,
                super::artifact::FORGE_HOSTS,
                &format!("Forge installer {full_version}"),
            )
            .await?;
    }

    // The checksum proves the exact Maven bytes; the structural check is an
    // additional guard against an installer that is authentic but unusable.
    // If the cached bytes are structurally broken, discard the provenance and
    // retry the verified download once instead of surfacing a dead cache.
    if !installer_jar_is_valid(&cache.installer) {
        let _ = std::fs::remove_file(&cache.installer);
        let _ = std::fs::remove_file(&cache.installer_identity);
        if locked {
            cache
                .ensure_installer_locked(
                    &installer_urls,
                    super::artifact::FORGE_HOSTS,
                    &format!("Forge installer {full_version}"),
                )
                .await?;
        } else {
            cache
                .ensure_installer(
                    &installer_urls,
                    super::artifact::FORGE_HOSTS,
                    &format!("Forge installer {full_version}"),
                )
                .await?;
        }
        if !installer_jar_is_valid(&cache.installer) {
            let _ = std::fs::remove_file(&cache.installer);
            let _ = std::fs::remove_file(&cache.installer_identity);
            return Err(LauncherError::ModLoader(format!(
                "Downloaded Forge installer {full_version} is not a valid installer archive"
            )));
        }
    }
    Ok(cache.installer.clone())
}

fn full_forge_version(mc_version: &str, forge_version: &str) -> String {
    if forge_version.contains('-') {
        forge_version.to_string()
    } else {
        format!("{}-{}", mc_version, forge_version)
    }
}

/// A cached installer is usable only if it opens as a ZIP and carries one of
/// the metadata files the profile/processor code expects. Size alone is not
/// enough: a truncated download or an HTML error page can easily exceed 1 KB.
pub(crate) fn installer_jar_is_valid(path: &Path) -> bool {
    let ok_meta = std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() > 1000)
        .unwrap_or(false);
    if !ok_meta {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return false;
    };
    archive.by_name("install_profile.json").is_ok() || archive.by_name("version.json").is_ok()
}

/// Java major version expected by Forge for a MC version:
/// 1.8–1.16.5 → 8, 1.17 → 16, 1.18–1.20.4 → 17, 1.20.5+ → 21.
pub(crate) fn required_forge_java_major(mc_version: &str) -> u32 {
    let parts: Vec<&str> = mc_version.trim_start_matches("1.").split('.').collect();
    let minor: u32 = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch: u32 = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    match (minor, patch) {
        (m, _) if m < 17 => 8,
        (17, _) => 16,
        (m, p) if m < 20 || (m == 20 && p < 5) => 17,
        _ => 21,
    }
}

/// Pick a Java for the Forge installer processor pipeline.
///
/// Processors are version-sensitive in BOTH directions: old ForgeGradle
/// dies on a too-new JVM with cryptic "Unsupported class file major
/// version" / "did not generate" errors, while new installers need at least
/// their required major. So an EXACT major is required below 21, and 21 is
/// treated as a minimum (modern installers run on 22+). The old
/// closest-higher matching handed Java 21 to 1.16-era ForgeGradle. Falls
/// back to any system Java only when nothing suitable is installed.
pub(crate) fn find_java_for(data_dir: &Path, mc_version: &str) -> Option<PathBuf> {
    let want = required_forge_java_major(mc_version);
    // Processors need their own Java (often newer than the game launch uses)
    // and — uniquely in this codebase — may run on the closest NEWER major,
    // but only for wants of Java 21+. This requirement is the Forge-order
    // one and is never mixed with the game's JVM choice.
    let req = crate::java::JavaRequirement {
        major: want,
        arch: None,
        require_64bit: true,
        allow_newer: want >= 21,
    };
    let all = crate::launch::find_all_java_installations(data_dir);
    match crate::java::resolve_java(&req, None, None, &all) {
        crate::java::JavaChoice::Selected { install } => {
            tracing::debug!(target: "launcher", "Using Java {} (v{}) for Forge processors (want {})",
                install.major_version, install.version, want);
            Some(install.path)
        }
        _ => None,
    }
}

fn missing_processor_java_message(major: u32, can_download: bool) -> String {
    if can_download {
        format!(
            "Java {major} is required to run the Forge installers, but no compatible Java is installed and the launcher's managed Java download did not produce a usable runtime. Install Java {major} manually or retry the download."
        )
    } else {
        format!(
            "Java {major} is required to run the Forge installers, but no compatible Java is installed. Install a matching JDK, or run the operation through the launcher so it can prepare its managed Java runtime."
        )
    }
}

/// The single preparation point for the Forge/NeoForge processor JVM.
///
/// Discovery is unchanged (`find_java_for`); only when it returns `None` does
/// this reuse the existing managed-Java download and re-run discovery. It
/// never changes system-wide Java and never substitutes a user-configured
/// Java path for the processor pipeline.
pub(crate) async fn ensure_java_for_processors(
    data_dir: &Path,
    mc_version: &str,
    app: Option<&tauri::AppHandle>,
) -> Result<PathBuf> {
    if let Some(path) = find_java_for(data_dir, mc_version) {
        return Ok(path);
    }
    let major = required_forge_java_major(mc_version);
    let Some(app) = app else {
        return Err(LauncherError::ModLoader(missing_processor_java_message(
            major, false,
        )));
    };

    tracing::info!(
        target: "launcher",
        "Java {} required for Forge installers but not found; downloading the managed runtime",
        major
    );
    let runtime = crate::java_download::download_java_runtime(major, data_dir, app)
        .await
        .map_err(|e| {
            LauncherError::ModLoader(format!(
                "Java {major} is required for the Forge installers, but the managed Java download failed: {e}"
            ))
        })?;

    // The downloader probes the published runtime, but run the normal
    // discovery again so the selected executable still follows the single
    // Java-selection policy used by launch and Repair.
    if let Some(path) = find_java_for(data_dir, mc_version) {
        return Ok(path);
    }
    if runtime.major_version == major && runtime.path.is_file() {
        return Ok(runtime.path);
    }
    Err(LauncherError::ModLoader(missing_processor_java_message(
        major, true,
    )))
}

/// Convert a Maven artifact coordinate `group:name:version[:classifier][@ext]`
/// to a local filesystem path under libraries_dir.
pub(crate) fn artifact_to_path(artifact: &str, libraries_dir: &Path, ext: &str) -> PathBuf {
    let parts: Vec<&str> = artifact.split(':').collect();
    if parts.len() < 3 {
        return PathBuf::new();
    }
    let group_path = parts[0].replace('.', "/");
    let artifact_name = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).copied().unwrap_or("");
    // Install-profile coordinates are remote input. A component containing
    // `..`, a separator or a drive prefix must never be joined into the
    // libraries directory; return an empty path so the caller fails clearly.
    let component_ok =
        |value: &str| value.is_empty() || crate::import::check_safe_file_name(value).is_ok();
    if crate::import::check_safe_remote_path(&group_path).is_err()
        || !component_ok(artifact_name)
        || !component_ok(version)
        || !component_ok(classifier)
        || !component_ok(ext)
    {
        tracing::warn!(target: "launcher", "Unsafe artifact coordinate: {}", artifact);
        return PathBuf::new();
    }
    let filename = if classifier.is_empty() {
        format!("{}-{}.{}", artifact_name, version, ext)
    } else {
        format!("{}-{}-{}.{}", artifact_name, version, classifier, ext)
    };
    libraries_dir
        .join(&group_path)
        .join(artifact_name)
        .join(version)
        .join(filename)
}

/// Parse an artifact reference like `[group:name:version:classifier@ext]`
/// and resolve to a local path under libraries_dir.
pub(crate) fn resolve_artifact_ref(ref_str: &str, libraries_dir: &Path) -> PathBuf {
    let inner = ref_str.trim_start_matches('[').trim_end_matches(']');
    let (coord, ext) = if let Some(at_pos) = inner.rfind('@') {
        (&inner[..at_pos], &inner[at_pos + 1..])
    } else {
        (inner, "jar")
    };
    artifact_to_path(coord, libraries_dir, ext)
}

/// Read a text entry from inside a JAR (zip) archive.
pub(crate) fn read_entry_from_jar<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            let Some(entry_name) = entry.name().ok().map(|name| name.replace('\\', "/")) else {
                continue;
            };
            if entry_name == name || entry_name.ends_with(&format!("/{}", name)) {
                let mut content = String::new();
                entry.read_to_string(&mut content).ok()?;
                return Some(content);
            }
        }
    }
    None
}

/// Extract a binary entry from inside a JAR (zip) archive to a destination path.
pub(crate) fn extract_entry_from_jar<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    dest: &Path,
) -> bool {
    for i in 0..archive.len() {
        if let Ok(mut entry) = archive.by_index(i) {
            let Some(entry_name) = entry.name().ok().map(|name| name.replace('\\', "/")) else {
                continue;
            };
            if entry_name == name || entry_name.ends_with(&format!("/{}", name)) {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                if let Ok(mut file) = std::fs::File::create(dest) {
                    use std::io::Write;
                    let mut buf = Vec::new();
                    if entry.read_to_end(&mut buf).is_ok() && file.write_all(&buf).is_ok() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Read the main class from a JAR file's manifest.
pub(crate) fn get_jar_main_class(jar_path: &Path) -> Option<String> {
    let bytes = std::fs::read(jar_path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for line in read_entry_from_jar(&mut archive, "META-INF/MANIFEST.MF")?.lines() {
        if let Some(stripped) = line.strip_prefix("Main-Class: ") {
            return Some(stripped.trim().to_string());
        }
    }
    None
}

/// Download one processor library from the install_profile.json libraries list.
/// Returns Ok if already exists or downloaded successfully.
pub(crate) async fn download_processor_lib(
    lib_val: &serde_json::Value,
    libraries_dir: &Path,
    maven_base: &str,
    allowed_hosts: &[&str],
) -> Result<()> {
    let name = lib_val.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if name.is_empty() {
        return Ok(());
    }

    let artifact = lib_val
        .get("downloads")
        .and_then(|downloads| downloads.get("artifact"));
    let (url, path_str, sha1, sha256, sha512, size) = if let Some(artifact) = artifact {
        let path_str = artifact
            .get("path")
            .and_then(|p| p.as_str())
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| maven_to_path(name));
        let sha1 = artifact
            .get("sha1")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        let sha256 = artifact
            .get("sha256")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        let sha512 = artifact
            .get("sha512")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        let size = artifact.get("size").and_then(|v| v.as_u64());
        match classify_artifact_value_url(artifact.get("url"))? {
            // An explicitly empty URL means the file is embedded in the
            // verified installer and must be extracted there.
            ForgeArtifactUrl::Embedded => return Ok(()),
            ForgeArtifactUrl::Remote(url) => {
                (url.to_string(), path_str, sha1, sha256, sha512, size)
            }
            ForgeArtifactUrl::Missing => {
                // Historical metadata omitted `url`; resolve a known Mojang
                // coordinate to Mojang Libraries, or a loader coordinate to
                // the loader Maven. Unknown sources fail closed.
                let base = super::artifact::declared_maven_base(
                    lib_val.get("url").and_then(|u| u.as_str()),
                    name,
                    maven_base,
                )?;
                (
                    super::artifact::join_maven_url(&base, &path_str),
                    path_str,
                    sha1,
                    sha256,
                    sha512,
                    size,
                )
            }
        }
    } else {
        let path_str = maven_to_path(name);
        let base = super::artifact::declared_maven_base(
            lib_val.get("url").and_then(|u| u.as_str()),
            name,
            maven_base,
        )?;
        (
            super::artifact::join_maven_url(&base, &path_str),
            path_str,
            None,
            None,
            None,
            None,
        )
    };

    if crate::import::check_safe_remote_path(&path_str).is_err() {
        return Err(LauncherError::ModLoader(format!(
            "Unsafe processor library path: {}",
            path_str
        )));
    }

    let source = super::artifact::has_metadata_checksum(
        sha1.as_deref(),
        sha256.as_deref(),
        sha512.as_deref(),
    )
    .then(|| "metadata".to_string());
    let mut lib = LoaderLibrary {
        name: name.to_string(),
        url,
        path: path_str,
        sha1,
        sha256,
        sha512,
        checksum_source: source,
        size,
    };
    super::artifact::ensure_library(&mut lib, libraries_dir, allowed_hosts, "processor")
        .await
        .map(|_| ())
        .map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to verify processor library {}: {}",
                name, e
            ))
        })
}

/// Inputs for the Forge/NeoForge installer processor pipeline.
pub(crate) struct ForgeProcessorInputs<'a> {
    pub install_profile: &'a serde_json::Value,
    pub installer_path: &'a Path,
    pub java_path: &'a Path,
    pub libraries_dir: &'a Path,
    pub mc_version: &'a str,
    pub client_jar: &'a Path,
    pub root_dir: &'a Path,
    pub data_base: &'a str,
    /// Full loader version (e.g. `1.20.1-47.4.20`); scopes the extracted
    /// installer data files under `<data_base>/_data/<data_version>/...` so
    /// different loader versions can never reuse each other's patch files.
    pub data_version: &'a str,
    /// Optional target -> staging map. When present, processors write
    /// generated JARs into the transaction staging directory and the caller
    /// publishes them only after verification.
    pub output_overrides: Option<&'a std::collections::HashMap<PathBuf, PathBuf>>,
}

/// A processor-generated JAR and the checksum the installer metadata declares
/// for it. Not every generated JAR has a publisher checksum; those without one
/// are still required to be structurally valid.
#[derive(Debug, Clone)]
pub(crate) struct ProcessorOutput {
    pub path: PathBuf,
    pub checksum: Option<HashSpec>,
}

/// Build the variable map used by installer processors and output checks.
// The arguments mirror the installer context one-to-one; grouping them into
// another struct would duplicate `ForgeProcessorInputs` without simplifying
// either caller.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_processor_vars(
    install_profile: &serde_json::Value,
    installer_path: &Path,
    libraries_dir: &Path,
    mc_version: &str,
    client_jar: &Path,
    root_dir: &Path,
    data_base: &str,
    data_version: &str,
) -> HashMap<String, String> {
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("SIDE".to_string(), "client".to_string());
    vars.insert(
        "MINECRAFT_JAR".to_string(),
        client_jar.to_string_lossy().to_string(),
    );
    vars.insert("MINECRAFT_VERSION".to_string(), mc_version.to_string());
    vars.insert("ROOT".to_string(), root_dir.to_string_lossy().to_string());
    vars.insert(
        "INSTALLER".to_string(),
        installer_path.to_string_lossy().to_string(),
    );
    vars.insert(
        "LIBRARY_DIR".to_string(),
        libraries_dir.to_string_lossy().to_string(),
    );

    if let Some(data_map) = install_profile.get("data").and_then(|d| d.as_object()) {
        for (key, entry) in data_map {
            let client_val = entry.get("client").and_then(|c| c.as_str()).unwrap_or("");
            if client_val.starts_with('[') {
                // Maven artifact reference - resolve to local filesystem path
                vars.insert(
                    key.clone(),
                    resolve_artifact_ref(client_val, libraries_dir)
                        .to_string_lossy()
                        .to_string(),
                );
            } else if client_val.starts_with('/') {
                // Path inside installer JAR - use extracted location in libraries dir
                let jar_path = client_val.trim_start_matches('/');
                let fname = Path::new(jar_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("data");
                let data_path = data_file_path(libraries_dir, data_base, data_version, key, fname);
                vars.insert(key.clone(), data_path.to_string_lossy().to_string());
            } else {
                // Plain string (e.g. SHA hash in quotes) - use as-is, strip surrounding quotes
                let clean = client_val.trim_matches('\'').trim_matches('"').to_string();
                vars.insert(key.clone(), clean);
            }
        }
    }
    vars
}

fn resolve_processor_value(raw: &str, vars: &HashMap<String, String>) -> Result<String> {
    let mut value = raw.to_string();
    for (key, replacement) in vars {
        value = value.replace(&format!("{{{}}}", key), replacement);
    }
    if value.contains('{') || value.contains('}') {
        return Err(LauncherError::ModLoader(format!(
            "Processor metadata has an unresolved placeholder: {}",
            truncate_for_error(&value, 300)
        )));
    }
    let trimmed = value.trim();
    let without_quotes = trimmed
        .strip_prefix('\'')
        .and_then(|v| v.strip_suffix('\''))
        .or_else(|| trimmed.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
        .unwrap_or(trimmed);
    Ok(without_quotes.to_string())
}

/// Resolve the processor outputs that are expected for this installer.
///
/// The installer's `data` map names the client-side JARs and, for some of
/// them, a paired `*_SHA` value. Those SHA-1 values are publisher metadata,
/// not a hash computed after generation. `MC_UNPACKED` is server-only and is
/// deliberately excluded.
pub(crate) fn processor_output_plan(
    install_profile: &serde_json::Value,
    vars: &HashMap<String, String>,
    libraries_dir: &Path,
) -> Result<Vec<ProcessorOutput>> {
    let mut outputs: Vec<ProcessorOutput> = Vec::new();
    let data = install_profile.get("data").and_then(|d| d.as_object());

    let mut push = |path: PathBuf, checksum: Option<HashSpec>| -> Result<()> {
        if !path.starts_with(libraries_dir) {
            return Err(LauncherError::ModLoader(format!(
                "Processor output escapes the libraries directory: {}",
                path.display()
            )));
        }
        crate::import::ensure_no_reparse_points(libraries_dir, &path)?;
        if let Some(existing) = outputs.iter_mut().find(|output| {
            super::processor_cache::path_identity(&output.path)
                == super::processor_cache::path_identity(&path)
        }) {
            match (&existing.checksum, &checksum) {
                (None, Some(_)) => existing.checksum = checksum,
                (Some(existing_hash), Some(new_hash)) if existing_hash.hex() != new_hash.hex() => {
                    return Err(LauncherError::ModLoader(format!(
                        "Processor output {} is declared with conflicting checksums",
                        path.display()
                    )))
                }
                _ => {}
            }
            return Ok(());
        }
        outputs.push(ProcessorOutput { path, checksum });
        Ok(())
    };

    if let Some(data) = data {
        for (key, entry) in data {
            if key == "MC_UNPACKED" || entry.get("client").and_then(|v| v.as_str()).is_none() {
                continue;
            }
            let Some(path) = vars.get(key) else {
                continue;
            };
            if path.is_empty() {
                let artifact_ref = entry
                    .get("client")
                    .and_then(|v| v.as_str())
                    .is_some_and(|value| value.starts_with('['));
                if artifact_ref {
                    return Err(LauncherError::ModLoader(format!(
                        "Processor output {key} has an unsafe or malformed artifact coordinate"
                    )));
                }
                continue;
            }
            if !path.to_ascii_lowercase().ends_with(".jar") {
                continue;
            }
            let checksum = match vars.get(&format!("{key}_SHA")) {
                Some(hash) if !hash.is_empty() => Some(HashSpec::sha1(hash)?),
                _ => None,
            };
            push(PathBuf::from(path), checksum)?;
        }
    }

    // Newer installer profiles may declare outputs directly. Use them too so
    // a renamed output is not silently ignored.
    if let Some(processors) = install_profile.get("processors").and_then(|p| p.as_array()) {
        for processor in processors {
            let Some(outputs_map) = processor.get("outputs").and_then(|o| o.as_object()) else {
                continue;
            };
            for (raw_path, raw_checksum) in outputs_map {
                let Some(raw_checksum) = raw_checksum.as_str() else {
                    continue;
                };
                let path = PathBuf::from(resolve_processor_value(raw_path, vars)?);
                let checksum = resolve_processor_value(raw_checksum, vars)?;
                push(path, Some(HashSpec::sha1(&checksum)?))?;
            }
        }
    }

    Ok(outputs)
}

pub(crate) fn verify_processor_outputs(outputs: &[ProcessorOutput]) -> Result<()> {
    for output in outputs {
        if !is_usable_jar(&output.path) {
            return Err(LauncherError::ModLoader(format!(
                "Processor output is missing or not a valid JAR: {}",
                output.path.display()
            )));
        }
        if let Some(checksum) = &output.checksum {
            if !checksum.verify_file(&output.path)? {
                return Err(LauncherError::ModLoader(format!(
                    "Processor output failed its installer metadata checksum: {}",
                    output.path.display()
                )));
            }
        }
    }
    Ok(())
}

/// Run the Forge installer processors (Forge 1.13+) by spawning Java processes
/// for each processor step.
///
/// All required libraries must already be downloaded to `libraries_dir` before
/// calling this function. Embedded files (universal jar, client.lzma) must also
/// have been extracted.
///
/// `data_base` is the maven directory of the loader inside `libraries_dir`
/// ("net/minecraftforge/forge" for Forge, "net/neoforged/neoforge" for
/// NeoForge 21.x); installer data files extracted from the JAR live under
/// `<data_base>/_data/<data_version>/...` (version-scoped, see
/// `data_file_path`).
///
/// Returns Ok(()) if all processors completed successfully and output JARs exist.
pub(crate) fn run_forge_processors(inputs: ForgeProcessorInputs<'_>) -> Result<()> {
    let ForgeProcessorInputs {
        install_profile,
        installer_path,
        java_path,
        libraries_dir,
        mc_version,
        client_jar,
        root_dir,
        data_base,
        data_version,
        output_overrides,
    } = inputs;
    let processors = install_profile
        .get("processors")
        .and_then(|p| p.as_array())
        .ok_or_else(|| LauncherError::ModLoader("No processors in install_profile.json".into()))?;

    let mut vars = build_processor_vars(
        install_profile,
        installer_path,
        libraries_dir,
        mc_version,
        client_jar,
        root_dir,
        data_base,
        data_version,
    );
    // Resolve the output list before applying staging overrides, so the
    // persisted manifest always refers to the final target paths.
    let target_outputs = processor_output_plan(install_profile, &vars, libraries_dir)?;
    if target_outputs.is_empty() {
        return Err(LauncherError::ModLoader(
            "Processor metadata declares no verifiable client outputs".into(),
        ));
    }
    let expected_outputs = if let Some(overrides) = output_overrides {
        for value in vars.values_mut() {
            if let Some(staged) = overrides.get(Path::new(value)) {
                *value = staged.to_string_lossy().to_string();
            }
        }
        target_outputs
            .iter()
            .map(|output| {
                let staged = overrides.get(&output.path).ok_or_else(|| {
                    LauncherError::ModLoader(format!(
                        "Processor output has no staging path: {}",
                        output.path.display()
                    ))
                })?;
                Ok(ProcessorOutput {
                    path: staged.clone(),
                    checksum: output.checksum.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        target_outputs.clone()
    };

    tracing::info!(target: "launcher", "Running {} Forge processors", processors.len());

    for (idx, proc_val) in processors.iter().enumerate() {
        // Check sides
        let sides = proc_val
            .get("sides")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        if !sides.is_empty() && !sides.contains(&"client") {
            tracing::debug!(target: "launcher", "Skipping processor {} (not client side)", idx);
            continue;
        }

        let jar_artifact = proc_val.get("jar").and_then(|j| j.as_str()).unwrap_or("");
        if jar_artifact.is_empty() {
            // Skipping a step silently produces an incomplete pipeline whose
            // missing output is only discovered (if at all) much later.
            return Err(LauncherError::ModLoader(format!(
                "Processor {idx} has no 'jar' entry in install_profile.json"
            )));
        }

        // Resolve the main processor jar path
        let jar_path = resolve_artifact_ref(&format!("[{}]", jar_artifact), libraries_dir);
        if !jar_path.is_file() {
            return Err(LauncherError::ModLoader(format!(
                "Processor {idx} JAR is missing: {jar_artifact} (resolved to {})",
                jar_path.display()
            )));
        }

        let main_class = get_jar_main_class(&jar_path).ok_or_else(|| {
            LauncherError::ModLoader(format!(
                "Processor {idx} JAR has no Main-Class manifest entry: {}",
                jar_path.display()
            ))
        })?;

        // Build classpath: processor jar + all classpath dependencies
        let classpath_artifacts = proc_val
            .get("classpath")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        let mut classpath_parts: Vec<String> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let all_refs = std::iter::once(jar_artifact).chain(classpath_artifacts.iter().copied());
        for artifact_ref in all_refs {
            let ref_str = if artifact_ref.starts_with('[') {
                artifact_ref.to_string()
            } else {
                format!("[{}]", artifact_ref)
            };
            let local_path = resolve_artifact_ref(&ref_str, libraries_dir);
            if local_path.is_file() {
                classpath_parts.push(local_path.to_string_lossy().to_string());
            } else {
                missing.push(format!("{} -> {}", artifact_ref, local_path.display()));
            }
        }

        if !missing.is_empty() {
            return Err(LauncherError::ModLoader(format!(
                "Processor {idx} classpath is incomplete ({} missing): {}",
                missing.len(),
                missing.join("; ")
            )));
        }
        if classpath_parts.is_empty() {
            return Err(LauncherError::ModLoader(format!(
                "Processor {idx} has an empty classpath in install_profile.json"
            )));
        }

        let classpath = classpath_parts.join(";");

        // Build args with variable substitution
        let raw_args = proc_val
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        let mut processed_args: Vec<String> = Vec::new();
        for raw_arg in &raw_args {
            let resolved = if raw_arg.starts_with('[') && raw_arg.ends_with(']') {
                let path = resolve_artifact_ref(raw_arg, libraries_dir);
                if path.as_os_str().is_empty() {
                    return Err(LauncherError::ModLoader(format!(
                        "Processor {idx} has an unsafe or malformed artifact argument: {raw_arg}"
                    )));
                }
                path.to_string_lossy().to_string()
            } else {
                let mut arg = raw_arg.to_string();
                for (key, val) in &vars {
                    arg = arg.replace(&format!("{{{}}}", key), val);
                }
                if arg.contains('{') || arg.contains('}') {
                    // An unresolved placeholder would be passed to Java
                    // literally and the processor would write to a path
                    // containing braces instead of the requested output.
                    return Err(LauncherError::ModLoader(format!(
                        "Processor {idx} argument has an unresolved placeholder: {}",
                        truncate_for_error(&arg, 300)
                    )));
                }
                arg
            };
            processed_args.push(resolved);
        }

        tracing::info!(target: "launcher", "Processor {}: {} main={}", idx, jar_path.display(), main_class);
        tracing::debug!(target: "launcher", "  classpath ({}): {}", classpath_parts.len(), classpath);
        tracing::debug!(target: "launcher", "  args: {:?}", processed_args);

        // Spawn Java process for this processor
        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = std::process::Command::new(java_path);
            c.creation_flags(0x08000000);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut cmd = std::process::Command::new(java_path);

        let output = crate::java::run_command_with_timeout(
            cmd.arg("-cp")
                .arg(&classpath)
                .arg(&main_class)
                .args(&processed_args),
            std::time::Duration::from_secs(600),
        )
        .map_err(|e| {
            LauncherError::ModLoader(format!(
                "Failed to run processor {} ({}): {}",
                idx,
                if e.kind() == std::io::ErrorKind::TimedOut {
                    "timed out after 10 minutes"
                } else {
                    "spawn error"
                },
                e
            ))
        })?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            let detail = if stderr.trim().is_empty() {
                truncate_for_error(&stdout, 500)
            } else {
                truncate_for_error(&stderr, 500)
            };
            return Err(LauncherError::ModLoader(format!(
                "Forge processor {} failed (exit: {}): {}",
                idx, output.status, detail
            )));
        }

        if !stderr.trim().is_empty() {
            tracing::debug!(target: "launcher", "  stderr: {}", truncate_for_error(&stderr, 200));
        }
        tracing::info!(target: "launcher", "Processor {} completed", idx);
    }

    // A successful process exit is not enough: the installer metadata names
    // the expected client JAR checksums. Verify them before reporting the
    // generated pipeline as usable.
    if let Err(e) = verify_processor_outputs(&expected_outputs) {
        for output in &expected_outputs {
            let _ = std::fs::remove_file(&output.path);
        }
        return Err(e);
    }
    tracing::info!(
        target: "launcher",
        "Processor outputs verified ({} declared checksums, {} structural-only)",
        expected_outputs
            .iter()
            .filter(|output| output.checksum.is_some())
            .count(),
        expected_outputs
            .iter()
            .filter(|output| output.checksum.is_none())
            .count()
    );

    Ok(())
}

fn build_profile(
    main_class: String,
    libs: Vec<ForgeLibrary>,
    arguments: Option<ForgeArguments>,
    minecraft_arguments: Option<String>,
) -> Result<LoaderProfile> {
    let mut libraries = Vec::new();
    for lib in libs {
        let (url, path, sha1, sha256, sha512, checksum_source, size) =
            if let Some(downloads) = &lib.downloads {
                if let Some(artifact) = &downloads.artifact {
                    let lib_path = artifact
                        .path
                        .clone()
                        .unwrap_or_else(|| maven_to_path(&lib.name));
                    // An explicitly empty URL means the artifact is embedded in the
                    // installer JAR (modern Forge ships its runtime jar that way)
                    // and must be extracted from it, not downloaded from maven.
                    // A missing URL is the historical Maven fallback, not embedded.
                    let lib_url = match artifact.url_kind()? {
                        ForgeArtifactUrl::Embedded => String::new(),
                        ForgeArtifactUrl::Remote(url) => url.to_string(),
                        ForgeArtifactUrl::Missing => {
                            let base = super::artifact::declared_maven_base(
                                lib.url.as_deref(),
                                &lib.name,
                                super::artifact::FORGE_MAVEN,
                            )?;
                            super::artifact::join_maven_url(&base, &lib_path)
                        }
                    };
                    let sha1 = artifact.sha1.clone().filter(|s| !s.is_empty());
                    let sha256 = artifact.sha256.clone().filter(|s| !s.is_empty());
                    let sha512 = artifact.sha512.clone().filter(|s| !s.is_empty());
                    let source = super::artifact::has_metadata_checksum(
                        sha1.as_deref(),
                        sha256.as_deref(),
                        sha512.as_deref(),
                    )
                    .then(|| "metadata".to_string());
                    (
                        lib_url,
                        lib_path,
                        sha1,
                        sha256,
                        sha512,
                        source,
                        artifact.size,
                    )
                } else {
                    let path = maven_to_path(&lib.name);
                    let base = super::artifact::declared_maven_base(
                        lib.url.as_deref(),
                        &lib.name,
                        super::artifact::FORGE_MAVEN,
                    )?;
                    (
                        super::artifact::join_maven_url(&base, &path),
                        path,
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                }
            } else {
                let path = maven_to_path(&lib.name);
                let base = super::artifact::declared_maven_base(
                    lib.url.as_deref(),
                    &lib.name,
                    super::artifact::FORGE_MAVEN,
                )?;
                (
                    super::artifact::join_maven_url(&base, &path),
                    path,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            };

        libraries.push(LoaderLibrary {
            name: lib.name.clone(),
            url,
            path,
            sha1,
            sha256,
            sha512,
            checksum_source,
            size,
        });
    }

    // Loader metadata is remote input too: a path containing `..`, an
    // absolute path or a drive prefix must never reach a filesystem join.
    for lib in &libraries {
        if crate::import::check_safe_remote_path(&lib.path).is_err() {
            return Err(LauncherError::ModLoader(format!(
                "Unsafe library path in Forge profile: {}",
                lib.path
            )));
        }
    }

    // For MC <= 1.12.2 the Forge profile carries its game arguments in the
    // legacy `minecraftArguments` string, which contains the FULL argument
    // list (including tweakClass and the standard Mojang tokens) and must
    // REPLACE the vanilla game arguments. Newer profiles use
    // `arguments.game` which only adds extra args on top of vanilla.
    let (game_args, jvm_args, legacy_args) = if let Some(args) = arguments {
        let game = args
            .game
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>();
        if !game.is_empty() {
            let jvm = args
                .jvm
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            (game, jvm, false)
        } else if let Some(mc_args) = &minecraft_arguments {
            let all = mc_args
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>();
            (all, Vec::new(), true)
        } else {
            (Vec::new(), Vec::new(), false)
        }
    } else if let Some(mc_args) = &minecraft_arguments {
        let all = mc_args
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>();
        (all, Vec::new(), true)
    } else {
        (Vec::new(), Vec::new(), false)
    };

    let mut jvm_args = jvm_args;
    // Legacy Forge (MC <= 1.12.2) needs these flags or FML aborts with
    // signature/launch errors. The official launcher adds them for Forge.
    if legacy_args {
        for flag in [
            "-Dfml.ignoreInvalidMinecraftCertificates=true",
            "-Dfml.ignorePatchDiscrepancies=true",
        ] {
            if !jvm_args.iter().any(|a| a.starts_with(flag)) {
                jvm_args.push(flag.to_string());
            }
        }
    }

    Ok(LoaderProfile {
        main_class,
        libraries,
        jvm_args,
        game_args,
        legacy_args,
    })
}

/// Get Forge launch profile by extracting from the installer JAR.
pub async fn get_profile(mc_version: &str, forge_version: &str) -> Result<LoaderProfile> {
    let full_version = full_forge_version(mc_version, forge_version);
    let data_dir = dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("VoidLauncher");
    let cache = ProcessorCache::new(&data_dir, "forge", mc_version, &full_version)?;
    get_profile_cached(mc_version, forge_version, &cache).await
}

pub(crate) async fn get_profile_cached(
    mc_version: &str,
    forge_version: &str,
    cache: &ProcessorCache,
) -> Result<LoaderProfile> {
    let installer_path = download_installer_cached(mc_version, forge_version, cache).await?;

    let jar_bytes = std::fs::read(&installer_path).map_err(|e| {
        LauncherError::ModLoader(format!("Failed to read Forge installer JAR: {}", e))
    })?;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid Forge installer JAR: {}", e)))?;

    // Try version.json first (processor-based installer, MC 1.13+)
    if let Some(version_str) = read_entry_from_jar(&mut archive, "version.json") {
        #[derive(Deserialize)]
        struct MinecraftVersionJson {
            #[serde(rename = "mainClass", default)]
            main_class: Option<String>,
            #[serde(default)]
            libraries: Vec<ForgeLibrary>,
            #[serde(default)]
            arguments: Option<ForgeArguments>,
            #[serde(rename = "minecraftArguments", default)]
            minecraft_arguments: Option<String>,
        }
        if let Ok(ver) = serde_json::from_str::<MinecraftVersionJson>(&version_str) {
            if let Some(main_class) = ver.main_class {
                if !ver.libraries.is_empty() {
                    return build_profile(
                        main_class,
                        ver.libraries,
                        ver.arguments,
                        ver.minecraft_arguments,
                    );
                }
            }
        }
    }

    // Fall back to install_profile.json (legacy standalone installer)
    let install_profile_str = read_entry_from_jar(&mut archive, "install_profile.json")
        .ok_or_else(|| {
            LauncherError::ModLoader(
                "Forge installer JAR has no version.json or install_profile.json".into(),
            )
        })?;

    let mut profile: ForgeInstallProfile =
        serde_json::from_str(&install_profile_str).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e))
        })?;

    // If processor-based, try version.json referenced by the json field
    if profile.main_class.is_none() || profile.libraries.is_empty() {
        if let Some(json_path) = &profile.json {
            if let Some(version_str) = read_entry_from_jar(&mut archive, json_path) {
                if let Ok(version_profile) =
                    serde_json::from_str::<ForgeInstallProfile>(&version_str)
                {
                    if profile.main_class.is_none() {
                        profile.main_class = version_profile.main_class;
                    }
                    if profile.libraries.is_empty() {
                        profile.libraries = version_profile.libraries;
                    }
                    if profile.arguments.is_none() {
                        profile.arguments = version_profile.arguments;
                    }
                    if profile.minecraft_arguments.is_none() {
                        profile.minecraft_arguments = version_profile.minecraft_arguments;
                    }
                }
            }
        }
    }

    // Legacy installers (MC 1.8.x and older) keep the launch data in a
    // nested `versionInfo` object instead of the top level.
    if profile.main_class.is_none() || profile.libraries.is_empty() {
        if let Ok(root) = serde_json::from_str::<serde_json::Value>(&install_profile_str) {
            if let Some(version_info) = root.get("versionInfo") {
                if let Ok(vi) = serde_json::from_value::<ForgeInstallProfile>(version_info.clone())
                {
                    if profile.main_class.is_none() {
                        profile.main_class = vi.main_class;
                    }
                    if profile.libraries.is_empty() {
                        profile.libraries = vi.libraries;
                    }
                    if profile.arguments.is_none() {
                        profile.arguments = vi.arguments;
                    }
                    if profile.minecraft_arguments.is_none() {
                        profile.minecraft_arguments = vi.minecraft_arguments;
                    }
                }
            }
        }
    }

    let main_class = profile
        .main_class
        .ok_or_else(|| LauncherError::ModLoader("No mainClass found in Forge installer".into()))?;

    build_profile(
        main_class,
        profile.libraries,
        profile.arguments,
        profile.minecraft_arguments,
    )
}

/// Path of an extracted installer data file inside the version-scoped
/// `_data` directory: `<libraries>/<data_base>/_data/<data_version>/<key>/<file>`.
///
/// The version component is essential: installer data files (e.g.
/// `client.lzma` patches) differ between loader versions, and a shared path
/// lets the first-installed version's file leak into every other version's
/// processor run. That was observed as a binarypatcher checksum failure
/// which silently produced an empty patched-client JAR.
pub(crate) fn data_file_path(
    libraries_dir: &Path,
    data_base: &str,
    data_version: &str,
    key: &str,
    file_name: &str,
) -> PathBuf {
    libraries_dir
        .join(data_base)
        .join("_data")
        .join(data_version)
        .join(key.to_lowercase())
        .join(file_name)
}

/// Structural sanity check for a processor-generated JAR: the file must
/// exist, be plausibly sized and open as a ZIP with at least one entry.
///
/// This rejects the "processor failed and left an empty or truncated
/// archive" failure mode (e.g. the 22-byte EOCD-only zip binarypatcher left
/// behind on a patching error), which a bare `exists()` check accepts as
/// installed. It is deliberately NOT a full integrity check — a
/// structurally valid but wrong or partial JAR still passes — so it must
/// never be treated as a checksum comparison.
pub(crate) fn is_usable_jar(path: &Path) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.is_file() || meta.len() < 1024 {
        return false;
    }
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    match zip::ZipArchive::new(file) {
        Ok(archive) => !archive.is_empty(),
        Err(_) => false,
    }
}

/// Truncate a subprocess stream for an error message without splitting a
/// UTF-8 code point. Raw byte slicing panicked when Java printed localized
/// (non-ASCII) output.
pub(crate) fn truncate_for_error(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out.trim().to_string()
}

pub(crate) fn processor_identities_from_install_profile(
    install_profile: &serde_json::Value,
) -> Result<Vec<ProcessorIdentity>> {
    let Some(processors) = install_profile.get("processors").and_then(|p| p.as_array()) else {
        return Ok(Vec::new());
    };
    let mut identities = Vec::with_capacity(processors.len());
    for processor in processors {
        let jar = processor
            .get("jar")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                LauncherError::ModLoader("Processor metadata has no jar coordinate".into())
            })?
            .to_string();
        let classpath = processor
            .get("classpath")
            .and_then(|v| v.as_array())
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_string).ok_or_else(|| {
                            LauncherError::ModLoader(
                                "Processor metadata has a non-string classpath entry".into(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let args = processor
            .get("args")
            .and_then(|v| v.as_array())
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_string).ok_or_else(|| {
                            LauncherError::ModLoader(
                                "Processor metadata has a non-string argument".into(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        identities.push(ProcessorIdentity {
            jar,
            classpath,
            args,
        });
    }
    Ok(identities)
}

pub(crate) fn processor_library_identities(
    install_profile: &serde_json::Value,
    libraries_dir: &Path,
) -> Result<Vec<ProcessorLibraryIdentity>> {
    let Some(libraries) = install_profile.get("libraries").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut identities = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for library in libraries {
        let name = library
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                LauncherError::ModLoader("Processor library metadata has no name".into())
            })?;
        let artifact = library.get("downloads").and_then(|v| v.get("artifact"));
        let (relative, empty_url) = if let Some(artifact) = artifact {
            let relative = artifact
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| maven_to_path(name));
            let empty = matches!(
                classify_artifact_value_url(artifact.get("url"))?,
                ForgeArtifactUrl::Embedded
            );
            (relative, empty)
        } else {
            // `download_processor_lib` falls back to the loader Maven base
            // when the entry has no artifact URL. Bind that file too, if it
            // is present after the pipeline.
            (maven_to_path(name), false)
        };
        if empty_url {
            continue;
        }
        let path = checked_relative(libraries_dir, &relative)?;
        if !path.is_file() {
            return Err(LauncherError::ModLoader(format!(
                "Processor library is missing before provenance capture: {}",
                path.display()
            )));
        }
        if !seen.insert(relative.clone()) {
            continue;
        }
        let digest = file_digest(&path)?;
        identities.push(ProcessorLibraryIdentity {
            name: name.to_string(),
            relative_path: relative,
            sha256: digest.sha256,
            size: digest.size,
        });
    }
    identities.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(identities)
}

pub(crate) fn expected_outputs_from_plan(
    plan: &[ProcessorOutput],
    libraries_dir: &Path,
) -> Result<Vec<ExpectedOutput>> {
    let mut expected = Vec::with_capacity(plan.len());
    let mut seen = std::collections::HashSet::new();
    for output in plan {
        if !seen.insert(super::processor_cache::path_identity(&output.path)) {
            return Err(LauncherError::ModLoader(format!(
                "Duplicate processor output target: {}",
                output.path.display()
            )));
        }
        let relative_path = relative_to(libraries_dir, &output.path)?;
        let publisher_sha1 = match &output.checksum {
            Some(hash) if hash.algorithm() == "sha1" => Some(hash.hex().to_string()),
            Some(hash) => {
                return Err(LauncherError::ModLoader(format!(
                    "Unsupported generated-output checksum algorithm {} for {}",
                    hash.algorithm(),
                    output.path.display()
                )))
            }
            None => None,
        };
        expected.push(ExpectedOutput {
            target: output.path.clone(),
            relative_path,
            publisher_sha1,
            structural_only: output.checksum.is_none(),
        });
    }
    Ok(expected)
}

pub(crate) fn stage_processor_outputs(
    cache: &ProcessorCache,
    plan: &[ProcessorOutput],
) -> Result<(Vec<OutputSpec>, HashMap<PathBuf, PathBuf>)> {
    if plan.is_empty() {
        return Err(LauncherError::ModLoader(
            "Processor pipeline has no declared outputs".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for output in plan {
        if !seen.insert(super::processor_cache::path_identity(&output.path)) {
            return Err(LauncherError::ModLoader(format!(
                "Duplicate processor output target before staging: {}",
                output.path.display()
            )));
        }
    }
    cache.begin_staging()?;
    let mut specs = Vec::with_capacity(plan.len());
    let mut overrides = HashMap::with_capacity(plan.len());
    for (index, output) in plan.iter().enumerate() {
        let file_name = output
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                LauncherError::ModLoader(format!(
                    "Processor output has no file name: {}",
                    output.path.display()
                ))
            })?;
        let staged = cache.staging_dir.join(format!("{index}-{file_name}"));
        if let Some(parent) = staged.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let publisher_sha1 = match &output.checksum {
            Some(hash) if hash.algorithm() == "sha1" => Some(hash.hex().to_string()),
            Some(hash) => {
                return Err(LauncherError::ModLoader(format!(
                    "Unsupported generated-output checksum algorithm {} for {}",
                    hash.algorithm(),
                    output.path.display()
                )))
            }
            None => None,
        };
        overrides.insert(output.path.clone(), staged.clone());
        specs.push(OutputSpec {
            target: output.path.clone(),
            staged,
            publisher_sha1,
            structural_only: output.checksum.is_none(),
        });
    }
    Ok((specs, overrides))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_generation_manifest(
    loader: &str,
    mc_version: &str,
    loader_version: &str,
    installer: InstallerIdentity,
    install_profile_sha256: String,
    client_jar: &Path,
    libraries_dir: &Path,
    plan: &[ProcessorOutput],
    processors: Vec<ProcessorIdentity>,
    processor_libraries: Vec<ProcessorLibraryIdentity>,
) -> Result<GenerationManifest> {
    let client = file_digest(client_jar)?;
    let mut outputs = Vec::with_capacity(plan.len());
    for output in plan {
        let digest = file_digest(&output.path)?;
        let publisher_sha1 = match &output.checksum {
            Some(hash) if hash.algorithm() == "sha1" => Some(hash.hex().to_string()),
            Some(hash) => {
                return Err(LauncherError::ModLoader(format!(
                    "Unsupported generated-output checksum algorithm {} for {}",
                    hash.algorithm(),
                    output.path.display()
                )))
            }
            None => None,
        };
        outputs.push(OutputIdentity {
            relative_path: relative_to(libraries_dir, &output.path)?,
            sha256: digest.sha256,
            size: digest.size,
            publisher_sha1,
            structural_only: output.checksum.is_none(),
        });
    }
    Ok(GenerationManifest {
        schema_version: super::processor_cache::MANIFEST_SCHEMA_VERSION,
        loader: loader.to_string(),
        mc_version: mc_version.to_string(),
        loader_version: loader_version.to_string(),
        installer,
        install_profile_sha256,
        client_jar: client,
        processor_libraries,
        processors,
        outputs,
    })
}

/// Ensure Forge processor-generated JARs exist, running the processor pipeline
/// if needed.
pub async fn ensure_processor_jars(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
    app: Option<&tauri::AppHandle>,
) -> Result<()> {
    // This path runs on every launch and derives file names from both
    // versions before any network validation, so reject unsafe values first.
    super::validate_loader_request(mc_version, loader_version)?;

    let libraries_dir = libraries_dir.to_path_buf();
    let versions_dir = versions_dir.to_path_buf();

    let full_version = full_forge_version(mc_version, loader_version);
    let data_dir = libraries_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let cache = ProcessorCache::new(&data_dir, "forge", mc_version, &full_version)?;
    // Install, repair and launch preparation for different instances share
    // this loader-version cache. Hold the cache-wide guard for the complete
    // prepare -> recover -> embedded extraction -> stage -> publish sequence.
    let cache_lock = cache.acquire_operation_lock().await;
    cache.prepare()?;
    cache.recover_pending_transaction(&libraries_dir)?;

    // The persistent installer cache survives %TEMP% cleanup. A cached file
    // without matching publisher provenance is re-downloaded and re-verified;
    // structural validity alone is never accepted.
    let installer_path =
        download_installer_cached_locked(mc_version, loader_version, &cache).await?;

    // Read install_profile.json and version.json. The latter carries the
    // launch-profile libraries, including old Forge generations whose plain
    // runtime JAR is embedded with an explicitly empty URL.
    let jar_bytes = std::fs::read(&installer_path)
        .map_err(|e| LauncherError::ModLoader(format!("Failed to read installer JAR: {}", e)))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Invalid installer JAR: {}", e)))?;

    let install_profile_str = read_entry_from_jar(&mut archive, "install_profile.json")
        .ok_or_else(|| {
            LauncherError::ModLoader("install_profile.json not found in installer JAR".into())
        })?;
    let version_json = read_entry_from_jar(&mut archive, "version.json");

    let install_profile: serde_json::Value =
        serde_json::from_str(&install_profile_str).map_err(|e| {
            LauncherError::ModLoader(format!("Failed to parse install_profile.json: {}", e))
        })?;

    drop(archive); // Release the read lock on jar_bytes
    let install_profile_sha256 = sha256_bytes(install_profile_str.as_bytes());

    // Embedded artifacts are part of the loader installation even when the
    // installer has no processors (Forge 1.12.2). Do this before the
    // processor shortcut and before any caller validates profile libraries.
    let embedded_recovered = ensure_embedded_forge_libraries(
        &installer_path,
        &install_profile,
        version_json.as_deref(),
        &libraries_dir,
    )?;
    if embedded_recovered > 0 {
        tracing::info!(
            target: "launcher",
            "Verified/recovered {} embedded Forge librar{} from the installer",
            embedded_recovered,
            if embedded_recovered == 1 { "y" } else { "ies" }
        );
    }

    // Check if this is a processor-based installer
    let has_processors = install_profile
        .get("processors")
        .and_then(|p| p.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    if !has_processors {
        tracing::debug!(
            target: "launcher",
            "No processors in install_profile.json; embedded libraries verified"
        );
        // Keep the validated installer cached: a later self-heal must not
        // re-download several megabytes for an unchanged loader version.
        return Ok(());
    }

    // Processor outputs are written into the timestamped version dir
    // (e.g. 1.16.5-20210115.111550), not the plain MC version one.
    let vd = versions_dir.join(mc_version);
    let client_jar = if vd.join("client.jar").exists() {
        vd.join("client.jar")
    } else {
        vd.join(format!("{}.jar", mc_version))
    };
    let root_dir = versions_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| versions_dir.clone());
    if !client_jar.exists() {
        return Err(LauncherError::ModLoader(format!(
            "Client JAR not found at {:?}",
            client_jar
        )));
    }

    let vars = build_processor_vars(
        &install_profile,
        &installer_path,
        &libraries_dir,
        mc_version,
        &client_jar,
        &root_dir,
        "net/minecraftforge/forge",
        &full_version,
    );
    let plan = processor_output_plan(&install_profile, &vars, &libraries_dir)?;
    if plan.is_empty() {
        return Err(LauncherError::ModLoader(
            "Forge installer declares processors but no verifiable outputs".into(),
        ));
    }
    let expected = expected_outputs_from_plan(&plan, &libraries_dir)?;
    let processors = processor_identities_from_install_profile(&install_profile)?;
    let installer_identity = cache.load_installer_identity()?.ok_or_else(|| {
        LauncherError::ModLoader("Forge installer provenance is missing from the cache".into())
    })?;

    if let Some(manifest) = cache.load_generation_manifest()? {
        if let Ok(processor_libraries) =
            processor_library_identities(&install_profile, &libraries_dir)
        {
            if cache
                .validate_generation_manifest(
                    &manifest,
                    "forge",
                    mc_version,
                    &full_version,
                    &installer_identity,
                    &install_profile_sha256,
                    &client_jar,
                    &libraries_dir,
                    &processors,
                    &processor_libraries,
                    &expected,
                )
                .is_ok()
            {
                tracing::debug!(
                    target: "launcher",
                    "Forge processor outputs match persistent provenance; skipping regeneration"
                );
                return Ok(());
            }
        }
    }

    tracing::info!(
        target: "launcher",
        "Forge processor provenance is missing or stale; regenerating outputs..."
    );

    // --- Step 1: Download all processor libraries ---
    tracing::info!(target: "launcher", "Downloading Forge processor libraries...");
    if let Some(libraries) = install_profile.get("libraries").and_then(|l| l.as_array()) {
        for lib_val in libraries {
            download_processor_lib(
                lib_val,
                &libraries_dir,
                super::artifact::FORGE_MAVEN,
                super::artifact::FORGE_HOSTS,
            )
            .await?;
        }
    }

    // --- Step 2: Extract installer data files (embedded libraries were
    // verified and atomically published above, before the processor shortcut).
    tracing::info!(target: "launcher", "Extracting installer data files...");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes))
        .map_err(|e| LauncherError::ModLoader(format!("Failed to reopen installer JAR: {}", e)))?;

    // Extract data files (like client.lzma) from the installer JAR into the
    // version-scoped `_data` directory. Always (over)write: the directory is
    // keyed by the loader version, so a correct file can only come from this
    // loader, and overwriting clears leftovers of an interrupted extract.
    if let Some(data_map) = install_profile.get("data").and_then(|d| d.as_object()) {
        for (key, entry) in data_map {
            let client_val = entry.get("client").and_then(|c| c.as_str()).unwrap_or("");
            if client_val.starts_with('/') {
                let jar_path = client_val.trim_start_matches('/');
                let fname = Path::new(jar_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("data");
                // The data-map key becomes a directory component; a crafted
                // ".." / separator must not escape _data.
                if crate::import::check_safe_file_name(&key.to_lowercase()).is_err() {
                    tracing::warn!(target: "launcher", "Skipping Forge data entry with unsafe key: {}", key);
                    continue;
                }
                let dest = data_file_path(
                    &libraries_dir,
                    "net/minecraftforge/forge",
                    &full_version,
                    key,
                    fname,
                );
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                if extract_entry_from_jar(&mut archive, jar_path, &dest) {
                    tracing::debug!(target: "launcher", "Extracted data {} -> {}", jar_path, dest.display());
                } else {
                    tracing::warn!(target: "launcher", "Failed to extract data file: {}", jar_path);
                }
            }
        }
    }

    drop(archive); // Release the JAR bytes

    // --- Step 3: Run the processor pipeline into a transaction staging area ---
    let (output_specs, output_overrides) = stage_processor_outputs(&cache, &plan)?;
    let java_path = ensure_java_for_processors(&data_dir, mc_version, app).await?;
    tracing::info!(target: "launcher", "Running Forge processors with Java: {:?}", java_path);
    // Long-lived subprocess pipeline (30-120s for some versions) — run on
    // the blocking pool so it can't stall the async runtime that drives
    // the UI and other downloads.
    let profile = install_profile.clone();
    let installer = installer_path.clone();
    let java = java_path.clone();
    let libs = libraries_dir.clone();
    let ver = mc_version.to_string();
    let cjar = client_jar.clone();
    let root = root_dir.clone();
    let data_ver = full_version.clone();
    let (pipeline, cache_lock) = cache
        .run_with_owned_operation_lock(cache_lock, move || {
            run_forge_processors(ForgeProcessorInputs {
                install_profile: &profile,
                installer_path: &installer,
                java_path: &java,
                libraries_dir: &libs,
                mc_version: &ver,
                client_jar: &cjar,
                root_dir: &root,
                data_base: "net/minecraftforge/forge",
                data_version: &data_ver,
                output_overrides: Some(&output_overrides),
            })
        })
        .await?;
    let _cache_lock = cache_lock;
    if let Err(e) = pipeline {
        // The staging directory is disposable; old target files were not
        // touched because publication happens only after verification.
        tracing::warn!(target: "launcher", "Forge processor run failed: {}", e);
        return Err(LauncherError::ModLoader(format!(
            "Forge processor pipeline failed: {}",
            e
        )));
    }

    // --- Step 4: Publish and record provenance ---
    cache.publish_outputs(&libraries_dir, &output_specs)?;
    verify_processor_outputs(&plan)?;
    let processor_libraries = processor_library_identities(&install_profile, &libraries_dir)?;
    let manifest = build_generation_manifest(
        "forge",
        mc_version,
        &full_version,
        installer_identity,
        install_profile_sha256,
        &client_jar,
        &libraries_dir,
        &plan,
        processors,
        processor_libraries,
    )?;
    cache.write_generation_manifest(&manifest)?;
    tracing::info!(
        target: "launcher",
        "Forge processor outputs verified and published ({} outputs)",
        plan.len()
    );
    Ok(())
}

/// Install Forge — download libraries and run installer to generate processor files.
/// Read-only generation inspection for a Forge instance.
pub(crate) fn inspect_generation(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
) -> GenerationInspection {
    let full_version = full_forge_version(mc_version, loader_version);
    inspect_processor_generation(
        "forge",
        mc_version,
        &full_version,
        &full_version,
        "net/minecraftforge/forge",
        libraries_dir,
        versions_dir,
        true,
    )
}

/// Read-only generation inspection shared by Forge and NeoForge Verify.
/// `cache_loader_version` is the loader-cache version key (Forge includes the
/// Minecraft prefix); `data_version` is the installer data-map version.
#[allow(clippy::too_many_arguments)]
pub(crate) fn inspect_processor_generation(
    loader: &str,
    mc_version: &str,
    cache_loader_version: &str,
    data_version: &str,
    data_base: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
    include_embedded: bool,
) -> GenerationInspection {
    let mut inspection = GenerationInspection::new(GenerationState::Healthy);
    let data_dir = libraries_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let cache = match ProcessorCache::new(&data_dir, loader, mc_version, cache_loader_version) {
        Ok(cache) => cache,
        Err(error) => {
            inspection.push(
                "loader-generation",
                "invalid_generation_identity",
                Some(error.to_string()),
                false,
            );
            inspection.state = GenerationState::Incomplete;
            return inspection;
        }
    };
    if !cache.root.is_dir() {
        inspection.push(
            format!("loader-cache/{loader}/{mc_version}/{cache_loader_version}"),
            "missing_generation_metadata",
            Some(
                "the persistent loader cache is missing; online preparation or migration is required"
                    .to_string(),
            ),
            true,
        );
        inspection.state = GenerationState::NeedsMigration;
        return inspection;
    }
    let _lock = match cache.try_acquire_operation_lock() {
        Some(lock) => lock,
        None => {
            inspection.push(
                "loader-generation",
                "verification_incomplete",
                Some(
                    "another install, repair or launch preparation is publishing this loader version"
                        .to_string(),
                ),
                false,
            );
            inspection.state = GenerationState::Busy;
            return inspection;
        }
    };
    if let Err(error) = crate::import::ensure_no_reparse_points(&data_dir, &cache.root) {
        inspection.push(
            "loader-generation",
            "unsafe_generation_cache",
            Some(error.to_string()),
            false,
        );
        inspection.state = GenerationState::Incomplete;
        return inspection;
    }
    if cache.tx_dir.exists() {
        if let Err(error) = crate::import::ensure_no_reparse_points(&cache.root, &cache.tx_dir) {
            inspection.push(
                "loader-generation",
                "unsafe_generation_cache",
                Some(error.to_string()),
                false,
            );
            inspection.state = GenerationState::Incomplete;
            return inspection;
        }
    }

    match cache.inspect_transaction(libraries_dir) {
        Ok(transaction) => match transaction.state {
            TransactionState::Clean => {}
            TransactionState::Prepared | TransactionState::CommittedValid => {
                inspection.push(
                    "loader-generation",
                    if transaction.state == TransactionState::Prepared {
                        "pending_prepared_transaction"
                    } else {
                        "pending_transaction_cleanup"
                    },
                    transaction.detail,
                    true,
                );
                inspection.state = GenerationState::NeedsRepair;
                return inspection;
            }
            TransactionState::CommittedDamaged => {
                inspection.push(
                    "loader-generation",
                    "recovery_required",
                    transaction.detail,
                    false,
                );
                inspection.state = GenerationState::RecoveryRequired;
                return inspection;
            }
        },
        Err(error) => {
            inspection.push(
                "loader-generation",
                "corrupt_processor_journal",
                Some(error.to_string()),
                false,
            );
            inspection.state = GenerationState::RecoveryRequired;
            return inspection;
        }
    }

    let installer_identity = match cache.load_installer_identity() {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            inspection.push(
                "loader-generation",
                "missing_installer_metadata",
                Some(
                    "the verified installer identity is missing; re-verification is required"
                        .to_string(),
                ),
                true,
            );
            inspection.state = GenerationState::NeedsMigration;
            return inspection;
        }
        Err(error) => {
            inspection.push(
                "loader-generation",
                "unreadable_installer_metadata",
                Some(error.to_string()),
                false,
            );
            inspection.state = GenerationState::Incomplete;
            return inspection;
        }
    };
    match cache.installer_identity_matches(&installer_identity) {
        Ok(true) => {}
        Ok(false) => {
            inspection.push(
                "loader-generation",
                "stale_installer_metadata",
                Some(
                    "the cached installer no longer matches its recorded identity; re-verification is required"
                        .to_string(),
                ),
                true,
            );
            inspection.state = GenerationState::NeedsMigration;
            return inspection;
        }
        Err(error) => {
            inspection.push(
                "loader-generation",
                "unreadable_installer",
                Some(error.to_string()),
                false,
            );
            inspection.state = GenerationState::Incomplete;
            return inspection;
        }
    }
    if !installer_jar_is_valid(&cache.installer) {
        inspection.push(
            "loader-generation",
            "corrupt_installer",
            Some("the cached installer is not a usable archive".to_string()),
            true,
        );
        inspection.state = GenerationState::NeedsRepair;
        return inspection;
    }

    let jar_bytes = match std::fs::read(&cache.installer) {
        Ok(bytes) => bytes,
        Err(error) => {
            inspection.push(
                "loader-generation",
                "unreadable_installer",
                Some(error.to_string()),
                false,
            );
            inspection.state = GenerationState::Incomplete;
            return inspection;
        }
    };
    let mut archive = match zip::ZipArchive::new(std::io::Cursor::new(&*jar_bytes)) {
        Ok(archive) => archive,
        Err(error) => {
            inspection.push(
                "loader-generation",
                "corrupt_installer",
                Some(error.to_string()),
                true,
            );
            inspection.state = GenerationState::NeedsRepair;
            return inspection;
        }
    };
    let install_profile_str = match read_entry_from_jar(&mut archive, "install_profile.json") {
        Some(value) => value,
        None => {
            inspection.push(
                "loader-generation",
                "missing_install_profile",
                Some("the verified installer has no install_profile.json".to_string()),
                true,
            );
            inspection.state = GenerationState::NeedsRepair;
            return inspection;
        }
    };
    let version_json = if include_embedded {
        read_entry_from_jar(&mut archive, "version.json")
    } else {
        None
    };
    let install_profile: serde_json::Value = match serde_json::from_str(&install_profile_str) {
        Ok(value) => value,
        Err(error) => {
            inspection.push(
                "loader-generation",
                "invalid_install_profile",
                Some(error.to_string()),
                false,
            );
            inspection.state = GenerationState::Incomplete;
            return inspection;
        }
    };
    drop(archive);
    let install_profile_sha256 = sha256_bytes(install_profile_str.as_bytes());

    if include_embedded {
        for issue in inspect_embedded_forge_libraries(
            &cache.installer,
            &install_profile,
            version_json.as_deref(),
            libraries_dir,
        ) {
            inspection.push(issue.path, &issue.kind, issue.detail, issue.repairable);
        }
    }

    let has_processors = install_profile
        .get("processors")
        .and_then(|processors| processors.as_array())
        .map(|processors| !processors.is_empty())
        .unwrap_or(false);
    if !has_processors {
        if inspection.issues.is_empty() {
            inspection.state = GenerationState::Healthy;
        } else {
            inspection.state = GenerationState::NeedsRepair;
        }
        return inspection;
    }

    let vd = versions_dir.join(mc_version);
    let client_jar = if vd.join("client.jar").exists() {
        vd.join("client.jar")
    } else {
        vd.join(format!("{}.jar", mc_version))
    };
    if !client_jar.is_file() {
        inspection.push(
            format!("versions/{mc_version}/client.jar"),
            "missing_client_jar",
            Some("the vanilla client JAR is missing".to_string()),
            true,
        );
        return inspection;
    }
    let root_dir = versions_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| versions_dir.to_path_buf());
    let vars = build_processor_vars(
        &install_profile,
        &cache.installer,
        libraries_dir,
        mc_version,
        &client_jar,
        &root_dir,
        data_base,
        data_version,
    );
    let plan = match processor_output_plan(&install_profile, &vars, libraries_dir) {
        Ok(plan) if !plan.is_empty() => plan,
        Ok(_) => {
            inspection.push(
                "loader-generation",
                "missing_generated_outputs",
                Some("the installer declares no verifiable client outputs".to_string()),
                false,
            );
            return inspection;
        }
        Err(error) => {
            inspection.push(
                "loader-generation",
                "invalid_processor_plan",
                Some(error.to_string()),
                false,
            );
            return inspection;
        }
    };
    let expected = match expected_outputs_from_plan(&plan, libraries_dir) {
        Ok(expected) => expected,
        Err(error) => {
            inspection.push(
                "loader-generation",
                "invalid_processor_plan",
                Some(error.to_string()),
                false,
            );
            return inspection;
        }
    };
    let processors = match processor_identities_from_install_profile(&install_profile) {
        Ok(processors) => processors,
        Err(error) => {
            inspection.push(
                "loader-generation",
                "invalid_processor_metadata",
                Some(error.to_string()),
                false,
            );
            return inspection;
        }
    };
    let processor_libraries = match processor_library_identities(&install_profile, libraries_dir) {
        Ok(libraries) => libraries,
        Err(error) => {
            inspection.push(
                "loader-generation",
                "missing_processor_library",
                Some(error.to_string()),
                true,
            );
            return inspection;
        }
    };

    for output in &expected {
        if !output.target.is_file() {
            inspection.push(
                output.relative_path.clone(),
                "missing_generated_output",
                Some("processor-generated output is missing".to_string()),
                true,
            );
            continue;
        }
        if !is_usable_jar(&output.target) {
            inspection.push(
                output.relative_path.clone(),
                "corrupt_generated_output",
                Some("processor-generated output is not a usable JAR".to_string()),
                true,
            );
            continue;
        }
        if let Some(publisher_sha1) = output.publisher_sha1.as_deref() {
            match HashSpec::sha1(publisher_sha1).and_then(|hash| hash.verify_file(&output.target)) {
                Ok(true) => {}
                Ok(false) => inspection.push(
                    output.relative_path.clone(),
                    "corrupt_generated_output",
                    Some("processor-generated output failed its publisher checksum".to_string()),
                    true,
                ),
                Err(error) => inspection.push(
                    output.relative_path.clone(),
                    "unreadable_generated_output",
                    Some(error.to_string()),
                    true,
                ),
            }
        }
    }

    let manifest = match cache.load_generation_manifest() {
        Ok(Some(manifest)) => Some(manifest),
        Ok(None) => {
            inspection.push(
                "loader-generation",
                "missing_generation_manifest",
                Some(
                    "the generation manifest is missing; safe regeneration is required".to_string(),
                ),
                true,
            );
            None
        }
        Err(error) => {
            inspection.push(
                "loader-generation",
                "invalid_generation_manifest",
                Some(error.to_string()),
                true,
            );
            None
        }
    };

    if let Some(manifest) = &manifest {
        // Check the per-output local hashes explicitly so a structurally valid
        // but wrong NeoForge client JAR is reported as corrupt, not merely as
        // a generic stale manifest.
        for output in &expected {
            let Some(record) = manifest.outputs.iter().find(|record| {
                relative_identity(&record.relative_path) == relative_identity(&output.relative_path)
            }) else {
                inspection.push(
                    output.relative_path.clone(),
                    "missing_generation_record",
                    Some("the generation manifest has no record for this output".to_string()),
                    true,
                );
                continue;
            };
            if output.target.is_file() {
                match file_digest(&output.target) {
                    Ok(digest) if digest.sha256 == record.sha256 && digest.size == record.size => {}
                    Ok(_) => inspection.push(
                        output.relative_path.clone(),
                        "corrupt_generated_output",
                        Some(
                            "processor-generated output does not match the generation manifest"
                                .to_string(),
                        ),
                        true,
                    ),
                    Err(error) => inspection.push(
                        output.relative_path.clone(),
                        "unreadable_generated_output",
                        Some(error.to_string()),
                        true,
                    ),
                }
            }
        }
        if let Err(error) = cache.validate_generation_manifest(
            manifest,
            loader,
            mc_version,
            cache_loader_version,
            &installer_identity,
            &install_profile_sha256,
            &client_jar,
            libraries_dir,
            &processors,
            &processor_libraries,
            &expected,
        ) {
            inspection.push(
                "loader-generation",
                "stale_generation_manifest",
                Some(error.to_string()),
                true,
            );
        }
    }

    if inspection.issues.is_empty() {
        inspection.state = GenerationState::Healthy;
    } else if inspection.state == GenerationState::Healthy {
        inspection.state = GenerationState::NeedsRepair;
    }
    inspection
}

pub async fn install(
    mc_version: &str,
    loader_version: &str,
    libraries_dir: &Path,
    versions_dir: &Path,
    app: Option<&tauri::AppHandle>,
) -> Result<LoaderProfile> {
    tracing::info!(target: "launcher", "Installing Forge for MC {} (loader {})", mc_version, loader_version);

    let full_version = full_forge_version(mc_version, loader_version);
    let data_dir = libraries_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let cache = ProcessorCache::new(&data_dir, "forge", mc_version, &full_version)?;
    let mut profile = get_profile_cached(mc_version, loader_version, &cache).await?;

    // Embedded Forge libraries are part of the verified installer. Ensure
    // them under the cache operation lock before profile-library validation so
    // `ensure_library` never has to guess what an empty URL means.
    ensure_processor_jars(mc_version, loader_version, libraries_dir, versions_dir, app).await?;

    super::artifact::ensure_libraries(
        &mut profile,
        libraries_dir,
        super::artifact::FORGE_HOSTS,
        "Forge",
    )
    .await?;

    tracing::info!(target: "launcher", "Forge install completed for MC {}", mc_version);
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_test_jar(path: &Path, seed: u8) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
        zip.write_all(b"Manifest-Version: 1.0\n").unwrap();
        zip.start_file("payload.bin", options).unwrap();
        zip.write_all(&vec![seed; 4096]).unwrap();
        zip.finish().unwrap();
    }

    fn test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_forge_processor_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn artifact(url: Option<&str>, sha1: Option<&str>) -> ForgeArtifact {
        ForgeArtifact {
            path: Some("net/minecraftforge/example/1.0/example-1.0.jar".to_string()),
            url: url.map(str::to_string),
            sha1: sha1.map(str::to_string),
            sha256: None,
            sha512: None,
            size: None,
        }
    }

    fn library(url: Option<&str>, sha1: Option<&str>) -> ForgeLibrary {
        ForgeLibrary {
            name: "net.minecraftforge:example:1.0".to_string(),
            url: None,
            downloads: Some(ForgeDownloads {
                artifact: Some(artifact(url, sha1)),
            }),
        }
    }

    #[test]
    fn artifact_url_classification_distinguishes_empty_missing_and_remote() {
        assert_eq!(
            classify_artifact_url(Some("")).unwrap(),
            ForgeArtifactUrl::Embedded
        );
        assert_eq!(
            classify_artifact_url(None).unwrap(),
            ForgeArtifactUrl::Missing
        );
        assert_eq!(
            classify_artifact_url(Some("https://maven.minecraftforge.net/a.jar")).unwrap(),
            ForgeArtifactUrl::Remote("https://maven.minecraftforge.net/a.jar")
        );
        assert!(classify_artifact_url(Some("   ")).is_err());
    }

    #[test]
    fn embedded_detection_uses_only_explicit_empty_urls() {
        let explicit = embedded_specs_from_libraries(&[library(Some(""), None)]).unwrap();
        assert_eq!(explicit.len(), 1);
        let missing = embedded_specs_from_libraries(&[library(None, None)]).unwrap();
        assert!(missing.is_empty());
        let remote = embedded_specs_from_libraries(&[library(
            Some("https://maven.minecraftforge.net/example.jar"),
            None,
        )])
        .unwrap();
        assert!(remote.is_empty());
    }

    #[test]
    fn embedded_checksum_classification_supports_all_metadata_algorithms() {
        let spec =
            |sha1: Option<&str>, sha256: Option<&str>, sha512: Option<&str>| EmbeddedLibrarySpec {
                name: "net.minecraftforge:example:1.0".to_string(),
                relative_path: "net/minecraftforge/example/1.0/example-1.0.jar".to_string(),
                sha1: sha1.map(str::to_string),
                sha256: sha256.map(str::to_string),
                sha512: sha512.map(str::to_string),
                size: None,
            };
        assert_eq!(
            spec(Some(&"aa".repeat(20)), None, None)
                .checksum()
                .unwrap()
                .unwrap()
                .algorithm(),
            "sha1"
        );
        assert_eq!(
            spec(None, Some(&"bb".repeat(32)), None)
                .checksum()
                .unwrap()
                .unwrap()
                .algorithm(),
            "sha256"
        );
        assert_eq!(
            spec(None, None, Some(&"cc".repeat(64)))
                .checksum()
                .unwrap()
                .unwrap()
                .algorithm(),
            "sha512"
        );
        assert!(spec(Some(""), None, None).checksum().unwrap().is_none());
        assert!(spec(None, None, None).checksum().unwrap().is_none());
        assert!(spec(Some("bad"), None, None).checksum().is_err());
    }

    #[test]
    fn malformed_embedded_checksum_is_an_error_not_absent_checksum() {
        let error = embedded_specs_from_libraries(&[library(Some(""), Some("not-a-checksum"))])
            .expect_err("malformed checksum must fail classification");
        assert!(
            error.to_string().to_lowercase().contains("sha1"),
            "got: {error}"
        );
    }

    #[test]
    fn build_profile_keeps_embedded_empty_url_and_falls_back_for_missing_url() {
        let embedded =
            build_profile("Main".into(), vec![library(Some(""), None)], None, None).unwrap();
        assert_eq!(embedded.libraries[0].url, "");

        let missing = build_profile("Main".into(), vec![library(None, None)], None, None).unwrap();
        assert_eq!(
            missing.libraries[0].url,
            "https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar"
        );

        let remote = build_profile(
            "Main".into(),
            vec![library(
                Some("https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar"),
                None,
            )],
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            remote.libraries[0].url,
            "https://maven.minecraftforge.net/net/minecraftforge/example/1.0/example-1.0.jar"
        );
    }

    #[test]
    fn real_forge_1_8_9_version_info_resolves_mojang_libraries_to_mojang_origin() {
        let libs: Vec<ForgeLibrary> =
            serde_json::from_str(include_str!("testdata/forge-1.8.9-version-info.json")).unwrap();
        let profile = build_profile("Main".into(), libs, None, None).unwrap();
        let by_name = |name: &str| {
            profile
                .libraries
                .iter()
                .find(|library| library.name == name)
                .unwrap_or_else(|| panic!("missing library {name}"))
                .url
                .as_str()
        };

        assert_eq!(
            by_name("net.minecraft:launchwrapper:1.12"),
            "https://libraries.minecraft.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar"
        );
        assert_eq!(
            by_name("org.ow2.asm:asm-all:5.0.3"),
            "https://libraries.minecraft.net/org/ow2/asm/asm-all/5.0.3/asm-all-5.0.3.jar"
        );
        assert_eq!(
            by_name("lzma:lzma:0.0.1"),
            "https://libraries.minecraft.net/lzma/lzma/0.0.1/lzma-0.0.1.jar"
        );
        assert_eq!(
            by_name("net.minecraftforge:forge:1.8.9-11.15.1.2318-1.8.9"),
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.8.9-11.15.1.2318-1.8.9/forge-1.8.9-11.15.1.2318-1.8.9.jar"
        );
    }

    #[test]
    fn unknown_historical_library_origin_fails_closed() {
        let unknown = ForgeLibrary {
            name: "com.example:not-in-any-trusted-metadata:1.0".to_string(),
            url: None,
            downloads: None,
        };
        let error = build_profile("Main".into(), vec![unknown], None, None).unwrap_err();
        assert!(
            error.to_string().contains("No trusted Maven origin"),
            "got: {error}"
        );
    }

    #[tokio::test]
    async fn missing_processor_artifact_url_uses_the_verified_maven_fallback() {
        use crate::download::tests::{direct_proxy_lock, sha512_hex, spawn_mock, MockSpec};
        use std::sync::atomic::Ordering;

        let _proxy = direct_proxy_lock().await;
        let body = b"processor-from-maven";
        let hash = sha512_hex(body);
        let server = spawn_mock(vec![MockSpec::ok(hash.as_bytes()), MockSpec::ok(body)]).await;
        let base = server.url.trim_end_matches("artifact.bin");
        let dir = test_dir("missing_processor_url");
        let libs = dir.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();
        let profile_lib = serde_json::json!({
            "name": "net.minecraftforge:example:1.0",
            "downloads": {
                "artifact": {
                    "path": "net/minecraftforge/example/1.0/example-1.0.jar"
                }
            }
        });

        download_processor_lib(
            &profile_lib,
            &libs,
            base,
            super::super::artifact::FORGE_HOSTS,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(libs.join("net/minecraftforge/example/1.0/example-1.0.jar")).unwrap(),
            body
        );
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_processor_url_with_unsafe_path_and_malformed_url_fail() {
        let dir = test_dir("processor_bad_urls");
        let libs = dir.join("libraries");
        let unsafe_lib = serde_json::json!({
            "name": "net.minecraftforge:example:1.0",
            "downloads": {
                "artifact": { "path": "../escape.jar" }
            }
        });
        download_processor_lib(
            &unsafe_lib,
            &libs,
            super::super::artifact::FORGE_MAVEN,
            super::super::artifact::FORGE_HOSTS,
        )
        .await
        .expect_err("unsafe metadata path must fail before network");

        let malformed_lib = serde_json::json!({
            "name": "net.minecraftforge:example:1.0",
            "downloads": {
                "artifact": {
                    "path": "net/minecraftforge/example/1.0/example-1.0.jar",
                    "url": "not a url"
                }
            }
        });
        download_processor_lib(
            &malformed_lib,
            &libs,
            super::super::artifact::FORGE_MAVEN,
            super::super::artifact::FORGE_HOSTS,
        )
        .await
        .expect_err("malformed URL must fail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn explicit_empty_processor_artifact_url_does_not_use_network() {
        use crate::download::tests::{direct_proxy_lock, spawn_mock, MockSpec};
        use std::sync::atomic::Ordering;

        let _proxy = direct_proxy_lock().await;
        let server = spawn_mock(vec![MockSpec::status(500)]).await;
        let dir = test_dir("empty_processor_url");
        let profile_lib = serde_json::json!({
            "name": "net.minecraftforge:example:1.0",
            "downloads": {
                "artifact": {
                    "path": "net/minecraftforge/example/1.0/example-1.0.jar",
                    "url": ""
                }
            }
        });

        download_processor_lib(
            &profile_lib,
            &dir.join("libraries"),
            &server.url,
            super::super::artifact::FORGE_HOSTS,
        )
        .await
        .unwrap();
        assert_eq!(server.hits.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_reports_malformed_embedded_checksum_instead_of_healthy() {
        let dir = test_dir("embedded_bad_checksum_verify");
        let profile = serde_json::json!({
            "libraries": [{
                "name": "net.minecraftforge:example:1.0",
                "downloads": {
                    "artifact": {
                        "path": "net/minecraftforge/example/1.0/example-1.0.jar",
                        "url": "",
                        "sha1": "not-a-checksum"
                    }
                }
            }]
        });
        let issues = inspect_embedded_forge_libraries(
            &dir.join("missing-installer.jar"),
            &profile,
            None,
            &dir.join("libraries"),
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, "invalid_embedded_metadata");
        assert!(!issues[0].repairable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_library_identities_include_missing_url_but_skip_embedded() {
        let dir = test_dir("processor_identity_urls");
        let libraries = dir.join("libraries");
        let missing_path = "net/minecraftforge/example/1.0/example-1.0.jar";
        let embedded_path = "net/minecraftforge/example/1.0/example-1.0-embedded.jar";
        for path in [missing_path, embedded_path] {
            let target = libraries.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, b"jar").unwrap();
        }
        let profile = serde_json::json!({
            "libraries": [
                {
                    "name": "net.minecraftforge:example:1.0",
                    "downloads": { "artifact": { "path": missing_path } }
                },
                {
                    "name": "net.minecraftforge:example:1.0:embedded",
                    "downloads": { "artifact": { "path": embedded_path, "url": "" } }
                }
            ]
        });
        let identities = processor_library_identities(&profile, &libraries).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].relative_path, missing_path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_output_plan_uses_installer_metadata_and_rejects_corruption() {
        let dir = test_dir("plan");
        let libs = dir.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();

        let slim = resolve_artifact_ref("[net.minecraft:client:1.20.1-TEST:slim]", &libs);
        let extra = resolve_artifact_ref("[net.minecraft:client:1.20.1-TEST:extra]", &libs);
        let srg = resolve_artifact_ref("[net.minecraft:client:1.20.1-TEST:srg]", &libs);
        let patched =
            resolve_artifact_ref("[net.minecraftforge:forge:1.20.1-47.4.29:client]", &libs);
        let unpacked = resolve_artifact_ref("[net.minecraft:client:1.20.1-TEST:unpacked]", &libs);
        write_test_jar(&slim, 1);
        write_test_jar(&extra, 2);
        write_test_jar(&srg, 3);
        write_test_jar(&patched, 4);

        let slim_sha = crate::download::hash_file_sha1(&slim).unwrap();
        let extra_sha = crate::download::hash_file_sha1(&extra).unwrap();
        let patched_sha = crate::download::hash_file_sha1(&patched).unwrap();
        let profile = serde_json::json!({
            "data": {
                "MC_SLIM": { "client": "[net.minecraft:client:1.20.1-TEST:slim]" },
                "MC_SLIM_SHA": { "client": format!("'{}'", slim_sha) },
                "MC_EXTRA": { "client": "[net.minecraft:client:1.20.1-TEST:extra]" },
                "MC_EXTRA_SHA": { "client": format!("'{}'", extra_sha) },
                "MC_SRG": { "client": "[net.minecraft:client:1.20.1-TEST:srg]" },
                "MC_UNPACKED": { "client": "[net.minecraft:client:1.20.1-TEST:unpacked]" },
                "PATCHED": { "client": "[net.minecraftforge:forge:1.20.1-47.4.29:client]" },
                "PATCHED_SHA": { "client": format!("'{}'", patched_sha) }
            }
        });
        let installer = dir.join("installer.jar");
        let client = dir.join("client.jar");
        let root = dir.join("root");
        let vars = build_processor_vars(
            &profile,
            &installer,
            &libs,
            "1.20.1",
            &client,
            &root,
            "net/minecraftforge/forge",
            "1.20.1-47.4.29",
        );
        let plan = processor_output_plan(&profile, &vars, &libs).unwrap();
        assert_eq!(plan.len(), 4, "only client-side JARs are expected");
        assert!(!plan.iter().any(|output| output.path == unpacked));
        assert!(plan
            .iter()
            .find(|output| output.path == slim)
            .unwrap()
            .checksum
            .is_some());
        assert!(plan
            .iter()
            .find(|output| output.path == srg)
            .unwrap()
            .checksum
            .is_none());
        verify_processor_outputs(&plan).unwrap();

        // A structurally valid but wrong JAR must fail the metadata hash.
        write_test_jar(&patched, 9);
        assert!(verify_processor_outputs(&plan).is_err());

        // A missing declared output is not accepted just because the others
        // exist.
        std::fs::remove_file(&srg).unwrap();
        assert!(verify_processor_outputs(&plan).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_output_plan_rejects_bad_hashes_and_unsafe_paths() {
        let dir = test_dir("bad");
        let libs = dir.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();
        let installer = dir.join("installer.jar");
        let client = dir.join("client.jar");
        let root = dir.join("root");

        let malformed = serde_json::json!({
            "data": {
                "MC_SLIM": { "client": "[net.minecraft:client:1.20.1-TEST:slim]" },
                "MC_SLIM_SHA": { "client": "'not-a-hash'" }
            }
        });
        let vars = build_processor_vars(
            &malformed,
            &installer,
            &libs,
            "1.20.1",
            &client,
            &root,
            "net/minecraftforge/forge",
            "1.20.1-47.4.29",
        );
        assert!(processor_output_plan(&malformed, &vars, &libs).is_err());

        let traversal = serde_json::json!({
            "data": {
                "MC_SLIM": { "client": "[net.minecraft:client:../../escape:slim]" }
            }
        });
        let vars = build_processor_vars(
            &traversal,
            &installer,
            &libs,
            "1.20.1",
            &client,
            &root,
            "net/minecraftforge/forge",
            "1.20.1-47.4.29",
        );
        assert!(processor_output_plan(&traversal, &vars, &libs).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_output_paths_are_version_scoped() {
        let dir = test_dir("versions");
        let libs = dir.join("libraries");
        std::fs::create_dir_all(&libs).unwrap();
        let installer = dir.join("installer.jar");
        let client = dir.join("client.jar");
        let root = dir.join("root");

        let plan_for = |loader_version: &str, mappings_version: &str| {
            let profile = serde_json::json!({
                "data": {
                    "MC_SLIM": {
                        "client": format!("[net.minecraft:client:{mappings_version}:slim]")
                    },
                    "PATCHED": {
                        "client": format!(
                            "[net.minecraftforge:forge:{loader_version}:client]"
                        )
                    }
                }
            });
            let vars = build_processor_vars(
                &profile,
                &installer,
                &libs,
                "1.20.1",
                &client,
                &root,
                "net/minecraftforge/forge",
                loader_version,
            );
            processor_output_plan(&profile, &vars, &libs).unwrap()
        };

        let first = plan_for("1.20.1-47.4.29", "1.20.1-20230612.114412");
        let second = plan_for("1.20.1-47.4.30", "1.20.1-20230701.000000");
        assert_ne!(
            first.iter().map(|o| &o.path).collect::<Vec<_>>(),
            second.iter().map(|o| &o.path).collect::<Vec<_>>(),
            "a changed loader/mapping version must not reuse the old output path"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_outputs_map_is_resolved_and_verified() {
        let dir = test_dir("outputs_map");
        let libs = dir.join("libraries");
        let out = libs.join("example/output/1.0/output-1.0.jar");
        write_test_jar(&out, 7);
        let sha = crate::download::hash_file_sha1(&out).unwrap();
        let profile = serde_json::json!({
            "data": {},
            "processors": [{
                "outputs": {
                    "{OUT}": "{OUT_SHA}"
                }
            }]
        });
        let mut vars = HashMap::new();
        vars.insert("OUT".to_string(), out.to_string_lossy().to_string());
        vars.insert("OUT_SHA".to_string(), sha);
        let plan = processor_output_plan(&profile, &vars, &libs).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, out);
        assert!(plan[0].checksum.is_some());
        verify_processor_outputs(&plan).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_outputs_are_processed_without_data() {
        for data in [
            serde_json::Value::Null,
            serde_json::json!([]),
            serde_json::json!("not-an-object"),
        ] {
            let dir = test_dir("outputs_no_data");
            let libs = dir.join("libraries");
            let out = libs.join("example/output/1.0/output-1.0.jar");
            write_test_jar(&out, 11);
            let sha = crate::download::hash_file_sha1(&out).unwrap();
            let profile = serde_json::json!({
                "data": data,
                "processors": [{
                    "outputs": {
                        "{OUT}": "{OUT_SHA}"
                    }
                }]
            });
            let mut vars = HashMap::new();
            vars.insert("OUT".to_string(), out.to_string_lossy().to_string());
            vars.insert("OUT_SHA".to_string(), sha);
            let plan = processor_output_plan(&profile, &vars, &libs).unwrap();
            assert_eq!(
                plan.len(),
                1,
                "processors[].outputs must survive absent data"
            );
            verify_processor_outputs(&plan).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn modern_patched_output_without_publisher_hash_is_structural_only() {
        // Synthetic fixture: modern NeoForge profiles can declare PATCHED
        // without a PATCHED_SHA companion. It must still be discovered and
        // marked structural-only, never assigned a fabricated checksum.
        let dir = test_dir("modern_patched");
        let libs = dir.join("libraries");
        let out = libs.join(
            "net/neoforged/minecraft-client-patched/26.3.0.99/minecraft-client-patched-26.3.0.99.jar",
        );
        write_test_jar(&out, 31);
        let profile = serde_json::json!({
            "data": {
                "PATCHED": {
                    "client": "[net.neoforged:minecraft-client-patched:26.3.0.99]"
                }
            }
        });
        let installer = dir.join("installer.jar");
        let client = dir.join("client.jar");
        let root = dir.join("root");
        let vars = build_processor_vars(
            &profile,
            &installer,
            &libs,
            "26.3",
            &client,
            &root,
            "net/neoforged/neoforge",
            "26.3.0.99",
        );
        let plan = processor_output_plan(&profile, &vars, &libs).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, out);
        assert!(plan[0].checksum.is_none());
        verify_processor_outputs(&plan).unwrap();
        let expected = expected_outputs_from_plan(&plan, &libs).unwrap();
        assert!(expected[0].structural_only);
        assert!(expected[0].publisher_sha1.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn processor_outputs_map_supports_multiple_processors_and_rejects_bad_hash() {
        let dir = test_dir("outputs_multiple");
        let libs = dir.join("libraries");
        let first = libs.join("example/first/1.0/first-1.0.jar");
        let second = libs.join("example/second/1.0/second-1.0.jar");
        write_test_jar(&first, 21);
        write_test_jar(&second, 22);
        let first_sha = crate::download::hash_file_sha1(&first).unwrap();
        let second_sha = crate::download::hash_file_sha1(&second).unwrap();
        let profile = serde_json::json!({
            "processors": [
                { "outputs": { "{FIRST}": "{FIRST_SHA}" } },
                { "outputs": { "{SECOND}": "{SECOND_SHA}" } }
            ]
        });
        let mut vars = HashMap::new();
        vars.insert("FIRST".to_string(), first.to_string_lossy().to_string());
        vars.insert("FIRST_SHA".to_string(), first_sha);
        vars.insert("SECOND".to_string(), second.to_string_lossy().to_string());
        vars.insert("SECOND_SHA".to_string(), second_sha);
        let plan = processor_output_plan(&profile, &vars, &libs).unwrap();
        assert_eq!(plan.len(), 2);
        verify_processor_outputs(&plan).unwrap();

        let bad = serde_json::json!({
            "processors": [{ "outputs": { "{FIRST}": "not-a-hash" } }]
        });
        assert!(processor_output_plan(&bad, &vars, &libs).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provider_artifact_hashes_and_sizes_reach_the_loader_profile() {
        let lib: ForgeLibrary = serde_json::from_value(serde_json::json!({
            "name": "example:library:1.0",
            "downloads": {
                "artifact": {
                    "path": "example/library/1.0/library-1.0.jar",
                    "url": "https://maven.example.invalid/library-1.0.jar",
                    "sha1": "0123456789abcdef0123456789abcdef01234567",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "sha512": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size": 4242
                }
            }
        }))
        .expect("forge library fixture");
        let profile = build_profile("Main".into(), vec![lib], None, None).unwrap();
        let library = &profile.libraries[0];
        assert_eq!(
            library.sha1.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(library.sha256.is_some());
        assert!(library.sha512.is_some());
        assert_eq!(library.checksum_source.as_deref(), Some("metadata"));
        assert_eq!(library.checksum().unwrap().unwrap().algorithm(), "sha512");
        assert_eq!(library.size, Some(4242));
    }

    #[test]
    fn missing_processor_java_message_matches_the_actual_install_capability() {
        let no_context = missing_processor_java_message(17, false);
        assert!(no_context.contains("Java 17"));
        assert!(no_context.contains("Install a matching JDK"));
        assert!(!no_context.contains("managed Java download did not produce"));

        let managed_failed = missing_processor_java_message(17, true);
        assert!(managed_failed.contains("managed Java download did not produce"));
    }

    #[test]
    fn forge_java_matrix() {
        assert_eq!(required_forge_java_major("1.8.9"), 8);
        assert_eq!(required_forge_java_major("1.12.2"), 8);
        assert_eq!(required_forge_java_major("1.16.5"), 8);
        assert_eq!(required_forge_java_major("1.17.1"), 16);
        assert_eq!(required_forge_java_major("1.18.2"), 17);
        assert_eq!(required_forge_java_major("1.20.1"), 17);
        assert_eq!(required_forge_java_major("1.20.4"), 17);
        assert_eq!(required_forge_java_major("1.20.5"), 21);
        assert_eq!(required_forge_java_major("1.21.4"), 21);
        // Unknown/unparseable input degrades to the oldest supported Java.
        assert_eq!(required_forge_java_major(""), 8);
        assert_eq!(required_forge_java_major("banana"), 8);
    }

    /// Two loader versions must never share extracted installer data files —
    /// the shared-path leak corrupted a 1.20.1 patching run with 1.13.2 data.
    #[test]
    fn data_file_path_is_version_scoped() {
        let p = data_file_path(
            Path::new("C:/libs"),
            "net/minecraftforge/forge",
            "1.20.1-47.4.20",
            "BINPATCH",
            "client.lzma",
        );
        let s = p.to_string_lossy().replace('\\', "/");
        assert_eq!(
            s,
            "C:/libs/net/minecraftforge/forge/_data/1.20.1-47.4.20/binpatch/client.lzma"
        );

        let p2 = data_file_path(
            Path::new("C:/libs"),
            "net/minecraftforge/forge",
            "1.13.2-25.0.223",
            "BINPATCH",
            "client.lzma",
        );
        assert_ne!(p, p2, "different loader versions must use different files");
    }

    /// The structural check must reject the exact artifact a failed
    /// processor leaves behind (empty/truncated archives) while accepting a
    /// real JAR — and it must never be confused with a checksum check.
    #[test]
    fn usable_jar_rejects_missing_empty_and_truncated_files() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("vl_usablejar_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Missing file.
        assert!(!is_usable_jar(&dir.join("nope.jar")));

        // The failure mode seen in the wild: binarypatcher's 22-byte
        // end-of-central-directory-only archive (a structurally valid but
        // EMPTY zip).
        let empty_zip = dir.join("empty.jar");
        let mut bytes = vec![0x50, 0x4B, 0x05, 0x06];
        bytes.extend_from_slice(&[0u8; 18]);
        std::fs::write(&empty_zip, &bytes).unwrap();
        assert!(!is_usable_jar(&empty_zip));

        // A real archive with content passes (Stored so the on-disk size
        // stays above the size floor instead of compressing to a few bytes).
        let ok_jar = dir.join("ok.jar");
        let f = std::fs::File::create(&ok_jar).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("data.bin", opts).unwrap();
        zw.write_all(&vec![0u8; 4096]).unwrap();
        zw.finish().unwrap();
        assert!(is_usable_jar(&ok_jar));

        // Truncated write (interrupted download/extract) fails to open.
        let truncated = dir.join("truncated.jar");
        let full = std::fs::read(&ok_jar).unwrap();
        std::fs::write(&truncated, &full[..full.len() / 2]).unwrap();
        assert!(!is_usable_jar(&truncated));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn installer_cache_requires_zip_with_metadata() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("vl_installer_cache_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Junk that is big enough to pass a size-only check.
        let junk = dir.join("junk.jar");
        std::fs::write(&junk, vec![0x41u8; 8192]).unwrap();
        assert!(!installer_jar_is_valid(&junk));

        // A valid zip without installer metadata is still not an installer.
        let nometa = dir.join("nometa.jar");
        let f = std::fs::File::create(&nometa).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("hello.txt", opts).unwrap();
        zw.write_all(&vec![0u8; 4096]).unwrap();
        zw.finish().unwrap();
        assert!(!installer_jar_is_valid(&nometa));

        // A readable archive carrying install_profile.json is usable.
        let good = dir.join("good.jar");
        let f = std::fs::File::create(&good).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        zw.start_file("install_profile.json", opts).unwrap();
        zw.write_all(&vec![b' '; 2048]).unwrap();
        zw.start_file("version.json", opts).unwrap();
        zw.write_all(b"{}").unwrap();
        zw.finish().unwrap();
        assert!(installer_jar_is_valid(&good));

        // Truncated archive is rejected even if the file is large.
        let truncated = dir.join("truncated.jar");
        let full = std::fs::read(&good).unwrap();
        std::fs::write(&truncated, &full[..full.len() / 2]).unwrap();
        assert!(!installer_jar_is_valid(&truncated));

        std::fs::remove_dir_all(&dir).ok();
    }

    fn test_jar_bytes(seed: u8) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
            zip.write_all(b"Manifest-Version: 1.0\n").unwrap();
            zip.start_file("payload.bin", options).unwrap();
            zip.write_all(&vec![seed; 4096]).unwrap();
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn fake_installer(
        install_profile: serde_json::Value,
        version_json: Option<serde_json::Value>,
        embedded: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("install_profile.json", options).unwrap();
            zip.write_all(serde_json::to_string(&install_profile).unwrap().as_bytes())
                .unwrap();
            if let Some(version_json) = version_json {
                zip.start_file("version.json", options).unwrap();
                zip.write_all(serde_json::to_string(&version_json).unwrap().as_bytes())
                    .unwrap();
            }
            for (relative_path, bytes) in embedded {
                zip.start_file(format!("maven/{relative_path}"), options)
                    .unwrap();
                zip.write_all(bytes).unwrap();
            }
            // Keep the installer above installer_jar_is_valid's minimum.
            zip.start_file("padding.bin", options).unwrap();
            zip.write_all(&vec![0u8; 4096]).unwrap();
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn seed_installer_cache(
        root: &Path,
        loader: &str,
        mc_version: &str,
        loader_version: &str,
        installer: &[u8],
    ) -> ProcessorCache {
        let cache = ProcessorCache::new(root, loader, mc_version, loader_version).unwrap();
        cache.prepare().unwrap();
        std::fs::write(&cache.installer, installer).unwrap();
        let sha1 = crate::download::hash_file_sha1(&cache.installer).unwrap();
        let identity = InstallerIdentity {
            schema_version: 1,
            loader: loader.to_string(),
            mc_version: mc_version.to_string(),
            loader_version: loader_version.to_string(),
            sha256: sha256_bytes(installer),
            size: installer.len() as u64,
            checksum_algorithm: "sha1".to_string(),
            checksum_hex: sha1,
            checksum_source: "test".to_string(),
            url: format!(
                "https://maven.minecraftforge.net/{loader}/{mc_version}/{loader_version}/installer.jar"
            ),
        };
        cache.write_installer_identity(&identity).unwrap();
        cache
    }

    fn legacy_embedded_fixture(
        mc_version: &str,
        loader_version: &str,
        with_version_json: bool,
        universal: bool,
    ) -> (Vec<u8>, String, String) {
        let plain_rel = format!(
            "net/minecraftforge/forge/{mc_version}-{loader_version}/forge-{mc_version}-{loader_version}.jar"
        );
        let universal_rel = format!(
            "net/minecraftforge/forge/{mc_version}-{loader_version}/forge-{mc_version}-{loader_version}-universal.jar"
        );
        let plain = test_jar_bytes(11);
        let universal_bytes = test_jar_bytes(12);
        let mut libraries = vec![serde_json::json!({
            "name": format!("net.minecraftforge:forge:{mc_version}-{loader_version}"),
            "downloads": {
                "artifact": {
                    "path": plain_rel,
                    "url": "",
                    "sha1": crate::download::tests::sha1_hex(&plain),
                    "size": plain.len(),
                }
            }
        })];
        if universal {
            libraries.push(serde_json::json!({
                "name": format!("net.minecraftforge:forge:{mc_version}-{loader_version}:universal"),
                "downloads": {
                    "artifact": {
                        "path": universal_rel,
                        "url": "",
                        "sha1": crate::download::tests::sha1_hex(&universal_bytes),
                        "size": universal_bytes.len(),
                    }
                }
            }));
        }
        let install_profile = serde_json::json!({ "libraries": libraries });
        let version_json = with_version_json.then(|| {
            serde_json::json!({
                "id": mc_version,
                "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
                "libraries": libraries
            })
        });
        let mut embedded: Vec<(&str, &[u8])> = vec![(plain_rel.as_str(), &plain)];
        if universal {
            embedded.push((universal_rel.as_str(), &universal_bytes));
        }
        let installer = fake_installer(install_profile, version_json, &embedded);
        (installer, plain_rel, universal_rel)
    }

    #[tokio::test]
    async fn legacy_embedded_forge_libraries_are_recovered_and_verified() {
        for (mc_version, loader_version, with_version_json, universal) in [
            ("1.12.2", "14.23.5.2860", false, false),
            ("1.13.2", "25.0.223", true, true),
            ("1.16.5", "36.2.39", true, true),
        ] {
            let dir = test_dir(&format!("embedded_{mc_version}"));
            let libraries = dir.join("libraries");
            let versions = dir.join("versions");
            std::fs::create_dir_all(&libraries).unwrap();
            std::fs::create_dir_all(&versions).unwrap();
            let (installer, plain_rel, universal_rel) =
                legacy_embedded_fixture(mc_version, loader_version, with_version_json, universal);
            let full_version = format!("{mc_version}-{loader_version}");
            let _cache = seed_installer_cache(&dir, "forge", mc_version, &full_version, &installer);

            ensure_processor_jars(mc_version, loader_version, &libraries, &versions, None)
                .await
                .expect("embedded libraries must be restored without processors");
            let plain = std::fs::read(libraries.join(&plain_rel)).unwrap();
            assert!(
                !plain.is_empty(),
                "{mc_version}: plain embedded JAR missing"
            );
            if universal {
                let universal_bytes = std::fs::read(libraries.join(&universal_rel)).unwrap();
                assert!(
                    !universal_bytes.is_empty(),
                    "{mc_version}: universal embedded JAR missing"
                );
            }
            let inspection = inspect_generation(mc_version, loader_version, &libraries, &versions);
            assert_eq!(
                inspection.state,
                GenerationState::Healthy,
                "{mc_version}: {:?}",
                inspection.issues
            );

            // Missing file: Verify must say NeedsRepair, and ensure must restore it.
            std::fs::remove_file(libraries.join(&plain_rel)).unwrap();
            let inspection = inspect_generation(mc_version, loader_version, &libraries, &versions);
            assert_eq!(inspection.state, GenerationState::NeedsRepair);
            assert!(
                inspection
                    .issues
                    .iter()
                    .any(|issue| issue.kind == "missing_embedded_library"),
                "{mc_version}: {:?}",
                inspection.issues
            );
            ensure_processor_jars(mc_version, loader_version, &libraries, &versions, None)
                .await
                .unwrap();
            assert_eq!(
                std::fs::read(libraries.join(&plain_rel)).unwrap(),
                plain,
                "{mc_version}: restore must publish the exact installer bytes"
            );

            // Same-size corruption: a size check alone must not accept it.
            let mut corrupt = plain.clone();
            corrupt[0] ^= 0xff;
            std::fs::write(libraries.join(&plain_rel), &corrupt).unwrap();
            let inspection = inspect_generation(mc_version, loader_version, &libraries, &versions);
            assert_eq!(inspection.state, GenerationState::NeedsRepair);
            assert!(
                inspection
                    .issues
                    .iter()
                    .any(|issue| issue.kind == "corrupt_embedded_library"),
                "{mc_version}: {:?}",
                inspection.issues
            );
            ensure_processor_jars(mc_version, loader_version, &libraries, &versions, None)
                .await
                .unwrap();
            assert_eq!(
                std::fs::read(libraries.join(&plain_rel)).unwrap(),
                plain,
                "{mc_version}: corrupt embedded JAR must be replaced"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[tokio::test]
    async fn concurrent_embedded_preparation_publishes_complete_bytes() {
        let dir = test_dir("embedded_concurrent");
        let libraries = dir.join("libraries");
        let versions = dir.join("versions");
        std::fs::create_dir_all(&libraries).unwrap();
        std::fs::create_dir_all(&versions).unwrap();
        let (installer, plain_rel, _) = legacy_embedded_fixture("1.16.5", "36.2.39", true, true);
        let full_version = "1.16.5-36.2.39";
        let _cache = seed_installer_cache(&dir, "forge", "1.16.5", full_version, &installer);

        let first = ensure_processor_jars("1.16.5", "36.2.39", &libraries, &versions, None);
        let second = ensure_processor_jars("1.16.5", "36.2.39", &libraries, &versions, None);
        let (a, b) = tokio::join!(first, second);
        a.unwrap();
        b.unwrap();
        let bytes = std::fs::read(libraries.join(&plain_rel)).unwrap();
        assert!(!bytes.is_empty());
        assert!(
            !libraries
                .join(plain_rel)
                .parent()
                .unwrap()
                .read_dir()
                .unwrap()
                .any(|entry| entry
                    .ok()
                    .map(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
                    .unwrap_or(false)),
            "temporary embedded files must not survive publication"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn generation_inspection_detects_missing_corrupt_and_stale_outputs() {
        let dir = test_dir("generation_inspection");
        let libraries = dir.join("libraries");
        let versions = dir.join("versions").join("1.20.1");
        std::fs::create_dir_all(&libraries).unwrap();
        std::fs::create_dir_all(&versions).unwrap();
        let client = versions.join("client.jar");
        write_test_jar(&client, 2);
        let output_rel = "net/minecraftforge/test/1.0/test-1.0-slim.jar";
        let output = libraries.join(output_rel);
        write_test_jar(&output, 4);
        let output_sha1 = crate::download::hash_file_sha1(&output).unwrap();
        let output_size = std::fs::metadata(&output).unwrap().len();
        let install_profile = serde_json::json!({
            "libraries": [],
            "processors": [{
                "jar": "net.minecraftforge:installertools:1.0",
                "classpath": [],
                "args": [],
                "sides": ["client"]
            }],
            "data": {
                "MC_SLIM": { "client": "[net.minecraftforge:test:1.0:slim]" },
                "MC_SLIM_SHA": { "client": format!("'{}'", output_sha1) }
            }
        });
        let installer = fake_installer(install_profile.clone(), None, &[]);
        let full_version = "1.20.1-47.4.29";
        let cache = seed_installer_cache(&dir, "forge", "1.20.1", full_version, &installer);

        let vars = build_processor_vars(
            &install_profile,
            &cache.installer,
            &libraries,
            "1.20.1",
            &client,
            &dir,
            "net/minecraftforge/forge",
            full_version,
        );
        let plan = processor_output_plan(&install_profile, &vars, &libraries).unwrap();
        let processors = processor_identities_from_install_profile(&install_profile).unwrap();
        let manifest = build_generation_manifest(
            "forge",
            "1.20.1",
            full_version,
            cache.load_installer_identity().unwrap().unwrap(),
            sha256_bytes(serde_json::to_string(&install_profile).unwrap().as_bytes()),
            &client,
            &libraries,
            &plan,
            processors,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].checksum.as_ref().unwrap().hex(), output_sha1);
        assert_eq!(output_size, std::fs::metadata(&output).unwrap().len());
        cache.write_generation_manifest(&manifest).unwrap();

        let inspection = inspect_generation("1.20.1", "47.4.29", &libraries, &dir.join("versions"));
        assert_eq!(
            inspection.state,
            GenerationState::Healthy,
            "{:?}",
            inspection.issues
        );

        std::fs::remove_file(&output).unwrap();
        let inspection = inspect_generation("1.20.1", "47.4.29", &libraries, &dir.join("versions"));
        assert_eq!(inspection.state, GenerationState::NeedsRepair);
        assert!(inspection
            .issues
            .iter()
            .any(|issue| issue.kind == "missing_generated_output"));

        write_test_jar(&output, 9);
        let inspection = inspect_generation("1.20.1", "47.4.29", &libraries, &dir.join("versions"));
        assert_eq!(inspection.state, GenerationState::NeedsRepair);
        assert!(inspection
            .issues
            .iter()
            .any(|issue| issue.kind == "corrupt_generated_output"));

        write_test_jar(&output, 4);
        std::fs::remove_file(&cache.generation_manifest).unwrap();
        let inspection = inspect_generation("1.20.1", "47.4.29", &libraries, &dir.join("versions"));
        assert_eq!(inspection.state, GenerationState::NeedsRepair);
        assert!(inspection
            .issues
            .iter()
            .any(|issue| issue.kind == "missing_generation_manifest"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn neoforge_structurally_valid_but_wrong_client_jar_is_not_healthy() {
        let dir = test_dir("neoforge_generation_wrong");
        let libraries = dir.join("libraries");
        let versions = dir.join("versions").join("1.21.4");
        std::fs::create_dir_all(&libraries).unwrap();
        std::fs::create_dir_all(&versions).unwrap();
        let client = versions.join("client.jar");
        write_test_jar(&client, 2);
        let output_rel = "net/neoforged/neoforge/21.4.157/neoforge-21.4.157-client.jar";
        let output = libraries.join(output_rel);
        write_test_jar(&output, 4);
        let output_sha1 = crate::download::hash_file_sha1(&output).unwrap();
        let install_profile = serde_json::json!({
            "libraries": [],
            "processors": [{
                "jar": "net.neoforged:installertools:1.0",
                "classpath": [],
                "args": [],
                "sides": ["client"]
            }],
            "data": {
                "MC_SLIM": { "client": "[net.neoforged:neoforge:21.4.157:client]" },
                "MC_SLIM_SHA": { "client": format!("'{}'", output_sha1) }
            }
        });
        let installer = fake_installer(install_profile.clone(), None, &[]);
        let cache = seed_installer_cache(&dir, "neoforge", "1.21.4", "21.4.157", &installer);
        let vars = build_processor_vars(
            &install_profile,
            &cache.installer,
            &libraries,
            "1.21.4",
            &client,
            &dir,
            "net/neoforged/neoforge",
            "21.4.157",
        );
        let plan = processor_output_plan(&install_profile, &vars, &libraries).unwrap();
        assert_eq!(plan.len(), 1);
        let processors = processor_identities_from_install_profile(&install_profile).unwrap();
        let manifest = build_generation_manifest(
            "neoforge",
            "1.21.4",
            "21.4.157",
            cache.load_installer_identity().unwrap().unwrap(),
            sha256_bytes(serde_json::to_string(&install_profile).unwrap().as_bytes()),
            &client,
            &libraries,
            &plan,
            processors,
            Vec::new(),
        )
        .unwrap();
        cache.write_generation_manifest(&manifest).unwrap();
        let inspection = crate::modloaders::neoforge::inspect_generation(
            "1.21.4",
            "21.4.157",
            &libraries,
            &dir.join("versions"),
        );
        assert_eq!(
            inspection.state,
            GenerationState::Healthy,
            "{:?}",
            inspection.issues
        );

        // Structurally valid JAR with different bytes: the manifest hash, not
        // find_client_jar's structural check, must catch it.
        write_test_jar(&output, 9);
        let inspection = crate::modloaders::neoforge::inspect_generation(
            "1.21.4",
            "21.4.157",
            &libraries,
            &dir.join("versions"),
        );
        assert_eq!(inspection.state, GenerationState::NeedsRepair);
        assert!(
            inspection
                .issues
                .iter()
                .any(|issue| issue.kind == "corrupt_generated_output"),
            "{:?}",
            inspection.issues
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn generation_inspection_reports_busy_without_false_health() {
        let dir = test_dir("generation_busy");
        let libraries = dir.join("libraries");
        let versions = dir.join("versions");
        std::fs::create_dir_all(&libraries).unwrap();
        std::fs::create_dir_all(&versions).unwrap();
        let (installer, _, _) = legacy_embedded_fixture("1.12.2", "14.23.5.2860", false, false);
        let full_version = "1.12.2-14.23.5.2860";
        let cache = seed_installer_cache(&dir, "forge", "1.12.2", full_version, &installer);
        let _guard = cache.acquire_operation_lock().await;
        let inspection = inspect_generation("1.12.2", "14.23.5.2860", &libraries, &versions);
        assert_eq!(inspection.state, GenerationState::Busy);
        assert!(inspection
            .issues
            .iter()
            .any(|issue| issue.kind == "verification_incomplete"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

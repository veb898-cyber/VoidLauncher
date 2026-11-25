//! Backend-side resolution of catalog content before download.
//!
//! The frontend sends identifiers plus the user's file selection. It must not
//! be the source of truth for the download URL or checksum: the provider API
//! response is. These functions are pure (except for the API calls made by the
//! command layer) so the matching and hash rules are unit-testable without
//! network access.

use crate::curseforge::CfFile;
use crate::error::{LauncherError, Result};
use crate::integrity::HashSpec;
use crate::modrinth::{ModrinthFile, ModrinthVersionResponse};
use std::collections::HashMap;

/// A file resolved from trusted provider metadata.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedContent {
    pub provider: &'static str,
    pub project_id: String,
    pub version_id: String,
    pub file_name: String,
    /// API-provided URL. For CurseForge this may be `None`; the native
    /// downloader resolves the signed/CDN chain from the file id.
    pub url: Option<String>,
    pub file_id: Option<String>,
    pub hash: HashSpec,
    pub size: Option<u64>,
    /// API-provided SHA-1 when present, for the sidecar/update index. This is
    /// never used as the download checksum when a stronger hash is available.
    pub sha1: Option<String>,
}

fn content_error(message: impl Into<String>) -> LauncherError {
    LauncherError::Download(message.into())
}

pub(crate) fn ensure_api_key(api_key: &str) -> Result<()> {
    if api_key.trim().is_empty() {
        Err(content_error(
            "CurseForge API key is not configured; remote content install is unavailable",
        ))
    } else {
        Ok(())
    }
}

fn validate_modrinth_id(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(content_error(format!("Invalid Modrinth {label}")));
    }
    Ok(())
}

fn api_sha1_from_map(hashes: &HashMap<String, String>) -> Result<Option<HashSpec>> {
    match hashes.get("sha1") {
        Some(value) => HashSpec::from_sha1(value)
            .map_err(|e| content_error(format!("Invalid Modrinth SHA-1 metadata: {e}"))),
        None => Ok(None),
    }
}

fn cross_check_frontend_sha1(provided: &str, api_sha1: Option<&HashSpec>) -> Result<()> {
    if provided.is_empty() {
        return Ok(());
    }
    let frontend = HashSpec::from_sha1(provided)
        .map_err(|_| content_error("Frontend supplied an invalid SHA-1 selector"))?
        .ok_or_else(|| content_error("Frontend supplied an empty SHA-1 selector"))?;
    if let Some(api_sha1) = api_sha1 {
        if !frontend.hex().eq_ignore_ascii_case(api_sha1.hex()) {
            return Err(content_error(
                "Frontend SHA-1 does not match the provider metadata",
            ));
        }
    }
    // If the provider has no SHA-1 but has a valid stronger hash, the stronger
    // provider hash remains the source of truth; there is nothing to compare.
    Ok(())
}

/// Resolve the exact Modrinth file selected by the user.
///
/// The filename is the primary selector. If a version contains multiple files
/// with the same name, the frontend-provided URL is used only to disambiguate;
/// it never becomes the download URL. The returned URL and hashes always come
/// from the API response.
pub(crate) fn resolve_modrinth(
    version: &ModrinthVersionResponse,
    expected_project_id: &str,
    selected_filename: &str,
    selected_url: &str,
    frontend_sha1: &str,
) -> Result<ResolvedContent> {
    let project_id = expected_project_id.trim();
    validate_modrinth_id(project_id, "project id")?;
    validate_modrinth_id(&version.id, "version id")?;
    if version.project_id != project_id {
        return Err(content_error(
            "Modrinth version does not belong to the requested project",
        ));
    }
    if selected_filename.is_empty() {
        return Err(content_error("Selected file name is empty"));
    }

    let mut matches: Vec<&ModrinthFile> = version
        .files
        .iter()
        .filter(|file| file.filename == selected_filename)
        .collect();
    if matches.is_empty() {
        return Err(content_error(format!(
            "Selected file '{}' is not part of Modrinth version {}",
            selected_filename, version.id
        )));
    }
    if matches.len() > 1 {
        if selected_url.is_empty() {
            return Err(content_error(format!(
                "Modrinth version {} contains multiple files named '{}'; a URL selector is required",
                version.id, selected_filename
            )));
        }
        let by_url: Vec<&ModrinthFile> = matches
            .iter()
            .copied()
            .filter(|file| file.url == selected_url)
            .collect();
        if by_url.len() != 1 {
            return Err(content_error(format!(
                "Selected file '{}' is ambiguous in Modrinth version {}",
                selected_filename, version.id
            )));
        }
        matches = by_url;
    }

    let file = matches[0];
    if !file.url.starts_with("https://") || !crate::is_allowed_download_host(&file.url) {
        return Err(content_error(
            "Modrinth returned a download URL outside the trusted HTTPS allowlist",
        ));
    }
    let hash = HashSpec::from_metadata(&file.hashes)
        .map_err(|e| content_error(format!("Modrinth checksum metadata is invalid: {e}")))?
        .ok_or_else(|| {
            content_error(format!(
                "Modrinth file '{}' has no usable checksum; refusing to install it",
                file.filename
            ))
        })?;
    let api_sha1 = api_sha1_from_map(&file.hashes)?;
    cross_check_frontend_sha1(frontend_sha1, api_sha1.as_ref())?;

    Ok(ResolvedContent {
        provider: "modrinth",
        project_id: version.project_id.clone(),
        version_id: version.id.clone(),
        file_name: file.filename.clone(),
        url: Some(file.url.clone()),
        file_id: None,
        hash,
        size: (file.size > 0).then_some(file.size),
        sha1: api_sha1.map(|spec| spec.hex().to_string()),
    })
}

/// Resolve the pack archive selected by a Modrinth version.
///
/// Pack installs select a version, not an individual file. The backend picks
/// the only file or the single primary file and its strongest publisher hash;
/// the frontend never supplies the archive URL or checksum.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedModrinthArchive {
    pub url: String,
    pub file_name: String,
    pub hash: HashSpec,
    pub size: Option<u64>,
}

pub(crate) fn resolve_modrinth_archive(files: &[ModrinthFile]) -> Result<ResolvedModrinthArchive> {
    // A modpack install selects a *version*, not a file. The file must be
    // unambiguous: the only file, or the single primary file. Falling back to
    // "the first file" would silently install an arbitrary extra artifact when
    // the version carries more than one file.
    let file = match files {
        [] => {
            return Err(content_error(
                "Modrinth version has no downloadable archive",
            ))
        }
        [only] => only,
        _ => {
            let mut primaries = files.iter().filter(|file| file.primary);
            match (primaries.next(), primaries.next()) {
                (Some(primary), None) => primary,
                (None, _) => {
                    return Err(content_error(
                        "Modrinth version contains multiple files and no primary archive; refusing to choose one",
                    ))
                }
                (Some(_), Some(_)) => {
                    return Err(content_error(
                        "Modrinth version contains multiple primary files; the archive is ambiguous",
                    ))
                }
            }
        }
    };
    if !file.url.starts_with("https://") || !crate::is_allowed_download_host(&file.url) {
        return Err(content_error(
            "Modrinth archive URL is outside the trusted HTTPS allowlist",
        ));
    }
    let hash = HashSpec::from_metadata(&file.hashes)
        .map_err(|e| {
            content_error(format!(
                "Modrinth archive checksum metadata is invalid: {e}"
            ))
        })?
        .ok_or_else(|| {
            content_error(format!(
                "Modrinth archive '{}' has no usable checksum; refusing to download it",
                file.filename
            ))
        })?;
    Ok(ResolvedModrinthArchive {
        url: file.url.clone(),
        file_name: file.filename.clone(),
        hash,
        size: (file.size > 0).then_some(file.size),
    })
}

pub(crate) async fn download_modrinth_archive(
    archive: &ResolvedModrinthArchive,
    dest: &std::path::Path,
) -> Result<()> {
    if let Some(size) = archive.size {
        crate::download::download_file_sized_verified(&archive.url, dest, &archive.hash, size).await
    } else {
        crate::download::download_file_verified(&archive.url, dest, &archive.hash).await
    }
}

/// Resolve the exact CurseForge file selected by the user.
///
/// The caller must pass metadata fetched for the requested file id. The
/// `modId` field is checked against the expected project; unknown hash
/// algorithms are ignored and never treated as SHA-1.
pub(crate) fn resolve_curseforge(
    files: &[CfFile],
    project_id: u64,
    file_id: u64,
    frontend_sha1: &str,
) -> Result<ResolvedContent> {
    if project_id == 0 || file_id == 0 {
        return Err(content_error("Invalid CurseForge project or file id"));
    }
    let mut matches = files.iter().filter(|file| file.id == file_id);
    let file = matches
        .next()
        .ok_or_else(|| content_error("CurseForge file metadata was not returned"))?;
    if matches.next().is_some() {
        return Err(content_error(
            "CurseForge returned duplicate metadata for the requested file",
        ));
    }
    if file.mod_id != project_id {
        return Err(content_error(
            "CurseForge file does not belong to the requested project",
        ));
    }
    if file.is_available == Some(false) {
        return Err(content_error("CurseForge file is not available"));
    }
    if file.file_name.is_empty() {
        return Err(content_error("CurseForge returned an empty file name"));
    }

    let sha1_value = file
        .hashes
        .iter()
        .find(|hash| hash.algo == 1)
        .map(|hash| hash.value.as_str())
        .ok_or_else(|| {
            content_error(format!(
                "CurseForge file {} has no SHA-1 checksum; refusing to install it",
                file_id
            ))
        })?;
    let api_sha1 = HashSpec::from_sha1(sha1_value)
        .map_err(|e| content_error(format!("CurseForge SHA-1 metadata is invalid: {e}")))?
        .ok_or_else(|| content_error("CurseForge SHA-1 metadata is empty"))?;
    cross_check_frontend_sha1(frontend_sha1, Some(&api_sha1))?;

    Ok(ResolvedContent {
        provider: "curseforge",
        project_id: project_id.to_string(),
        version_id: format!("cf:{project_id}:{file_id}"),
        file_name: file.file_name.clone(),
        url: file.download_url.clone(),
        file_id: Some(file_id.to_string()),
        hash: api_sha1.clone(),
        size: (file.file_length > 0).then_some(file.file_length),
        sha1: Some(api_sha1.hex().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curseforge::CfHash;
    use crate::modrinth::ModrinthVersionResponse;
    use std::collections::HashMap;

    const SHA1_A: &str = "1111111111111111111111111111111111111111";
    const SHA1_B: &str = "2222222222222222222222222222222222222222";
    const SHA256_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA512_A: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn mr_file(filename: &str, url: &str, primary: bool, hashes: &[(&str, &str)]) -> ModrinthFile {
        ModrinthFile {
            filename: filename.to_string(),
            url: url.to_string(),
            primary,
            size: 123,
            hashes: hashes
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect::<HashMap<_, _>>(),
        }
    }

    fn mr_version(files: Vec<ModrinthFile>) -> ModrinthVersionResponse {
        ModrinthVersionResponse {
            id: "version-1".to_string(),
            project_id: "project-1".to_string(),
            name: "Version 1".to_string(),
            version_number: "1.0.0".to_string(),
            game_versions: vec!["1.20.1".to_string()],
            loaders: vec!["fabric".to_string()],
            files,
            dependencies: vec![],
            date_published: String::new(),
            version_type: "release".to_string(),
        }
    }

    fn cf_file(id: u64, mod_id: u64, name: &str, hashes: Vec<CfHash>) -> CfFile {
        CfFile {
            id,
            mod_id,
            display_name: name.to_string(),
            file_name: name.to_string(),
            download_url: None,
            file_length: 123,
            file_date: "2026-01-01T00:00:00Z".to_string(),
            game_versions: vec!["1.20.1".to_string()],
            dependencies: vec![],
            release_type: 1,
            hashes,
            is_available: Some(true),
        }
    }

    fn sha1_hash(value: &str) -> CfHash {
        CfHash {
            value: value.to_string(),
            algo: 1,
        }
    }

    #[test]
    fn modrinth_selects_the_only_file_and_prefers_sha512() {
        let file = mr_file(
            "only.jar",
            "https://cdn.modrinth.com/data/only.jar",
            true,
            &[("sha1", SHA1_A), ("sha512", SHA512_A)],
        );
        let resolved = resolve_modrinth(
            &mr_version(vec![file]),
            "project-1",
            "only.jar",
            "https://cdn.modrinth.com/data/only.jar",
            "",
        )
        .unwrap();

        assert_eq!(resolved.file_name, "only.jar");
        assert_eq!(
            resolved.url.as_deref(),
            Some("https://cdn.modrinth.com/data/only.jar")
        );
        assert_eq!(resolved.hash.algorithm(), "sha512");
        assert_eq!(resolved.sha1.as_deref(), Some(SHA1_A));
    }

    #[test]
    fn modrinth_uses_sha256_when_sha512_is_absent() {
        let file = mr_file(
            "only.jar",
            "https://cdn.modrinth.com/data/only.jar",
            true,
            &[("sha256", SHA256_A)],
        );
        let resolved =
            resolve_modrinth(&mr_version(vec![file]), "project-1", "only.jar", "", "").unwrap();
        assert_eq!(resolved.hash.algorithm(), "sha256");
        assert_eq!(resolved.hash.hex(), SHA256_A);
    }

    #[test]
    fn modrinth_selects_the_additional_file_not_primary() {
        let primary = mr_file(
            "primary.jar",
            "https://cdn.modrinth.com/data/primary.jar",
            true,
            &[("sha1", SHA1_A)],
        );
        let extra = mr_file(
            "extra.jar",
            "https://cdn.modrinth.com/data/extra.jar",
            false,
            &[("sha1", SHA1_B)],
        );
        let resolved = resolve_modrinth(
            &mr_version(vec![primary, extra]),
            "project-1",
            "extra.jar",
            "https://cdn.modrinth.com/data/extra.jar",
            "",
        )
        .unwrap();

        assert_eq!(resolved.file_name, "extra.jar");
        assert_eq!(resolved.sha1.as_deref(), Some(SHA1_B));
    }

    #[test]
    fn modrinth_rejects_ambiguous_duplicate_filenames_without_a_unique_url() {
        let first = mr_file(
            "same.jar",
            "https://cdn.modrinth.com/data/first.jar",
            true,
            &[("sha1", SHA1_A)],
        );
        let second = mr_file(
            "same.jar",
            "https://cdn.modrinth.com/data/second.jar",
            false,
            &[("sha1", SHA1_B)],
        );
        let err = resolve_modrinth(
            &mr_version(vec![first, second]),
            "project-1",
            "same.jar",
            "",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("multiple files"), "got: {err}");
    }

    #[test]
    fn modrinth_uses_the_url_only_to_disambiguate_not_as_download_source() {
        let first = mr_file(
            "same.jar",
            "https://cdn.modrinth.com/data/first.jar",
            true,
            &[("sha1", SHA1_A)],
        );
        let second = mr_file(
            "same.jar",
            "https://cdn.modrinth.com/data/second.jar",
            false,
            &[("sha1", SHA1_B)],
        );
        let resolved = resolve_modrinth(
            &mr_version(vec![first, second]),
            "project-1",
            "same.jar",
            "https://cdn.modrinth.com/data/second.jar",
            "",
        )
        .unwrap();

        assert_eq!(
            resolved.url.as_deref(),
            Some("https://cdn.modrinth.com/data/second.jar")
        );
        assert_eq!(resolved.sha1.as_deref(), Some(SHA1_B));
    }

    #[test]
    fn modrinth_rejects_project_mismatch_and_missing_file() {
        let file = mr_file(
            "only.jar",
            "https://cdn.modrinth.com/data/only.jar",
            true,
            &[("sha1", SHA1_A)],
        );
        let version = mr_version(vec![file]);
        assert!(resolve_modrinth(&version, "other-project", "only.jar", "", "").is_err());
        assert!(resolve_modrinth(&version, "project-1", "missing.jar", "", "").is_err());
    }

    #[test]
    fn modrinth_rejects_missing_or_malformed_checksums() {
        let no_hash = mr_file(
            "no-hash.jar",
            "https://cdn.modrinth.com/data/no-hash.jar",
            true,
            &[],
        );
        assert!(resolve_modrinth(
            &mr_version(vec![no_hash]),
            "project-1",
            "no-hash.jar",
            "",
            ""
        )
        .is_err());

        let bad_hash = mr_file(
            "bad-hash.jar",
            "https://cdn.modrinth.com/data/bad-hash.jar",
            true,
            &[("sha512", "not-a-hash")],
        );
        assert!(resolve_modrinth(
            &mr_version(vec![bad_hash]),
            "project-1",
            "bad-hash.jar",
            "",
            ""
        )
        .is_err());
    }

    #[test]
    fn modrinth_cross_checks_frontend_sha1_and_ignores_tampered_url_for_unique_names() {
        let file = mr_file(
            "only.jar",
            "https://cdn.modrinth.com/data/only.jar",
            true,
            &[("sha1", SHA1_A), ("sha512", SHA512_A)],
        );
        let version = mr_version(vec![file]);

        assert!(resolve_modrinth(
            &version,
            "project-1",
            "only.jar",
            "https://attacker.example/only.jar",
            SHA1_A
        )
        .is_ok());

        let err = resolve_modrinth(
            &version,
            "project-1",
            "only.jar",
            "https://attacker.example/only.jar",
            SHA1_B,
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not match"), "got: {err}");
    }

    #[test]
    fn modrinth_rejects_a_url_on_a_host_outside_the_allowlist() {
        let file = mr_file(
            "only.jar",
            "https://attacker.example/only.jar",
            true,
            &[("sha1", SHA1_A)],
        );
        assert!(
            resolve_modrinth(&mr_version(vec![file]), "project-1", "only.jar", "", "").is_err()
        );
    }

    #[test]
    fn modrinth_archive_uses_primary_file_and_strongest_hash() {
        let extra = mr_file(
            "extra.jar",
            "https://cdn.modrinth.com/data/extra.jar",
            false,
            &[("sha1", SHA1_B)],
        );
        let primary = mr_file(
            "pack.mrpack",
            "https://cdn.modrinth.com/data/pack.mrpack",
            true,
            &[("sha1", SHA1_A), ("sha512", SHA512_A)],
        );
        let resolved = resolve_modrinth_archive(&[extra, primary]).unwrap();

        assert_eq!(resolved.file_name, "pack.mrpack");
        assert_eq!(resolved.url, "https://cdn.modrinth.com/data/pack.mrpack");
        assert_eq!(resolved.hash.algorithm(), "sha512");
        assert_eq!(resolved.hash.hex(), SHA512_A);
        assert_eq!(resolved.size, Some(123));
    }

    #[test]
    fn modrinth_archive_uses_the_only_file_even_without_the_primary_flag() {
        let only = mr_file(
            "pack.mrpack",
            "https://cdn.modrinth.com/data/pack.mrpack",
            false,
            &[("sha1", SHA1_A)],
        );
        let resolved = resolve_modrinth_archive(&[only]).unwrap();
        assert_eq!(resolved.file_name, "pack.mrpack");
    }

    #[test]
    fn modrinth_archive_rejects_multiple_files_without_a_single_primary() {
        let first = mr_file(
            "first.mrpack",
            "https://cdn.modrinth.com/data/first.mrpack",
            false,
            &[("sha1", SHA1_A)],
        );
        let second = mr_file(
            "second.mrpack",
            "https://cdn.modrinth.com/data/second.mrpack",
            false,
            &[("sha1", SHA1_B)],
        );
        let error = resolve_modrinth_archive(&[first, second]).unwrap_err();
        assert!(error.to_string().contains("no primary"), "got: {error}");

        let primary_a = mr_file(
            "a.mrpack",
            "https://cdn.modrinth.com/data/a.mrpack",
            true,
            &[("sha1", SHA1_A)],
        );
        let primary_b = mr_file(
            "b.mrpack",
            "https://cdn.modrinth.com/data/b.mrpack",
            true,
            &[("sha1", SHA1_B)],
        );
        let error = resolve_modrinth_archive(&[primary_a, primary_b]).unwrap_err();
        assert!(error.to_string().contains("ambiguous"), "got: {error}");
    }

    #[test]
    fn modrinth_archive_rejects_missing_invalid_or_unsafe_metadata() {
        let no_hash = mr_file(
            "pack.mrpack",
            "https://cdn.modrinth.com/data/pack.mrpack",
            true,
            &[],
        );
        assert!(resolve_modrinth_archive(&[no_hash]).is_err());

        let bad_hash = mr_file(
            "pack.mrpack",
            "https://cdn.modrinth.com/data/pack.mrpack",
            true,
            &[("sha512", "not-a-hash")],
        );
        assert!(resolve_modrinth_archive(&[bad_hash]).is_err());

        let unsafe_url = mr_file(
            "pack.mrpack",
            "https://attacker.example/pack.mrpack",
            true,
            &[("sha1", SHA1_A)],
        );
        assert!(resolve_modrinth_archive(&[unsafe_url]).is_err());
        assert!(resolve_modrinth_archive(&[]).is_err());
    }

    #[test]
    fn curseforge_resolves_the_exact_file_and_algo_one() {
        let file = cf_file(42, 7, "mod.jar", vec![sha1_hash(SHA1_A)]);
        let resolved = resolve_curseforge(&[file], 7, 42, SHA1_A).unwrap();

        assert_eq!(resolved.file_name, "mod.jar");
        assert_eq!(resolved.file_id.as_deref(), Some("42"));
        assert_eq!(resolved.hash.algorithm(), "sha1");
        assert_eq!(resolved.hash.hex(), SHA1_A);
        assert_eq!(resolved.version_id, "cf:7:42");
    }

    #[test]
    fn curseforge_rejects_wrong_file_and_project_ids() {
        let file = cf_file(42, 7, "mod.jar", vec![sha1_hash(SHA1_A)]);
        assert!(resolve_curseforge(std::slice::from_ref(&file), 7, 43, "").is_err());
        assert!(resolve_curseforge(&[file], 8, 42, "").is_err());
    }

    #[test]
    fn curseforge_rejects_unavailable_or_unnamed_files() {
        let mut unavailable = cf_file(42, 7, "mod.jar", vec![sha1_hash(SHA1_A)]);
        unavailable.is_available = Some(false);
        assert!(resolve_curseforge(&[unavailable], 7, 42, "").is_err());

        let unnamed = cf_file(42, 7, "", vec![sha1_hash(SHA1_A)]);
        assert!(resolve_curseforge(&[unnamed], 7, 42, "").is_err());
    }

    #[test]
    fn curseforge_rejects_duplicate_metadata_for_the_same_file() {
        let first = cf_file(42, 7, "mod.jar", vec![sha1_hash(SHA1_A)]);
        let second = cf_file(42, 7, "mod.jar", vec![sha1_hash(SHA1_B)]);
        assert!(resolve_curseforge(&[first, second], 7, 42, "").is_err());
    }

    #[test]
    fn modrinth_duplicate_filenames_need_one_matching_url() {
        let first = mr_file(
            "same.jar",
            "https://cdn.modrinth.com/data/same.jar",
            true,
            &[("sha1", SHA1_A)],
        );
        let second = mr_file(
            "same.jar",
            "https://cdn.modrinth.com/data/same.jar",
            false,
            &[("sha1", SHA1_B)],
        );
        let err = resolve_modrinth(
            &mr_version(vec![first, second]),
            "project-1",
            "same.jar",
            "https://cdn.modrinth.com/data/same.jar",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("ambiguous"), "got: {err}");
    }

    #[test]
    fn curseforge_ignores_unknown_algorithms_and_requires_sha1() {
        let file = cf_file(
            42,
            7,
            "mod.jar",
            vec![CfHash {
                value: "cccccccccccccccccccccccccccccccc".to_string(),
                algo: 2,
            }],
        );
        assert!(resolve_curseforge(&[file], 7, 42, "").is_err());
    }

    #[test]
    fn curseforge_rejects_malformed_or_mismatched_frontend_sha1() {
        let file = cf_file(42, 7, "mod.jar", vec![sha1_hash(SHA1_A)]);
        assert!(resolve_curseforge(std::slice::from_ref(&file), 7, 42, "not-a-hash").is_err());
        assert!(resolve_curseforge(&[file], 7, 42, SHA1_B).is_err());
    }

    #[test]
    fn missing_curseforge_api_key_is_rejected_before_network() {
        assert!(ensure_api_key("").is_err());
        assert!(ensure_api_key("   ").is_err());
        assert!(ensure_api_key("key").is_ok());
    }
}

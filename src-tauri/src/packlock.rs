//! Modpack lock (roadmap items 25-29): records which pack file an instance
//! was installed from, so the launcher can check for updates, update
//! transactionally (stage everything → apply → commit the lock) and repair
//! missing content against the manifest without ever touching what the user
//! has. Supports CurseForge packs (`manifest.json`) and Modrinth packs
//! (`modrinth.index.json`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Lock file name inside the instance directory (above `.minecraft` — the
/// instance root is the launcher's own territory).
pub const LOCK_FILE: &str = "pack-lock.json";

/// Name used before the lock became provider-generic (stage 4). Still read
/// so instances tracked by earlier builds keep their update/repair menu.
pub const LEGACY_LOCK_FILE: &str = "curseforge-pack.json";

/// Everything needed to re-derive the installed pack state later. Written
/// only after a fully successful install/update (commit point).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackLock {
    /// `"curseforge"` / `"modrinth"`.
    pub provider: String,
    /// Pack project id (CF numeric id / Modrinth project id).
    pub pack_project_id: String,
    pub pack_name: String,
    /// Version identity: CF file id / Modrinth version id.
    pub file_id: String,
    /// Actual archive file name of that version (e.g. `MyPack-1.4.2.zip`).
    pub file_name: String,
    /// Display name of that version (what the UI shows as a version).
    pub file_display_name: String,
    pub installed_at: String,
    pub mc_version: String,
    /// Lowercase loader slug (`fabric`/`forge`/`neoforge`), `""` = vanilla.
    pub loader: String,
    /// Parsed pack manifest (CF `manifest.json` / `modrinth.index.json`).
    pub manifest: serde_json::Value,
    /// All override paths (relative to `.minecraft`) present in the pack
    /// zip at install time — the basis for identifying removed overrides.
    pub overrides: Vec<String>,
}

impl PackLock {
    pub fn lock_path(instance_dir: &Path) -> PathBuf {
        instance_dir.join(LOCK_FILE)
    }

    pub fn legacy_lock_path(instance_dir: &Path) -> PathBuf {
        instance_dir.join(LEGACY_LOCK_FILE)
    }

    pub fn read(instance_dir: &Path) -> Option<Self> {
        for path in [
            Self::lock_path(instance_dir),
            Self::legacy_lock_path(instance_dir),
        ] {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(lock) = serde_json::from_str(&text) {
                    return Some(lock);
                }
            }
        }
        None
    }

    /// Best-effort write: a failed lock write must not fail an otherwise
    /// successful install (the instance is usable, just untracked). Writing
    /// the new name also migrates away a leftover legacy-named file.
    ///
    /// The JSON is written to a sibling temp file and renamed into place, so
    /// a crash or power loss can never truncate an existing valid lock.
    pub fn write(&self, instance_dir: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        crate::instances::write_file_atomic(&Self::lock_path(instance_dir), &json)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let _ = std::fs::remove_file(Self::legacy_lock_path(instance_dir));
        Ok(())
    }

    /// Loader slug for pack version queries (`None` = no loader filter).
    pub fn loader_filter(&self) -> Option<&str> {
        if self.loader.is_empty() {
            None
        } else {
            Some(&self.loader)
        }
    }
}

/// One entry of a pack manifest, reduced to its identity pair (project +
/// version) — the unit the diff classifies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackFileRef {
    pub project_id: String,
    pub file_id: String,
}

impl PackFileRef {
    #[cfg(test)]
    pub fn parse_pair(pid: u64, fid: u64) -> Self {
        Self {
            project_id: pid.to_string(),
            file_id: fid.to_string(),
        }
    }
}

/// What changed between the installed manifest and the new one. `updated`
/// entries share an identity with the removed old file (the old jar is
/// found through its path/sidecar, not through the new manifest).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PackDiff {
    pub added: Vec<PackFileRef>,
    pub updated: Vec<PackFileRef>,
    pub removed: Vec<PackFileRef>,
    pub unchanged: Vec<PackFileRef>,
}

impl PackDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty()
    }
}

/// Diff two identity → version maps: added / updated (same identity, new
/// version) / removed / unchanged. Shared by both pack formats.
pub fn diff_keyed_maps(old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) -> PackDiff {
    let mut diff = PackDiff::default();
    for (key, new_ver) in new {
        match old.get(key) {
            None => diff.added.push(PackFileRef {
                project_id: key.clone(),
                file_id: new_ver.clone(),
            }),
            Some(old_ver) if old_ver != new_ver => diff.updated.push(PackFileRef {
                project_id: key.clone(),
                file_id: new_ver.clone(),
            }),
            Some(_) => diff.unchanged.push(PackFileRef {
                project_id: key.clone(),
                file_id: new_ver.clone(),
            }),
        }
    }
    for (key, old_ver) in old {
        if !new.contains_key(key) {
            diff.removed.push(PackFileRef {
                project_id: key.clone(),
                file_id: old_ver.clone(),
            });
        }
    }
    diff
}

/// Map a CF manifest `files` array to `project_id → file_id`. A project
/// appears at most once per manifest; later entries win (duplicates are
/// malformed anyway).
pub fn manifest_files(manifest: &serde_json::Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(files) = manifest["files"].as_array() else {
        return out;
    };
    for entry in files {
        let (Some(pid), Some(fid)) = (entry["projectID"].as_u64(), entry["fileID"].as_u64()) else {
            continue;
        };
        out.insert(pid.to_string(), fid.to_string());
    }
    out
}

/// Diff two CF manifests by project id.
pub fn diff_manifests(old: &serde_json::Value, new: &serde_json::Value) -> PackDiff {
    diff_keyed_maps(&manifest_files(old), &manifest_files(new))
}

/// Identity of one `modrinth.index.json` file entry: the pack project id,
/// or the path when a malformed entry carries no id (so renames are still
/// tracked instead of silently colliding on an empty key).
pub fn mrpack_entry_key(entry: &serde_json::Value) -> Option<String> {
    if let Some(pid) = entry["project_id"].as_str() {
        if !pid.is_empty() {
            return Some(pid.to_string());
        }
    }
    entry["path"].as_str().map(|p| format!("path:{}", p))
}

/// Version identity of an mrpack entry: `version_id`, falling back to the
/// first `versions` id and finally to the path (a path change must show up
/// as an update, never as an unnoticed no-op).
pub fn mrpack_entry_version(entry: &serde_json::Value) -> String {
    if let Some(v) = entry["version_id"].as_str() {
        if !v.is_empty() {
            return v.to_string();
        }
    }
    if let Some(v) = entry["versions"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
    {
        if !v.is_empty() {
            return v.to_string();
        }
    }
    entry["path"].as_str().unwrap_or_default().to_string()
}

/// Destination path of an mrpack entry, relative to `.minecraft`.
pub fn mrpack_entry_path(entry: &serde_json::Value) -> String {
    entry["path"].as_str().unwrap_or_default().to_string()
}

/// sha1 of an mrpack entry, when declared.
pub fn mrpack_entry_sha1(entry: &serde_json::Value) -> Option<String> {
    entry["hashes"]["sha1"].as_str().map(str::to_string)
}

/// Map an `modrinth.index.json` manifest to `identity → entry`, keeping only
/// the files a client install actually writes (server-only entries are
/// never on disk, so they must not participate in diffs or repairs).
pub fn mrpack_manifest_map(manifest: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    let Some(files) = manifest["files"].as_array() else {
        return out;
    };
    for entry in files {
        if !crate::import::mrpack_client_required(entry) {
            continue;
        }
        if let Some(key) = mrpack_entry_key(entry) {
            out.insert(key, entry.clone());
        }
    }
    out
}

/// Diff two mrpack manifests by project id (path fallback for malformed
/// entries).
pub fn diff_mrpack_manifests(old: &serde_json::Value, new: &serde_json::Value) -> PackDiff {
    let to_versions = |m: &serde_json::Value| -> BTreeMap<String, String> {
        mrpack_manifest_map(m)
            .iter()
            .map(|(k, e)| (k.clone(), mrpack_entry_version(e)))
            .collect()
    };
    diff_keyed_maps(&to_versions(old), &to_versions(new))
}

/// Content type recorded in a sidecar for a pack file path.
pub fn content_type_for_rel(rel: &str) -> &'static str {
    match rel.replace('\\', "/").split('/').next() {
        Some("resourcepacks") => "resourcepack",
        Some("shaderpacks") => "shader",
        _ => "mod",
    }
}

/// Read `manifest.json` and the full override path list (relative to
/// `.minecraft`) from an installed CF pack zip — called while the temp
/// download still exists, before import removes it.
pub fn pack_zip_info(zip_path: &Path) -> Result<(serde_json::Value, Vec<String>), String> {
    let file =
        std::fs::File::open(zip_path).map_err(|e| format!("Cannot open pack archive: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;

    let mut manifest_str = String::new();
    let mut overrides: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Cannot read pack entry: {}", e))?;
        let name = entry.name().to_string();
        if name == "manifest.json" {
            entry
                .read_to_string(&mut manifest_str)
                .map_err(|e| format!("Cannot read manifest.json: {}", e))?;
            continue;
        }
        if entry.is_dir() {
            continue;
        }
        if let Some(rel) = name.strip_prefix("overrides/") {
            if !rel.is_empty() {
                overrides.push(rel.to_string());
            }
        }
    }
    if manifest_str.is_empty() {
        return Err("Pack archive has no manifest.json".to_string());
    }
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_str).map_err(|e| format!("Invalid manifest.json: {}", e))?;
    overrides.sort();
    Ok((manifest, overrides))
}

/// Read `modrinth.index.json` and the full override path list (relative to
/// `.minecraft`, both `overrides/` and `overrides-client/`) from an mrpack
/// zip — the exact set the importer extracts.
pub fn mrpack_zip_info(zip_path: &Path) -> Result<(serde_json::Value, Vec<String>), String> {
    let file =
        std::fs::File::open(zip_path).map_err(|e| format!("Cannot open pack archive: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Cannot read pack archive: {}", e))?;

    let mut index_str = String::new();
    let mut overrides: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Cannot read pack entry: {}", e))?;
        let name = entry.name().to_string();
        if name == "modrinth.index.json" {
            entry
                .read_to_string(&mut index_str)
                .map_err(|e| format!("Cannot read modrinth.index.json: {}", e))?;
            continue;
        }
        if entry.is_dir() {
            continue;
        }
        if let Some(rel) = crate::import::extract_mrpack_override(&name) {
            if !rel.is_empty() {
                overrides.push(rel.to_string());
            }
        }
    }
    if index_str.is_empty() {
        return Err("Pack archive has no modrinth.index.json".to_string());
    }
    let index: serde_json::Value = serde_json::from_str(&index_str)
        .map_err(|e| format!("Invalid modrinth.index.json: {}", e))?;
    overrides.sort();
    overrides.dedup();
    Ok((index, overrides))
}

/// Paths an update/repair must never write or delete: the user's worlds,
/// personal media and core settings. Everything else in the instance is
/// pack-owned content the pack update may manage.
pub fn is_protected_path(rel: &str) -> bool {
    let norm = rel.replace('\\', "/");
    let lower = norm.trim_start_matches('/').to_ascii_lowercase();
    const DIRS: &[&str] = &["saves/", "screenshots/", "logs/", "crash-reports/"];
    if DIRS.iter().any(|d| lower.starts_with(d)) {
        return true;
    }
    matches!(
        lower.as_str(),
        "options.txt"
            | "optionsof.txt"
            | "servers.dat"
            | "usernamecache.json"
            | "usercache.json"
            | "eula.txt"
    )
}

/// Lowercase loader slug for lock storage and file queries.
pub fn loader_slug(loader: &crate::instances::LoaderType) -> &'static str {
    match loader {
        crate::instances::LoaderType::Fabric => "fabric",
        crate::instances::LoaderType::Forge => "forge",
        crate::instances::LoaderType::NeoForge => "neoforge",
        crate::instances::LoaderType::Vanilla => "",
    }
}

/// Loader slug declared by a CF pack manifest's primary `modLoaders` entry
/// (`"fabric-0.15.0"` → `fabric`), `""` when absent/vanilla.
pub fn manifest_loader_slug(manifest: &serde_json::Value) -> String {
    manifest["minecraft"]["modLoaders"]
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|v| v["primary"].as_bool().unwrap_or(false))
                .or_else(|| arr.first())
        })
        .and_then(|v| v["id"].as_str())
        .and_then(|id| id.split('-').next())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
        .to_string()
}

/// Is this override path backed by a sidecar (i.e. pack-managed content the
/// pack may remove), as opposed to a user config file? Only content folders
/// get sidecars — configs are never deleted by an update.
pub fn override_has_sidecar(instance_mc_dir: &Path, rel: &str) -> bool {
    let norm = rel.replace('\\', "/");
    let mut parts = norm.trim_start_matches('/').splitn(2, '/');
    let (Some(top), Some(rest)) = (parts.next(), parts.next()) else {
        return false;
    };
    if !matches!(top, "mods" | "resourcepacks" | "shaderpacks") || rest.contains('/') {
        return false;
    }
    crate::instances::read_sidecar_meta(&instance_mc_dir.join(top), rest).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn manifest(entries: &[(&str, &str)]) -> serde_json::Value {
        let files: Vec<serde_json::Value> = entries
            .iter()
            .map(|(pid, fid)| {
                serde_json::json!({ "projectID": pid.parse::<u64>().unwrap(), "fileID": fid.parse::<u64>().unwrap(), "required": true })
            })
            .collect();
        serde_json::json!({
            "minecraft": { "version": "1.20.1", "modLoaders": [{ "id": "forge-47.2.0", "primary": true }] },
            "files": files,
        })
    }

    fn mrpack(entries: &[serde_json::Value]) -> serde_json::Value {
        serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "2.0.0",
            "name": "Test Pack",
            "dependencies": { "minecraft": "1.20.1", "fabric-loader": "0.15.11" },
            "files": entries,
        })
    }

    fn mrpack_entry(pid: &str, vid: &str, path: &str) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "hashes": { "sha1": "abc" },
            "env": { "client": "required", "server": "unsupported" },
            "downloads": ["https://cdn.modrinth.com/data/x.jar"],
            "fileSize": 1,
            "project_id": pid,
            "version_id": vid,
        })
    }

    #[test]
    fn diff_classifies_added_updated_removed_unchanged() {
        let old = manifest(&[("1", "10"), ("2", "20"), ("3", "30")]);
        let new = manifest(&[("2", "20"), ("3", "31"), ("4", "40")]);
        let d = diff_manifests(&old, &new);
        assert_eq!(d.unchanged, vec![PackFileRef::parse_pair(2, 20)]);
        assert_eq!(d.updated, vec![PackFileRef::parse_pair(3, 31)]);
        assert_eq!(d.added, vec![PackFileRef::parse_pair(4, 40)]);
        assert_eq!(d.removed, vec![PackFileRef::parse_pair(1, 10)]);
    }

    #[test]
    fn diff_of_identical_manifests_is_empty() {
        let m = manifest(&[("1", "10"), ("2", "20")]);
        let d = diff_manifests(&m, &m);
        assert!(d.is_empty());
        assert_eq!(d.unchanged.len(), 2);
    }

    #[test]
    fn mrpack_diff_classifies_by_project_and_version() {
        let old = mrpack(&[
            mrpack_entry("p1", "v1", "mods/a.jar"),
            mrpack_entry("p2", "v2", "mods/b.jar"),
            mrpack_entry("p3", "v3", "mods/c.jar"),
        ]);
        let new = mrpack(&[
            mrpack_entry("p2", "v2", "mods/b.jar"),
            mrpack_entry("p3", "v4", "mods/c-new.jar"),
            mrpack_entry("p4", "v5", "mods/d.jar"),
        ]);
        let d = diff_mrpack_manifests(&old, &new);
        assert_eq!(
            d.unchanged,
            vec![PackFileRef {
                project_id: "p2".into(),
                file_id: "v2".into()
            }]
        );
        assert_eq!(
            d.updated,
            vec![PackFileRef {
                project_id: "p3".into(),
                file_id: "v4".into()
            }]
        );
        assert_eq!(
            d.added,
            vec![PackFileRef {
                project_id: "p4".into(),
                file_id: "v5".into()
            }]
        );
        assert_eq!(
            d.removed,
            vec![PackFileRef {
                project_id: "p1".into(),
                file_id: "v1".into()
            }]
        );
    }

    #[test]
    fn mrpack_diff_skips_server_only_files_and_keys_path_fallback() {
        let server_only = serde_json::json!({
            "path": "mods/server.jar",
            "env": { "client": "unsupported", "server": "required" },
            "project_id": "srv",
            "version_id": "v1",
        });
        let no_pid = serde_json::json!({
            "path": "mods/orphan.jar",
            "env": { "client": "required" },
            "project_id": "",
            "version_id": "v9",
        });
        let old = mrpack(&[mrpack_entry("p1", "v1", "mods/a.jar"), server_only.clone()]);
        let new = mrpack(&[mrpack_entry("p1", "v1", "mods/a.jar"), no_pid.clone()]);
        let d = diff_mrpack_manifests(&old, &new);
        // Server-only entry never counts as removed (it was never installed).
        assert!(d.removed.is_empty());
        // Path-keyed orphan shows up as added under its fallback key.
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].project_id, "path:mods/orphan.jar");
        assert_eq!(d.added[0].file_id, "v9");
    }

    #[test]
    fn protected_paths_cover_worlds_and_core_settings() {
        assert!(is_protected_path("saves/world/level.dat"));
        assert!(is_protected_path("Saves/New World/session.lock"));
        assert!(is_protected_path("screenshots/shot.png"));
        assert!(is_protected_path("logs/latest.log"));
        assert!(is_protected_path("crash-reports/crash.txt"));
        assert!(is_protected_path("options.txt"));
        assert!(is_protected_path("servers.dat"));
        // Pack-owned content stays manageable.
        assert!(!is_protected_path("config/forge.toml"));
        assert!(!is_protected_path("mods/some.jar"));
        assert!(!is_protected_path("resourcepacks/pack.zip"));
    }

    #[test]
    fn loader_slugs_round_trip() {
        assert_eq!(loader_slug(&crate::instances::LoaderType::Fabric), "fabric");
        assert_eq!(
            loader_slug(&crate::instances::LoaderType::NeoForge),
            "neoforge"
        );
        assert_eq!(loader_slug(&crate::instances::LoaderType::Vanilla), "");
        let m = manifest(&[]);
        assert_eq!(manifest_loader_slug(&m), "forge");
        let empty = serde_json::json!({ "minecraft": { "version": "1.20.1" } });
        assert_eq!(manifest_loader_slug(&empty), "");
    }

    #[test]
    fn content_type_follows_top_folder() {
        assert_eq!(content_type_for_rel("mods/a.jar"), "mod");
        assert_eq!(content_type_for_rel("resourcepacks/x.zip"), "resourcepack");
        assert_eq!(content_type_for_rel("shaderpacks/x.zip"), "shader");
        assert_eq!(content_type_for_rel("config/x.toml"), "mod");
    }

    #[test]
    fn pack_zip_info_reads_manifest_and_overrides() {
        let dir = std::env::temp_dir().join(format!("packlock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("pack.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(file);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            zw.start_file("manifest.json", opts).unwrap();
            let m = manifest(&[("5", "50")]).to_string();
            zw.write_all(m.as_bytes()).unwrap();
            zw.start_file("overrides/config/mod.toml", opts).unwrap();
            zw.write_all(b"cfg").unwrap();
            zw.start_file("overrides/options.txt", opts).unwrap();
            zw.write_all(b"fov:90").unwrap();
            zw.start_file("overrides/", opts).unwrap();
            zw.finish().unwrap();
        }
        let (m, overrides) = pack_zip_info(&zip_path).unwrap();
        assert_eq!(manifest_files(&m).get("5").map(String::as_str), Some("50"));
        assert_eq!(overrides, vec!["config/mod.toml", "options.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mrpack_zip_info_reads_index_and_both_override_roots() {
        let dir = std::env::temp_dir().join(format!("mrpack-info-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("pack.mrpack");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(file);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            zw.start_file("modrinth.index.json", opts).unwrap();
            let idx = mrpack(&[mrpack_entry("p1", "v1", "mods/a.jar")]).to_string();
            zw.write_all(idx.as_bytes()).unwrap();
            zw.start_file("overrides/config/a.toml", opts).unwrap();
            zw.write_all(b"a").unwrap();
            zw.start_file("overrides-client/config/client.toml", opts)
                .unwrap();
            zw.write_all(b"c").unwrap();
            zw.start_file("overrides-server/config/server.toml", opts)
                .unwrap();
            zw.write_all(b"s").unwrap();
            zw.start_file("overrides/", opts).unwrap();
            zw.finish().unwrap();
        }
        let (idx, overrides) = mrpack_zip_info(&zip_path).unwrap();
        assert_eq!(
            mrpack_manifest_map(&idx).get("p1").map(mrpack_entry_path),
            Some("mods/a.jar".to_string())
        );
        assert_eq!(
            overrides,
            vec![
                "config/a.toml".to_string(),
                "config/client.toml".to_string()
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_round_trips_through_disk_under_new_name() {
        let dir = std::env::temp_dir().join(format!("pack-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = PackLock {
            provider: "curseforge".into(),
            pack_project_id: "123".into(),
            pack_name: "Test Pack".into(),
            file_id: "456".into(),
            file_name: "TestPack-1.0.zip".into(),
            file_display_name: "TestPack 1.0".into(),
            installed_at: "2026-10-10T00:00:00Z".into(),
            mc_version: "1.20.1".into(),
            loader: "fabric".into(),
            manifest: manifest(&[]),
            overrides: vec!["config/a.toml".into()],
        };
        lock.write(&dir).unwrap();
        assert!(PackLock::lock_path(&dir).is_file());
        assert!(
            !dir.join(".pack-lock.json.tmp").exists(),
            "atomic write must not leave a temp file behind"
        );
        let back = PackLock::read(&dir).unwrap();
        assert_eq!(back, lock);
        assert_eq!(back.loader_filter(), Some("fabric"));
        assert!(PackLock::read(Path::new(&dir.join("nope"))).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_lock_is_read_and_migrated_on_write() {
        let dir =
            std::env::temp_dir().join(format!("pack-lock-legacy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = PackLock {
            provider: "curseforge".into(),
            pack_project_id: "1".into(),
            pack_name: "Legacy".into(),
            file_id: "2".into(),
            file_name: "l.zip".into(),
            file_display_name: "L".into(),
            installed_at: "2026-10-10T00:00:00Z".into(),
            mc_version: "1.20.1".into(),
            loader: String::new(),
            manifest: manifest(&[]),
            overrides: vec![],
        };
        std::fs::write(
            PackLock::legacy_lock_path(&dir),
            serde_json::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();
        // Readable under the old file name.
        let back = PackLock::read(&dir).unwrap();
        assert_eq!(back, lock);
        assert_eq!(back.loader_filter(), None);
        // First write migrates to the new name and drops the legacy file.
        back.write(&dir).unwrap();
        assert!(PackLock::lock_path(&dir).is_file());
        assert!(!PackLock::legacy_lock_path(&dir).exists());
        assert_eq!(PackLock::read(&dir).unwrap(), lock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

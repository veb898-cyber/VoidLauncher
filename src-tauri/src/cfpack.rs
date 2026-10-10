//! CurseForge pack lock: records which pack file an instance was installed
//! from, so the launcher can check for updates, update transactionally
//! (stage everything → apply → commit the lock) and repair missing content
//! against the manifest without ever touching what the user has.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Lock file name inside the instance directory (above `.minecraft` — the
/// instance root is the launcher's own territory).
pub const LOCK_FILE: &str = "curseforge-pack.json";

/// Everything needed to re-derive the installed pack state later. Written
/// only after a fully successful install/update (commit point).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CfPackLock {
    /// Always `"curseforge"` (room for other pack providers later).
    pub provider: String,
    /// CF project id of the pack itself.
    pub pack_project_id: String,
    pub pack_name: String,
    /// CF file id of the installed pack version.
    pub file_id: String,
    /// Actual archive file name of that file (e.g. `MyPack-1.4.2.zip`).
    pub file_name: String,
    /// Display name of that file (what the UI shows as a version).
    pub file_display_name: String,
    pub installed_at: String,
    pub mc_version: String,
    /// Lowercase loader slug (`fabric`/`forge`/`neoforge`), `""` = vanilla.
    pub loader: String,
    /// Parsed `manifest.json` of the installed pack zip.
    pub manifest: serde_json::Value,
    /// All override paths (relative to `.minecraft`) present in the pack
    /// zip at install time — the basis for identifying removed overrides.
    pub overrides: Vec<String>,
}

impl CfPackLock {
    pub fn lock_path(instance_dir: &Path) -> PathBuf {
        instance_dir.join(LOCK_FILE)
    }

    pub fn read(instance_dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(Self::lock_path(instance_dir)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Best-effort write: a failed lock write must not fail an otherwise
    /// successful install (the instance is usable, just untracked).
    pub fn write(&self, instance_dir: &Path) -> std::io::Result<()> {
        std::fs::write(Self::lock_path(instance_dir), serde_json::to_string_pretty(self)?)
    }

    /// Loader slug for CF file queries (`None` = no loader filter).
    pub fn loader_filter(&self) -> Option<&str> {
        if self.loader.is_empty() {
            None
        } else {
            Some(&self.loader)
        }
    }
}

/// One entry of the manifest `files` array (project + file id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
/// entries share a `project_id` with the removed old file (the old jar is
/// found through its sidecar, not through the manifest).
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

/// Map the manifest `files` array to `project_id → file_id`. A project
/// appears at most once per manifest; later entries win (duplicates are
/// malformed anyway).
pub fn manifest_files(manifest: &serde_json::Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(files) = manifest["files"].as_array() else {
        return out;
    };
    for entry in files {
        let (Some(pid), Some(fid)) = (entry["projectID"].as_u64(), entry["fileID"].as_u64())
        else {
            continue;
        };
        out.insert(pid.to_string(), fid.to_string());
    }
    out
}

/// Diff two manifests by project id: added / updated (same project, new
/// file id) / removed / unchanged.
pub fn diff_manifests(
    old: &serde_json::Value,
    new: &serde_json::Value,
) -> PackDiff {
    let old_map = manifest_files(old);
    let new_map = manifest_files(new);
    let mut diff = PackDiff::default();
    for (pid, new_fid) in &new_map {
        match old_map.get(pid) {
            None => diff.added.push(PackFileRef {
                project_id: pid.clone(),
                file_id: new_fid.clone(),
            }),
            Some(old_fid) if old_fid != new_fid => diff.updated.push(PackFileRef {
                project_id: pid.clone(),
                file_id: new_fid.clone(),
            }),
            Some(_) => diff.unchanged.push(PackFileRef {
                project_id: pid.clone(),
                file_id: new_fid.clone(),
            }),
        }
    }
    for (pid, old_fid) in &old_map {
        if !new_map.contains_key(pid) {
            diff.removed.push(PackFileRef {
                project_id: pid.clone(),
                file_id: old_fid.clone(),
            });
        }
    }
    diff
}

/// Read `manifest.json` and the full override path list (relative to
/// `.minecraft`) from an installed pack zip — called while the temp
/// download still exists, before import removes it.
pub fn pack_zip_info(zip_path: &Path) -> Result<(serde_json::Value, Vec<String>), String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| format!("Cannot open pack archive: {}", e))?;
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

/// Lowercase CF loader slug for lock storage and file queries.
pub fn loader_slug(loader: &crate::instances::LoaderType) -> &'static str {
    match loader {
        crate::instances::LoaderType::Fabric => "fabric",
        crate::instances::LoaderType::Forge => "forge",
        crate::instances::LoaderType::NeoForge => "neoforge",
        crate::instances::LoaderType::Vanilla => "",
    }
}

/// Loader slug declared by a pack manifest's primary `modLoaders` entry
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
        assert_eq!(loader_slug(&crate::instances::LoaderType::NeoForge), "neoforge");
        assert_eq!(loader_slug(&crate::instances::LoaderType::Vanilla), "");
        let m = manifest(&[]);
        assert_eq!(manifest_loader_slug(&m), "forge");
        let empty = serde_json::json!({ "minecraft": { "version": "1.20.1" } });
        assert_eq!(manifest_loader_slug(&empty), "");
    }

    #[test]
    fn pack_zip_info_reads_manifest_and_overrides() {
        let dir = std::env::temp_dir().join(format!("cfpack-test-{}", std::process::id()));
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
    fn lock_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("cfpack-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = CfPackLock {
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
        let back = CfPackLock::read(&dir).unwrap();
        assert_eq!(back, lock);
        assert_eq!(back.loader_filter(), Some("fabric"));
        assert!(CfPackLock::read(Path::new(&dir.join("nope"))).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

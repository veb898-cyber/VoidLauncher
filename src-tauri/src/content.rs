//! Unified installed-content model, dependency-graph resolution and
//! compatibility classification — stage 3 "Content architecture"
//! (roadmap items 13-16).
//!
//! [`InstalledContent`] normalizes `.index/` sidecars (and both historical
//! legacy layouts) into one record per installed file, so every subsystem
//! reads the same shape no matter how the file got on disk.
//!
//! [`build_resolution`] is a *pure* function over a pre-fetched dependency
//! graph: the network worklist lives in `commands::content`, while ordering,
//! conflict and skip logic live here where unit tests need no server.
//!
//! [`classify_compat`] turns Modrinth version queries into the four
//! normalized compatibility states instead of a bare boolean.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

// ==================== Installed-content model (item 13) ====================

/// Content type derived from the instance subfolder a file lives in.
pub fn content_type_for_dir(subfolder: &str) -> &'static str {
    match subfolder {
        "mods" => "mod",
        "resourcepacks" => "resourcepack",
        "shaderpacks" => "shader",
        _ => "unknown",
    }
}

/// Normalize a provider name recorded in a sidecar. Anything unknown
/// (historical sidecars sometimes leaked loader names in here) becomes
/// `"unknown"` so consumers only ever see the three real providers.
pub fn normalize_provider(raw: Option<&str>) -> String {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("modrinth") => "modrinth".to_string(),
        Some("curseforge") => "curseforge".to_string(),
        Some("local") => "local".to_string(),
        _ => "unknown".to_string(),
    }
}

/// One installed content file with its sidecar metadata normalized into a
/// single shape (roadmap item 13). Files without any sidecar (manual drops)
/// still produce a record with `provider: "unknown"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledContent {
    /// On-disk file name, including `.disabled` when disabled.
    pub filename: String,
    /// `mod` / `resourcepack` / `shader` — derived from the folder, the
    /// authoritative source, not from the sidecar.
    pub content_type: String,
    /// `modrinth` / `curseforge` / `local` / `unknown`.
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// Enabled state is read from the filename (`.disabled` suffix), which
    /// the game loader actually honors; the sidecar copy is informational.
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
}

impl InstalledContent {
    /// Read and normalize the sidecar for `filename`. Missing fields become
    /// `None`; a sidecar-less file still yields a usable record.
    pub fn read(content_dir: &Path, filename: &str, content_type: &str) -> Self {
        let meta = crate::instances::read_sidecar_meta(content_dir, filename);
        let field = |key: &str| {
            meta.as_ref()
                .and_then(|m| m[key].as_str())
                .map(str::to_string)
        };
        Self {
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            provider: normalize_provider(meta.as_ref().and_then(|m| m["provider"].as_str())),
            project_id: field("project_id"),
            version_id: field("version_id"),
            file_id: field("file_id"),
            project_name: field("project_name"),
            version_number: field("version_number"),
            // Pre-1.0 installers wrote the verified download hash as "sha1".
            hash: field("hash").or_else(|| field("sha1")),
            enabled: !filename.ends_with(".disabled"),
            installed_at: field("installed_at"),
        }
    }
}

/// The values a sidecar records when a file is (re)installed. This is a
/// *superset* of the historical five-field sidecar: old readers only look at
/// `provider` / `project_id` / `project_name` / `version_id` /
/// `version_number`, and JSON readers ignore unknown keys, so files written
/// by newer builds stay readable by older ones and vice versa.
pub struct SidecarWrite<'a> {
    pub provider: &'a str,
    pub content_type: &'a str,
    pub project_id: Option<&'a str>,
    pub project_name: Option<&'a str>,
    pub version_id: Option<&'a str>,
    pub version_number: Option<&'a str>,
    pub file_id: Option<&'a str>,
    pub hash: Option<&'a str>,
}

/// Write the unified sidecar into the `.index/` folder (Prism layout) and
/// drop any stale legacy-layout file. Callers treat failures as non-fatal:
/// the content file itself is already installed.
pub fn write_sidecar(
    content_dir: &Path,
    filename: &str,
    w: &SidecarWrite<'_>,
) -> std::io::Result<()> {
    let mut sidecar = serde_json::json!({
        "provider": w.provider,
        "content_type": w.content_type,
        "enabled": true,
        "installed_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(obj) = sidecar.as_object_mut() {
        for (key, value) in [
            ("project_id", w.project_id),
            ("project_name", w.project_name),
            ("version_id", w.version_id),
            ("version_number", w.version_number),
            ("file_id", w.file_id),
            ("hash", w.hash),
        ] {
            if let Some(v) = value {
                obj.insert(key.to_string(), serde_json::Value::String(v.to_string()));
            }
        }
    }
    let path = crate::instances::sidecar_meta_path(content_dir, filename);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, sidecar.to_string())?;
    let _ = std::fs::remove_file(crate::instances::legacy_sidecar_path(content_dir, filename));
    Ok(())
}

/// List every installed content file in `content_dir`, sidecar-less manual
/// drops included, sorted by filename for stable output.
pub fn list_installed(content_dir: &Path, content_type: &str) -> Vec<InstalledContent> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(content_dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        // Hidden entries (.index folder contents, OS junk) and launcher
        // metadata are not content.
        if name.starts_with('.') {
            continue;
        }
        if name.ends_with(".voidlauncher.json") || name.ends_with(".pw.toml") {
            continue;
        }
        if name.ends_with(crate::instances::STASH_SUFFIX) {
            continue;
        }
        let base = name.strip_suffix(".disabled").unwrap_or(&name);
        let matches_type = match content_type {
            "mod" => base.ends_with(".jar"),
            "resourcepack" | "shader" => base.ends_with(".zip"),
            _ => false,
        };
        if !matches_type {
            continue;
        }
        out.push(InstalledContent::read(content_dir, &name, content_type));
    }
    out.sort_by(|a, b| a.filename.cmp(&b.filename));
    out
}

// ==================== Dependency graph (items 14-15) ====================

pub const DEP_REQUIRED: &str = "required";
pub const DEP_OPTIONAL: &str = "optional";
pub const DEP_INCOMPATIBLE: &str = "incompatible";
pub const DEP_EMBEDDED: &str = "embedded";

pub const CONFLICT_VERSION_MISMATCH: &str = "version_mismatch";
pub const CONFLICT_INCOMPATIBLE: &str = "incompatible";

/// Normalize Modrinth's `dependency_type` (case-insensitive) to our kinds.
/// `embedded` dependencies ship inside the jar and must never be fetched;
/// unknown future types map to `optional` so they are never auto-installed.
pub fn normalize_dep_type(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "required" => DEP_REQUIRED,
        "incompatible" | "dontdepend" => DEP_INCOMPATIBLE,
        "embedded" => DEP_EMBEDDED,
        _ => DEP_OPTIONAL,
    }
}

/// The downloadable file of a planned version — everything the frontend
/// needs to call `cmd_download_to_folder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFile {
    pub url: String,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
}

/// One dependency edge of a fetched version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepEdge {
    /// Version the edge originates from (for conflict messages).
    pub source_version_id: String,
    /// Normalized [`DEP_*`] kind.
    pub dep_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Explicit pin when the API named a concrete version; `None` for
    /// project-only references (resolved against the graph, see
    /// [`build_resolution`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
}

/// A fetched Modrinth version, ready for graph resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionNode {
    pub version_id: String,
    pub project_id: String,
    /// Project display title (never the raw version name).
    pub name: String,
    pub version_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    pub file: PlanFile,
    #[serde(default)]
    pub deps: Vec<DepEdge>,
}

/// A required dependency phase 1 could not resolve to a version. Reported to
/// the user as a warning; it does not block installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedItem {
    pub project_id: String,
    pub project_name: String,
    pub source_project_id: String,
    pub source_name: String,
    /// Machine-readable reason: `no_compatible_version` / `unavailable`.
    pub reason: String,
}

/// A project already present on disk (per its sidecar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledProject {
    pub project_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
}

/// The pre-fetched graph handed to [`build_resolution`]: produced by the
/// network worklist in `commands::content`, consumed here purely.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphInput {
    /// Keyed by version id.
    pub nodes: HashMap<String, VersionNode>,
    /// Root version ids the user selected, in selection order.
    pub roots: Vec<String>,
    /// Failures recorded during fetching (deduplicated against the ones the
    /// resolver itself discovers).
    #[serde(default)]
    pub unresolved: Vec<UnresolvedItem>,
    /// Project title cache (`project_id` → title) covering projects without
    /// a node (optional/incompatible targets, unresolved reports).
    #[serde(default)]
    pub project_names: HashMap<String, String>,
}

/// One entry of the ordered install plan. Post-order: dependencies always
/// precede the versions that require them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub project_id: String,
    pub version_id: String,
    pub name: String,
    pub version_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    pub file: PlanFile,
    /// Distance from the nearest root (roots are 0).
    pub depth: usize,
    /// Display titles of the versions requiring this one (first is the
    /// primary parent in the UI).
    pub required_by: Vec<String>,
    pub is_root: bool,
}

/// Why a dependency blocks installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictReason {
    pub source_project_id: String,
    pub source_name: String,
    /// The other version's label for `version_mismatch`; empty for
    /// `incompatible`.
    pub requirement: String,
    /// [`CONFLICT_VERSION_MISMATCH`] / [`CONFLICT_INCOMPATIBLE`].
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub project_id: String,
    pub project_name: String,
    pub reasons: Vec<ConflictReason>,
}

/// Optional dependencies, surfaced for the UI but never auto-installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionalItem {
    pub project_id: String,
    pub name: String,
    pub source_name: String,
}

/// Required dependencies already on disk — skipped, not re-downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedItem {
    pub project_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Resolution {
    /// Post-order install plan (roots last).
    pub install: Vec<PlanItem>,
    /// Non-empty ⇒ installation must be blocked.
    pub conflicts: Vec<Conflict>,
    pub optional: Vec<OptionalItem>,
    pub skipped: Vec<SkippedItem>,
    /// Warnings only; installation may proceed.
    pub unresolved: Vec<UnresolvedItem>,
}

#[derive(Debug)]
struct Entered {
    version_id: String,
    depth: usize,
}

/// Resolve the graph into an install order with conflicts, skips and
/// warnings (roadmap items 14-15). Pure and deterministic for a given
/// input: no I/O, no clocks, stable output ordering.
///
/// Rules:
/// - Required edges plan their target before the requester (post-order);
///   each project is planned at most once, `required_by` lists every source.
/// - An explicit pin that disagrees with an already-planned version of the
///   same project is a `version_mismatch` conflict; project-only references
///   adopt whatever version is already planned.
/// - `incompatible` edges are checked *after* the whole traversal, so the
///   conflict is found regardless of visit order.
/// - Required targets already installed are skipped, not re-downloaded.
/// - Optional dependencies are reported but never planned; embedded ones
///   never reach the graph.
/// - Unresolved entries whose project ended up planned or installed are
///   dropped (a later edge resolved them).
pub fn build_resolution(input: &GraphInput, installed: &[InstalledProject]) -> Resolution {
    let mut walk = Walk::new(input, installed);
    for vid in &input.roots {
        walk.visit(vid, 0, "");
    }
    walk.finish_incompatible();
    walk.finish()
}

struct Walk<'a> {
    input: &'a GraphInput,
    installed: HashMap<String, &'a InstalledProject>,
    entered: HashMap<String, Entered>,
    sources: HashMap<String, Vec<String>>,
    order: Vec<String>,
    skipped: HashMap<String, SkippedItem>,
    optional: HashMap<String, OptionalItem>,
    unresolved: Vec<UnresolvedItem>,
    conflicts: HashMap<String, Conflict>,
    /// (target project id, source project id, source name) collected during
    /// traversal, evaluated once the final plan is known.
    deferred_incompatible: Vec<(String, String, String)>,
}

impl<'a> Walk<'a> {
    fn new(input: &'a GraphInput, installed: &'a [InstalledProject]) -> Self {
        Self {
            input,
            installed: installed
                .iter()
                .map(|p| (p.project_id.clone(), p))
                .collect(),
            entered: HashMap::new(),
            sources: HashMap::new(),
            order: Vec::new(),
            skipped: HashMap::new(),
            optional: HashMap::new(),
            unresolved: input.unresolved.clone(),
            conflicts: HashMap::new(),
            deferred_incompatible: Vec::new(),
        }
    }

    fn visit(&mut self, version_id: &str, depth: usize, source_name: &str) {
        let Some(node) = self.input.nodes.get(version_id).cloned() else {
            return;
        };
        if let Some(e) = self.entered.get(&node.project_id) {
            if e.version_id == version_id {
                self.push_source(&node.project_id, source_name);
            }
            return;
        }
        self.entered.insert(
            node.project_id.clone(),
            Entered {
                version_id: version_id.to_string(),
                depth,
            },
        );
        self.push_source(&node.project_id, source_name);
        for dep in node.deps.clone() {
            self.process_dep(&dep, &node);
        }
        self.order.push(node.project_id.clone());
    }

    fn process_dep(&mut self, dep: &DepEdge, source: &VersionNode) {
        match dep.dep_type.as_str() {
            DEP_REQUIRED => self.process_required(dep, source),
            DEP_OPTIONAL => {
                let Some(pid) = dep.project_id.as_deref() else {
                    return;
                };
                if self.optional.contains_key(pid) {
                    return;
                }
                let name = self.name_for(pid);
                self.optional.insert(
                    pid.to_string(),
                    OptionalItem {
                        project_id: pid.to_string(),
                        name,
                        source_name: source.name.clone(),
                    },
                );
            }
            _ => {
                // incompatible (embedded edges are dropped before the graph)
                let target = dep.project_id.clone().or_else(|| {
                    dep.version_id.as_deref().and_then(|vid| {
                        self.input.nodes.get(vid).map(|n| n.project_id.clone())
                    })
                });
                if let Some(pid) = target {
                    self.deferred_incompatible
                        .push((pid, source.project_id.clone(), source.name.clone()));
                }
            }
        }
    }

    fn process_required(&mut self, dep: &DepEdge, source: &VersionNode) {
        let Some(pid) = dep.project_id.as_deref() else {
            return;
        };
        // Self-dependency: nothing to plan.
        if pid == source.project_id {
            return;
        }
        if let Some(inst) = self.installed.get(pid) {
            self.skipped.entry(pid.to_string()).or_insert_with(|| SkippedItem {
                project_id: pid.to_string(),
                name: inst.name.clone(),
            });
            return;
        }
        match dep.version_id.as_deref() {
            Some(vid) => self.plan_target(pid, vid, source),
            None => {
                // Project-only reference: accept whatever version of the
                // project is already planned; otherwise adopt the project's
                // node from the graph, or report it unresolved.
                if self.entered.contains_key(pid) {
                    self.push_source(pid, &source.name);
                    return;
                }
                match self.node_for_pid(pid) {
                    Some(vid) => self.plan_target(pid, &vid, source),
                    None => self.add_unresolved(pid, source, "unavailable"),
                }
            }
        }
    }

    fn plan_target(&mut self, pid: &str, vid: &str, source: &VersionNode) {
        if let Some(e) = self.entered.get(pid) {
            if e.version_id == vid {
                self.push_source(pid, &source.name);
            } else {
                // Two explicit pins of the same project disagree.
                let requirement = self
                    .input
                    .nodes
                    .get(vid)
                    .map(|n| n.version_label.clone())
                    .unwrap_or_else(|| vid.to_string());
                let name = self.name_for(pid);
                self.add_conflict(
                    pid,
                    &name,
                    ConflictReason {
                        source_project_id: source.project_id.clone(),
                        source_name: source.name.clone(),
                        requirement,
                        kind: CONFLICT_VERSION_MISMATCH.to_string(),
                    },
                );
            }
            return;
        }
        if !self.input.nodes.contains_key(vid) {
            self.add_unresolved(pid, source, "unavailable");
            return;
        }
        let depth = self
            .entered
            .get(&source.project_id)
            .map(|e| e.depth + 1)
            .unwrap_or(1);
        self.visit(vid, depth, &source.name);
    }

    fn finish_incompatible(&mut self) {
        for (target_pid, source_pid, source_name) in
            std::mem::take(&mut self.deferred_incompatible)
        {
            let planned = self.entered.get(&target_pid);
            if planned.is_none() && !self.installed.contains_key(&target_pid) {
                continue;
            }
            let name = match planned {
                Some(e) => self
                    .input
                    .nodes
                    .get(&e.version_id)
                    .map(|n| n.name.clone())
                    .unwrap_or_else(|| self.name_for(&target_pid)),
                None => self.name_for(&target_pid),
            };
            self.add_conflict(
                &target_pid,
                &name,
                ConflictReason {
                    source_project_id: source_pid,
                    source_name,
                    requirement: String::new(),
                    kind: CONFLICT_INCOMPATIBLE.to_string(),
                },
            );
        }
    }

    fn finish(self) -> Resolution {
        let root_pids: HashSet<String> = self
            .input
            .roots
            .iter()
            .filter_map(|vid| self.input.nodes.get(vid))
            .map(|n| n.project_id.clone())
            .collect();

        let install = self
            .order
            .iter()
            .filter_map(|pid| {
                let e = self.entered.get(pid)?;
                let node = self.input.nodes.get(&e.version_id)?;
                Some(PlanItem {
                    project_id: pid.clone(),
                    version_id: e.version_id.clone(),
                    name: node.name.clone(),
                    version_label: node.version_label.clone(),
                    icon_url: node.icon_url.clone(),
                    file: node.file.clone(),
                    depth: e.depth,
                    required_by: self.sources.get(pid).cloned().unwrap_or_default(),
                    is_root: root_pids.contains(pid),
                })
            })
            .collect();

        // A warning about a project that ended up planned or installed is
        // stale: some later edge resolved it.
        let mut seen = HashSet::new();
        let unresolved = self
            .unresolved
            .into_iter()
            .filter(|u| {
                if self.entered.contains_key(&u.project_id)
                    || self.skipped.contains_key(&u.project_id)
                {
                    return false;
                }
                seen.insert((u.project_id.clone(), u.source_project_id.clone()))
            })
            .collect();

        let mut conflicts: Vec<Conflict> = self.conflicts.into_values().collect();
        conflicts.sort_by(|a, b| a.project_id.cmp(&b.project_id));
        let mut optional: Vec<OptionalItem> = self.optional.into_values().collect();
        optional.sort_by(|a, b| a.project_id.cmp(&b.project_id));
        let mut skipped: Vec<SkippedItem> = self.skipped.into_values().collect();
        skipped.sort_by(|a, b| a.project_id.cmp(&b.project_id));

        Resolution {
            install,
            conflicts,
            optional,
            skipped,
            unresolved,
        }
    }

    fn node_for_pid(&self, pid: &str) -> Option<String> {
        self.input
            .nodes
            .values()
            .filter(|n| n.project_id == pid)
            .map(|n| n.version_id.clone())
            .min()
    }

    fn name_for(&self, pid: &str) -> String {
        if let Some(e) = self.entered.get(pid) {
            if let Some(n) = self.input.nodes.get(&e.version_id) {
                return n.name.clone();
            }
        }
        if let Some(n) = self.input.project_names.get(pid) {
            return n.clone();
        }
        if let Some(n) = self
            .input
            .nodes
            .values()
            .filter(|n| n.project_id == pid)
            .map(|n| n.name.clone())
            .min()
        {
            return n;
        }
        if let Some(inst) = self.installed.get(pid) {
            return inst.name.clone();
        }
        pid.to_string()
    }

    fn push_source(&mut self, pid: &str, name: &str) {
        if name.is_empty() {
            return;
        }
        let entry = self.sources.entry(pid.to_string()).or_default();
        if !entry.iter().any(|s| s == name) {
            entry.push(name.to_string());
        }
    }

    fn add_unresolved(&mut self, pid: &str, source: &VersionNode, reason: &str) {
        let project_name = self.name_for(pid);
        self.unresolved.push(UnresolvedItem {
            project_id: pid.to_string(),
            project_name,
            source_project_id: source.project_id.clone(),
            source_name: source.name.clone(),
            reason: reason.to_string(),
        });
    }

    fn add_conflict(&mut self, pid: &str, name: &str, reason: ConflictReason) {
        let entry = self.conflicts.entry(pid.to_string()).or_insert_with(|| {
            Conflict {
                project_id: pid.to_string(),
                project_name: name.to_string(),
                reasons: Vec::new(),
            }
        });
        if entry.project_name == entry.project_id && name != pid {
            entry.project_name = name.to_string();
        }
        let dup = entry.reasons.iter().any(|r| {
            r.source_project_id == reason.source_project_id
                && r.requirement == reason.requirement
                && r.kind == reason.kind
        });
        if !dup {
            entry.reasons.push(reason);
        }
    }
}

// ==================== Compatibility states (item 16) ====================

pub const COMPAT_COMPATIBLE: &str = "compatible";
pub const COMPAT_UNVERIFIED: &str = "unverified";
pub const COMPAT_INCOMPATIBLE: &str = "incompatible";
pub const COMPAT_UNKNOWN: &str = "unknown";

/// Classify a project's compatibility from two version queries:
/// `strict` is the number of versions for (mc_version, loader), `loader_only`
/// the number for the loader alone (`None` when no fallback applies, e.g. a
/// Vanilla instance or a successful strict query).
///
/// - `strict > 0` → `compatible`: both constraints are proven.
/// - `strict == 0`, `loader_only > 0` → `unverified`: the loader is satisfied
///   but no build targets this exact Minecraft version yet. The old frontend
///   silently treated this as compatible — that was the dangerous heuristic
///   this function exists to replace.
/// - `strict == 0`, no loader-only matches → `incompatible`.
/// - `has_metadata == false` → `unknown` (no provider identity to query).
pub fn classify_compat(has_metadata: bool, strict: usize, loader_only: Option<usize>) -> &'static str {
    if !has_metadata {
        return COMPAT_UNKNOWN;
    }
    if strict > 0 {
        return COMPAT_COMPATIBLE;
    }
    match loader_only {
        Some(n) if n > 0 => COMPAT_UNVERIFIED,
        _ => COMPAT_INCOMPATIBLE,
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voidlauncher_content_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn full_write<'a>() -> SidecarWrite<'a> {
        SidecarWrite {
            provider: "modrinth",
            content_type: "mod",
            project_id: Some("abc"),
            project_name: Some("Sodium"),
            version_id: Some("v1"),
            version_number: Some("0.6.0"),
            file_id: Some("f42"),
            hash: Some("deadbeef"),
        }
    }

    #[test]
    fn write_sidecar_roundtrips_all_unified_fields() {
        let dir = temp_dir("roundtrip");
        let mods = dir.join("mods");
        std::fs::create_dir_all(&mods).unwrap();

        write_sidecar(&mods, "sodium.jar", &full_write()).unwrap();
        let got = InstalledContent::read(&mods, "sodium.jar", "mod");

        assert_eq!(got.filename, "sodium.jar");
        assert_eq!(got.content_type, "mod");
        assert_eq!(got.provider, "modrinth");
        assert_eq!(got.project_id.as_deref(), Some("abc"));
        assert_eq!(got.project_name.as_deref(), Some("Sodium"));
        assert_eq!(got.version_id.as_deref(), Some("v1"));
        assert_eq!(got.version_number.as_deref(), Some("0.6.0"));
        assert_eq!(got.file_id.as_deref(), Some("f42"));
        assert_eq!(got.hash.as_deref(), Some("deadbeef"));
        assert!(got.enabled);
        assert!(got.installed_at.is_some());
        // Legacy layout must have been cleaned up.
        assert!(!mods.join("sodium.voidlauncher.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_sidecars_and_disabled_files_normalize() {
        let dir = temp_dir("legacy");
        let mods = dir.join("resourcepacks");
        std::fs::create_dir_all(mods.join(".index")).unwrap();

        // Historical five-field sidecar (no content_type/enabled/hash).
        std::fs::write(
            mods.join(".index").join("faithful.voidlauncher.json"),
            r#"{"provider":"local"}"#,
        )
        .unwrap();
        let got = InstalledContent::read(&mods, "faithful.zip", "resourcepack");
        assert_eq!(got.provider, "local");
        assert_eq!(got.content_type, "resourcepack");
        assert_eq!(got.project_id, None);
        assert_eq!(got.hash, None);
        assert!(got.enabled);

        // Junk provider (a loader name leaked in historically) → unknown.
        std::fs::write(
            mods.join(".index").join("old.voidlauncher.json"),
            r#"{"provider":"Fabric","project_id":"p1","sha1":"abc"}"#,
        )
        .unwrap();
        let got = InstalledContent::read(&mods, "old.zip", "resourcepack");
        assert_eq!(got.provider, "unknown");
        assert_eq!(got.project_id.as_deref(), Some("p1"));
        assert_eq!(got.hash.as_deref(), Some("abc"), "legacy sha1 key maps to hash");

        // Disabled state comes from the filename, not the sidecar.
        std::fs::write(mods.join(".index").join("off.voidlauncher.json"), "{}").unwrap();
        let got = InstalledContent::read(&mods, "off.zip.disabled", "resourcepack");
        assert!(!got.enabled);

        // No sidecar at all: still a usable record.
        std::fs::write(mods.join("manual.zip"), b"zip").unwrap();
        let got = InstalledContent::read(&mods, "manual.zip", "resourcepack");
        assert_eq!(got.provider, "unknown");
        assert_eq!(got.project_id, None);
        assert!(got.enabled);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_installed_filters_junk_and_sorts() {
        let dir = temp_dir("list");
        let mods = dir.join("mods");
        std::fs::create_dir_all(mods.join(".index")).unwrap();
        for f in [
            "b.jar",
            "a.jar",
            "a.jar.disabled",
            ".hidden.jar",
            "notes.txt",
            "pack.zip",
            "old.jar.voidlauncher-old",
        ] {
            std::fs::write(mods.join(f), b"x").unwrap();
        }
        std::fs::write(mods.join(".index").join("a.voidlauncher.json"), "{}").unwrap();

        let got = list_installed(&mods, "mod");
        let names: Vec<&str> = got.iter().map(|c| c.filename.as_str()).collect();
        assert_eq!(names, vec!["a.jar", "a.jar.disabled", "b.jar"]);
        assert!(got.iter().all(|c| c.content_type == "mod"));

        // Missing folder → empty, not a panic.
        assert!(list_installed(&dir.join("nope"), "mod").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Graph helpers ----

    fn node(vid: &str, pid: &str, name: &str, label: &str, deps: Vec<DepEdge>) -> VersionNode {
        VersionNode {
            version_id: vid.to_string(),
            project_id: pid.to_string(),
            name: name.to_string(),
            version_label: label.to_string(),
            icon_url: None,
            file: PlanFile {
                url: format!("https://cdn.example/{}.jar", vid),
                filename: format!("{}.jar", vid),
                sha1: Some(format!("sha-{}", vid)),
            },
            deps,
        }
    }

    fn req(source: &str, pid: &str, vid: Option<&str>) -> DepEdge {
        DepEdge {
            source_version_id: source.to_string(),
            dep_type: DEP_REQUIRED.to_string(),
            project_id: Some(pid.to_string()),
            version_id: vid.map(String::from),
        }
    }

    fn edge_of_kind(source: &str, kind: &str, pid: &str) -> DepEdge {
        DepEdge {
            source_version_id: source.to_string(),
            dep_type: kind.to_string(),
            project_id: Some(pid.to_string()),
            version_id: None,
        }
    }

    fn graph(nodes: Vec<VersionNode>, roots: &[&str]) -> GraphInput {
        GraphInput {
            nodes: nodes.into_iter().map(|n| (n.version_id.clone(), n)).collect(),
            roots: roots.iter().map(|s| s.to_string()).collect(),
            unresolved: Vec::new(),
            project_names: HashMap::new(),
        }
    }

    fn installed(pid: &str, name: &str) -> InstalledProject {
        InstalledProject {
            project_id: pid.to_string(),
            name: name.to_string(),
            version_id: None,
        }
    }

    fn plan_ids(res: &Resolution) -> Vec<&str> {
        res.install.iter().map(|p| p.project_id.as_str()).collect()
    }

    #[test]
    fn chain_orders_dependencies_before_parents() {
        // A → B → C: post-order must install C, then B, then A.
        let g = graph(
            vec![
                node("va", "a", "Alpha", "1.0.0", vec![req("va", "b", Some("vb"))]),
                node("vb", "b", "Beta", "2.0.0", vec![req("vb", "c", Some("vc"))]),
                node("vc", "c", "Gamma", "3.0.0", vec![]),
            ],
            &["va"],
        );
        let res = build_resolution(&g, &[]);
        assert_eq!(plan_ids(&res), vec!["c", "b", "a"]);
        assert!(res.conflicts.is_empty());
        assert!(res.unresolved.is_empty());

        let c = &res.install[0];
        assert_eq!(c.depth, 2, "C is two hops from the root");
        assert_eq!(c.required_by, vec!["Beta"]);
        let b = &res.install[1];
        assert_eq!(b.required_by, vec!["Alpha"]);
        let a = &res.install[2];
        assert!(a.is_root && !b.is_root && !c.is_root);
        assert_eq!(a.file.sha1.as_deref(), Some("sha-va"));
        assert_eq!(a.file.filename, "va.jar");
    }

    #[test]
    fn diamond_plans_shared_dependency_once() {
        // A → {B, C}; B → D; C → D. D appears once, required by both.
        let g = graph(
            vec![
                node(
                    "va",
                    "a",
                    "Alpha",
                    "1.0",
                    vec![req("va", "b", Some("vb")), req("va", "c", Some("vc"))],
                ),
                node("vb", "b", "Beta", "1.0", vec![req("vb", "d", Some("vd"))]),
                node("vc", "c", "Gamma", "1.0", vec![req("vc", "d", Some("vd"))]),
                node("vd", "d", "Delta", "1.0", vec![]),
            ],
            &["va"],
        );
        let res = build_resolution(&g, &[]);
        assert_eq!(plan_ids(&res), vec!["d", "b", "c", "a"]);
        let d = &res.install[0];
        assert_eq!(d.required_by, vec!["Beta", "Gamma"]);
        assert!(res.conflicts.is_empty());
    }

    #[test]
    fn cycle_terminates_and_plans_each_project_once() {
        let g = graph(
            vec![
                node("va", "a", "Alpha", "1.0", vec![req("va", "b", Some("vb"))]),
                node("vb", "b", "Beta", "1.0", vec![req("vb", "a", Some("va"))]),
            ],
            &["va"],
        );
        let res = build_resolution(&g, &[]);
        assert_eq!(plan_ids(&res), vec!["b", "a"]);
        assert!(res.conflicts.is_empty());
    }

    #[test]
    fn disagreeing_pins_conflict_and_plan_one_version() {
        // A pins C@1, B pins C@2: one install entry, one conflict.
        let g = graph(
            vec![
                node("va", "a", "Alpha", "1.0", vec![req("va", "c", Some("c1"))]),
                node("vb", "b", "Beta", "1.0", vec![req("vb", "c", Some("c2"))]),
                node("c1", "c", "Cache", "1.0", vec![]),
                node("c2", "c", "Cache", "2.0", vec![]),
            ],
            &["va", "vb"],
        );
        let res = build_resolution(&g, &[]);
        assert_eq!(plan_ids(&res), vec!["c", "a", "b"]);

        assert_eq!(res.conflicts.len(), 1);
        let conflict = &res.conflicts[0];
        assert_eq!(conflict.project_id, "c");
        assert_eq!(conflict.project_name, "Cache");
        assert_eq!(conflict.reasons.len(), 1);
        let reason = &conflict.reasons[0];
        assert_eq!(reason.kind, CONFLICT_VERSION_MISMATCH);
        assert_eq!(reason.source_project_id, "b");
        assert_eq!(reason.source_name, "Beta");
        assert_eq!(reason.requirement, "2.0");
    }

    #[test]
    fn project_only_reference_adopts_planned_version_without_conflict() {
        // A pins C@1, B just needs "some C": no conflict, single plan entry.
        let g = graph(
            vec![
                node("va", "a", "Alpha", "1.0", vec![req("va", "c", Some("c1"))]),
                node("vb", "b", "Beta", "1.0", vec![req("vb", "c", None)]),
                node("c1", "c", "Cache", "1.0", vec![]),
            ],
            &["va", "vb"],
        );
        let res = build_resolution(&g, &[]);
        assert_eq!(plan_ids(&res), vec!["c", "a", "b"]);
        assert!(res.conflicts.is_empty());
        assert_eq!(res.install[0].required_by, vec!["Alpha", "Beta"]);
    }

    #[test]
    fn incompatible_edge_conflicts_regardless_of_visit_order() {
        // Roots ordered [A, B]: B's required C is planned only *after* A's
        // incompatible edge was collected — the deferred check must still fire.
        let g = graph(
            vec![
                node(
                    "va",
                    "a",
                    "Alpha",
                    "1.0",
                    vec![edge_of_kind("va", DEP_INCOMPATIBLE, "c")],
                ),
                node("vb", "b", "Beta", "1.0", vec![req("vb", "c", Some("c1"))]),
                node("c1", "c", "Cache", "1.0", vec![]),
            ],
            &["va", "vb"],
        );
        let res = build_resolution(&g, &[]);
        assert!(plan_ids(&res).contains(&"c"), "C is still planned");
        assert_eq!(res.conflicts.len(), 1);
        assert_eq!(res.conflicts[0].project_id, "c");
        let reason = &res.conflicts[0].reasons[0];
        assert_eq!(reason.kind, CONFLICT_INCOMPATIBLE);
        assert_eq!(reason.source_project_id, "a");
        assert_eq!(reason.source_name, "Alpha");
    }

    #[test]
    fn incompatible_against_installed_project_conflicts() {
        let g = graph(
            vec![node(
                "va",
                "a",
                "Alpha",
                "1.0",
                vec![edge_of_kind("va", DEP_INCOMPATIBLE, "c")],
            )],
            &["va"],
        );
        let res = build_resolution(&g, &[installed("c", "Cache")]);
        assert_eq!(res.conflicts.len(), 1);
        assert_eq!(res.conflicts[0].project_name, "Cache");
    }

    #[test]
    fn installed_required_dependency_is_skipped_not_planned() {
        let g = graph(
            vec![node(
                "va",
                "a",
                "Alpha",
                "1.0",
                vec![req("va", "b", Some("vb"))],
            )],
            &["va"],
        );
        let res = build_resolution(&g, &[installed("b", "Beta")]);
        assert_eq!(plan_ids(&res), vec!["a"]);
        assert_eq!(res.skipped.len(), 1);
        assert_eq!(res.skipped[0].project_id, "b");
        assert_eq!(res.skipped[0].name, "Beta");
        assert!(res.unresolved.is_empty());
    }

    #[test]
    fn optional_dependencies_are_reported_but_never_planned() {
        let mut g = graph(
            vec![
                node(
                    "va",
                    "a",
                    "Alpha",
                    "1.0",
                    vec![
                        edge_of_kind("va", DEP_OPTIONAL, "x"),
                        req("va", "b", Some("vb")),
                    ],
                ),
                node("vb", "b", "Beta", "1.0", vec![]),
            ],
            &["va"],
        );
        g.project_names.insert("x".to_string(), "X-Ray".to_string());
        let res = build_resolution(&g, &[]);
        assert_eq!(plan_ids(&res), vec!["b", "a"]);
        assert_eq!(res.optional.len(), 1);
        assert_eq!(res.optional[0].project_id, "x");
        assert_eq!(res.optional[0].name, "X-Ray");
        assert_eq!(res.optional[0].source_name, "Alpha");
    }

    #[test]
    fn unresolved_entries_pass_through_deduped_and_stale_ones_drop() {
        let stale = UnresolvedItem {
            project_id: "c".to_string(),
            project_name: "Cache".to_string(),
            source_project_id: "z".to_string(),
            source_name: "Zeta".to_string(),
            reason: "no_compatible_version".to_string(),
        };
        let keep = UnresolvedItem {
            project_id: "y".to_string(),
            project_name: "Why".to_string(),
            source_project_id: "a".to_string(),
            source_name: "Alpha".to_string(),
            reason: "no_compatible_version".to_string(),
        };
        let mut g = graph(
            vec![node(
                "va",
                "a",
                "Alpha",
                "1.0",
                vec![req("va", "c", Some("c1"))],
            )],
            &["va"],
        );
        g.unresolved = vec![stale.clone(), stale.clone(), keep.clone()];

        // C is resolvable now: its stale warning must disappear, the
        // unrelated warning survives, and the duplicate collapses.
        g.nodes.insert(
            "c1".to_string(),
            node("c1", "c", "Cache", "1.0", vec![]),
        );
        let res = build_resolution(&g, &[]);
        assert_eq!(res.unresolved.len(), 1);
        assert_eq!(res.unresolved[0].project_id, "y");
    }

    #[test]
    fn missing_dependency_node_reports_unresolved() {
        // Edge points at a version that was never fetched (phase-1 failure).
        let g = graph(
            vec![node(
                "va",
                "a",
                "Alpha",
                "1.0",
                vec![req("va", "ghost", Some("vg"))],
            )],
            &["va"],
        );
        let res = build_resolution(&g, &[]);
        assert_eq!(plan_ids(&res), vec!["a"]);
        assert_eq!(res.unresolved.len(), 1);
        assert_eq!(res.unresolved[0].project_id, "ghost");
        assert_eq!(res.unresolved[0].source_name, "Alpha");
        assert_eq!(res.unresolved[0].reason, "unavailable");
    }

    #[test]
    fn dependency_type_normalization() {
        assert_eq!(normalize_dep_type("Required"), DEP_REQUIRED);
        assert_eq!(normalize_dep_type("optional"), DEP_OPTIONAL);
        assert_eq!(normalize_dep_type("Incompatible"), DEP_INCOMPATIBLE);
        assert_eq!(normalize_dep_type("dontdepend"), DEP_INCOMPATIBLE);
        assert_eq!(normalize_dep_type("embedded"), DEP_EMBEDDED);
        assert_eq!(normalize_dep_type("future-kind"), DEP_OPTIONAL);
    }

    #[test]
    fn classify_compat_produces_all_four_states() {
        assert_eq!(classify_compat(true, 12, None), COMPAT_COMPATIBLE);
        assert_eq!(classify_compat(true, 0, Some(7)), COMPAT_UNVERIFIED);
        assert_eq!(classify_compat(true, 0, Some(0)), COMPAT_INCOMPATIBLE);
        assert_eq!(classify_compat(true, 0, None), COMPAT_INCOMPATIBLE);
        assert_eq!(classify_compat(false, 9, Some(9)), COMPAT_UNKNOWN);
    }
}

//! Content-architecture commands (stage 3): the async dependency-graph
//! worklist that feeds the pure resolver in [`crate::content`], and batch
//! compatibility classification replacing the old per-mod frontend polling.

use crate::commands::instances::validate_instance_name;
use crate::content::{
    self, DepEdge, GraphInput, InstalledContent, InstalledProject, PlanFile, Resolution,
    UnresolvedItem, VersionNode, DEP_EMBEDDED, DEP_INCOMPATIBLE, DEP_OPTIONAL, DEP_REQUIRED,
};
use crate::instances;
use crate::modrinth;
use crate::AppState;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use tauri::State;

/// Content folders a content command may touch (mirrors the frontend maps).
const ALLOWED_SUBFOLDERS: &[&str] = &["mods", "resourcepacks", "shaderpacks"];

/// Resolve and validate the instance content directory for a command.
/// Returns the directory and the normalized subfolder name. The config lock
/// is dropped before returning, so callers may `.await` safely.
fn content_dir(state: &AppState, instance_name: &str, subfolder: &str) -> Result<(PathBuf, String), String> {
    validate_instance_name(instance_name)?;
    let sub = subfolder
        .trim_matches('/')
        .trim_matches('\\')
        .to_ascii_lowercase();
    if !ALLOWED_SUBFOLDERS.contains(&sub.as_str()) {
        return Err(format!("Subfolder '{}' is not allowed.", subfolder));
    }
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let instance = instances::get_instance(&config.instances_dir(), instance_name)
        .map_err(|e| e.to_string())?;
    Ok((instance.minecraft_dir(&config.instances_dir()).join(&sub), sub))
}

/// Modrinth ids are short base58 strings; reject anything else so a crafted
/// id can never be interpolated into an API URL.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ==================== Phase 1: the network worklist ====================

/// Fetches root versions, walks `required` dependency edges breadth-first and
/// collects everything [`content::build_resolution`] needs: nodes, project
/// titles and phase-1 unresolved reports. Fetch failures on dependencies are
/// NOT reported here — the resolver detects the missing node on its edge and
/// reports it with full source context (and richer phase-1 reasons win the
/// dedup because they are seeded first).
struct GraphFetcher {
    mc_version: Option<String>,
    loader: Option<String>,
    installed_pids: HashSet<String>,
    roots: HashSet<String>,
    nodes: HashMap<String, VersionNode>,
    /// `project_id` → (title, icon) for every project the graph mentions,
    /// including optional/incompatible targets that never get a node.
    projects: HashMap<String, (String, Option<String>)>,
    /// Project-only required edges still awaiting a version choice; filled in
    /// only when no node for the project exists (a pin elsewhere already
    /// resolved it otherwise).
    need_resolve: HashSet<String>,
    unresolved: Vec<UnresolvedItem>,
    attempted: HashSet<String>,
    queued: HashSet<String>,
}

impl GraphFetcher {
    fn new(
        root_version_ids: &[String],
        mc_version: Option<String>,
        loader: Option<String>,
        installed_pids: HashSet<String>,
    ) -> Self {
        Self {
            mc_version,
            loader,
            installed_pids,
            roots: root_version_ids.iter().cloned().collect(),
            nodes: HashMap::new(),
            projects: HashMap::new(),
            need_resolve: HashSet::new(),
            unresolved: Vec::new(),
            attempted: HashSet::new(),
            queued: HashSet::new(),
        }
    }

    async fn ensure_project(&mut self, pid: &str) {
        if self.projects.contains_key(pid) {
            return;
        }
        let meta = match modrinth::get_project(pid).await {
            Ok(p) => (p.title, p.icon_url),
            // A dead project lookup must not sink the whole resolution.
            Err(_) => (pid.to_string(), None),
        };
        self.projects.insert(pid.to_string(), meta);
    }

    fn project_name(&self, pid: &str) -> String {
        self.projects
            .get(pid)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| pid.to_string())
    }

    fn enqueue(&mut self, queue: &mut VecDeque<String>, vid: &str) {
        if self.queued.insert(vid.to_string()) {
            queue.push_back(vid.to_string());
        }
    }

    async fn run(mut self, root_version_ids: &[String]) -> Result<GraphInput, String> {
        let mut queue: VecDeque<String> = VecDeque::new();
        for vid in root_version_ids {
            self.enqueue(&mut queue, vid);
        }

        loop {
            while let Some(vid) = queue.pop_front() {
                if !self.attempted.insert(vid.clone()) {
                    continue;
                }
                let version = match modrinth::get_version_by_id(&vid).await {
                    Ok(v) => v,
                    Err(e) => {
                        // A root the user picked must resolve; dependency
                        // failures surface later, on their edges.
                        if self.roots.contains(&vid) {
                            return Err(format!("Failed to resolve version {}: {}", vid, e));
                        }
                        continue;
                    }
                };
                let file = version
                    .files
                    .iter()
                    .find(|f| f.primary)
                    .or_else(|| version.files.first())
                    .map(|f| PlanFile {
                        url: f.url.clone(),
                        filename: f.filename.clone(),
                        sha1: f.hashes.get("sha1").cloned(),
                    });
                let Some(file) = file else {
                    if self.roots.contains(&vid) {
                        return Err(format!("Version {} has no downloadable file", vid));
                    }
                    continue;
                };

                self.ensure_project(&version.project_id).await;
                let (name, icon) = self
                    .projects
                    .get(&version.project_id)
                    .cloned()
                    .unwrap_or_else(|| (version.project_id.clone(), None));

                let mut deps: Vec<DepEdge> = Vec::new();
                for d in &version.dependencies {
                    let kind = content::normalize_dep_type(&d.dependency_type);
                    if kind == DEP_EMBEDDED {
                        continue;
                    }
                    match kind {
                        DEP_REQUIRED => match (&d.version_id, &d.project_id) {
                            // Explicit pin: fetch that exact version.
                            (Some(dep_vid), pid) => {
                                deps.push(DepEdge {
                                    source_version_id: vid.clone(),
                                    dep_type: DEP_REQUIRED.to_string(),
                                    project_id: pid.clone(),
                                    version_id: Some(dep_vid.clone()),
                                });
                                self.enqueue(&mut queue, dep_vid);
                            }
                            // Project-only: defer the choice until every pin
                            // is fetched (order-independent).
                            (None, Some(dep_pid)) => {
                                deps.push(DepEdge {
                                    source_version_id: vid.clone(),
                                    dep_type: DEP_REQUIRED.to_string(),
                                    project_id: Some(dep_pid.clone()),
                                    version_id: None,
                                });
                                if !self.installed_pids.contains(dep_pid.as_str()) {
                                    self.need_resolve.insert(dep_pid.clone());
                                }
                            }
                            (None, None) => {}
                        },
                        DEP_OPTIONAL => {
                            if let Some(pid) = &d.project_id {
                                // Title so the UI can name the optional entry.
                                self.ensure_project(pid).await;
                            }
                            deps.push(DepEdge {
                                source_version_id: vid.clone(),
                                dep_type: DEP_OPTIONAL.to_string(),
                                project_id: d.project_id.clone(),
                                version_id: d.version_id.clone(),
                            });
                        }
                        _ => {
                            // incompatible: title for the conflict message.
                            if let Some(pid) = &d.project_id {
                                self.ensure_project(pid).await;
                            }
                            deps.push(DepEdge {
                                source_version_id: vid.clone(),
                                dep_type: DEP_INCOMPATIBLE.to_string(),
                                project_id: d.project_id.clone(),
                                version_id: d.version_id.clone(),
                            });
                        }
                    }
                }

                self.nodes.insert(
                    vid.clone(),
                    VersionNode {
                        version_id: vid,
                        project_id: version.project_id.clone(),
                        name,
                        version_label: version.version_number.clone(),
                        icon_url: icon,
                        file,
                        deps,
                    },
                );
            }

            // Queue drained: resolve deferred project-only references whose
            // project already has no node (pins are all known now).
            if self.need_resolve.is_empty() {
                break;
            }
            let pending: Vec<String> = std::mem::take(&mut self.need_resolve)
                .into_iter()
                .collect();
            for pid in pending {
                if self.installed_pids.contains(&pid)
                    || self.nodes.values().any(|n| n.project_id == pid)
                {
                    continue;
                }
                match modrinth::get_versions(
                    &pid,
                    self.mc_version.as_deref(),
                    self.loader.as_deref(),
                )
                .await
                {
                    Ok(vs) if !vs.is_empty() => {
                        let vid = vs[0].id.clone();
                        self.enqueue(&mut queue, &vid);
                    }
                    Ok(_) => {
                        // No version for this (MC, loader) — reported with a
                        // real project title for the warning panel.
                        self.ensure_project(&pid).await;
                        let source = self.project_source_for(&pid);
                        self.unresolved.push(UnresolvedItem {
                            project_id: pid.clone(),
                            project_name: self.project_name(&pid),
                            source_project_id: source.0,
                            source_name: source.1,
                            reason: "no_compatible_version".to_string(),
                        });
                    }
                    Err(_) => {
                        self.ensure_project(&pid).await;
                        let source = self.project_source_for(&pid);
                        self.unresolved.push(UnresolvedItem {
                            project_id: pid.clone(),
                            project_name: self.project_name(&pid),
                            source_project_id: source.0,
                            source_name: source.1,
                            reason: "unavailable".to_string(),
                        });
                    }
                }
            }
        }

        Ok(GraphInput {
            nodes: self.nodes,
            roots: root_version_ids.to_vec(),
            unresolved: self.unresolved,
            project_names: self
                .projects
                .into_iter()
                .map(|(pid, (title, _))| (pid, title))
                .collect(),
        })
    }

    /// Best-effort attribution for a deferred project-only failure: the first
    /// node that references the project (dedup keeps one warning anyway).
    fn project_source_for(&self, pid: &str) -> (String, String) {
        for node in self.nodes.values() {
            if node
                .deps
                .iter()
                .any(|d| d.project_id.as_deref() == Some(pid))
            {
                return (node.project_id.clone(), node.name.clone());
            }
        }
        (String::new(), String::new())
    }
}

// ==================== Commands ====================

/// Resolve the recursive required-dependency graph for the selected root
/// versions (roadmap item 14) and return the ordered install plan with
/// conflicts (item 15), optional/already-installed skips and unresolved
/// warnings. Phase 1 (network) runs here; ordering and conflict logic is the
/// pure `content::build_resolution`.
#[tauri::command]
pub async fn cmd_resolve_dependencies(
    state: State<'_, AppState>,
    instance_name: String,
    subfolder: String,
    root_version_ids: Vec<String>,
    mc_version: Option<String>,
    loader: Option<String>,
) -> Result<Resolution, String> {
    if root_version_ids.is_empty() {
        return Err("No content versions selected".to_string());
    }
    if root_version_ids.len() > 64 {
        return Err("Too many selected versions".to_string());
    }
    for id in &root_version_ids {
        if !valid_id(id) {
            return Err("Invalid version id".to_string());
        }
    }

    let (dir, sub) = content_dir(&state, &instance_name, &subfolder)?;
    let content_type = content::content_type_for_dir(&sub);

    let installed = content::list_installed(&dir, content_type);
    let installed_projects: Vec<InstalledProject> = installed
        .iter()
        .filter_map(|c| {
            Some(InstalledProject {
                project_id: c.project_id.clone()?,
                name: c
                    .project_name
                    .clone()
                    .unwrap_or_else(|| c.filename.clone()),
                version_id: c.version_id.clone(),
            })
        })
        .collect();
    let installed_pids = installed_projects
        .iter()
        .map(|p| p.project_id.clone())
        .collect();

    let fetcher = GraphFetcher::new(&root_version_ids, mc_version, loader, installed_pids);
    let graph = fetcher.run(&root_version_ids).await?;
    Ok(content::build_resolution(&graph, &installed_projects))
}

/// One file's compatibility status.
#[derive(Debug, serde::Serialize)]
pub struct CompatResult {
    pub filename: String,
    /// One of the [`content::COMPAT_*`] states.
    pub status: String,
}

/// Batch compatibility classification (roadmap item 16): one call replaces
/// the old per-mod 200 ms frontend polling loop. Only Modrinth-sourced files
/// with a project id are queryable — everything else is reported as
/// `unknown`. `filenames` restricts the batch to files not classified yet
/// (incremental refresh); `None` classifies the whole content folder.
#[tauri::command]
pub async fn cmd_check_content_compatibilities(
    state: State<'_, AppState>,
    instance_name: String,
    subfolder: String,
    mc_version: Option<String>,
    loader: Option<String>,
    filenames: Option<Vec<String>>,
) -> Result<Vec<CompatResult>, String> {
    let (dir, sub) = content_dir(&state, &instance_name, &subfolder)?;
    let content_type = content::content_type_for_dir(&sub);
    let wanted: Option<HashSet<String>> =
        filenames.map(|f| f.into_iter().take(4000).collect());

    let targets: Vec<InstalledContent> = content::list_installed(&dir, content_type)
        .into_iter()
        .filter(|c| c.provider == "modrinth" && c.project_id.is_some())
        .filter(|c| wanted.as_ref().map_or(true, |w| w.contains(&c.filename)))
        .collect();

    let mut out = Vec::with_capacity(targets.len());
    for (i, item) in targets.iter().enumerate() {
        let Some(pid) = item.project_id.as_deref() else {
            continue;
        };
        if i > 0 {
            // Stay inside Modrinth's unauthenticated rate budget; this is the
            // same cadence the old per-item frontend loop used.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        let status = classify_one(pid, mc_version.as_deref(), loader.as_deref()).await;
        out.push(CompatResult {
            filename: item.filename.clone(),
            status: status.to_string(),
        });
    }
    Ok(out)
}

/// Query Modrinth for one project and classify it. Any network/API failure
/// yields `unknown` — a blip must never claim incompatibility.
async fn classify_one(
    project_id: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
) -> &'static str {
    let strict = match modrinth::get_versions(project_id, mc_version, loader).await {
        Ok(v) => v.len(),
        Err(_) => return content::COMPAT_UNKNOWN,
    };
    if strict > 0 {
        return content::classify_compat(true, strict, None);
    }
    // Loader-only fallback: the loader constraint may hold even when no build
    // targets this exact Minecraft version — that state is *unverified*, not
    // compatible (the old frontend silently called it compatible).
    let loader_only = match loader {
        Some(l) if !l.is_empty() && !l.eq_ignore_ascii_case("vanilla") => {
            match modrinth::get_versions(project_id, None, loader).await {
                Ok(v) => Some(v.len()),
                Err(_) => return content::COMPAT_UNKNOWN,
            }
        }
        _ => None,
    };
    content::classify_compat(true, strict, loader_only)
}

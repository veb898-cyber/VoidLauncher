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
fn content_dir(
    state: &AppState,
    instance_name: &str,
    subfolder: &str,
) -> Result<(PathBuf, String), String> {
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
    Ok((
        instance.minecraft_dir(&config.instances_dir()).join(&sub),
        sub,
    ))
}

/// Modrinth ids are short base58 strings; reject anything else so a crafted
/// id can never be interpolated into an API URL.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ==================== Phase 1: the network worklist ====================

/// Fetches root versions, walks `required` dependency edges breadth-first and
/// collects everything [`content::build_resolution`] needs: nodes, project
/// titles and phase-1 unresolved reports. Fetch failures on dependencies are
/// NOT reported here — the resolver detects the missing node on its edge and
/// reports it with full source context (and richer phase-1 reasons win the
/// dedup because they are seeded first).
///
/// Both providers run through the same worklist: Modrinth versions use raw
/// version ids, CurseForge entries use the structured key `cf:{project_id}`
/// in the resolve queue and `cf:{project_id}:{file_id}` as a concrete version
/// id (a CF file, not a Modrinth version — never passed to the Modrinth API).
/// CurseForge dependency edges are always provider-pure, so a CF node's
/// dependencies resolve against CF and vice versa.
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
    /// resolved it otherwise). CurseForge entries are keyed `cf:{pid}`.
    need_resolve: HashSet<String>,
    unresolved: Vec<UnresolvedItem>,
    attempted: HashSet<String>,
    queued: HashSet<String>,
    /// CurseForge API key (empty when unconfigured — CF metadata calls then
    /// fail soft and CDN-only downloads still work later).
    cf_key: String,
    /// `file_id` → file metadata fetched while resolving CF entries.
    cf_files: HashMap<u64, crate::curseforge::CfFile>,
}

impl GraphFetcher {
    fn new(
        root_version_ids: &[String],
        mc_version: Option<String>,
        loader: Option<String>,
        installed_pids: HashSet<String>,
        cf_key: String,
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
            cf_key,
            cf_files: HashMap::new(),
        }
    }

    async fn ensure_project(&mut self, pid: &str, cf: bool) {
        if self.projects.contains_key(pid) {
            return;
        }
        let meta = if cf {
            match crate::curseforge::get_mod(pid.parse().unwrap_or(0), &self.cf_key).await {
                Ok(p) => {
                    let url = p
                        .logo
                        .as_ref()
                        .and_then(|l| l.best_url())
                        .map(|u| u.to_string());
                    (p.name, url)
                }
                Err(_) => (pid.to_string(), None),
            }
        } else {
            match modrinth::get_project(pid).await {
                Ok(p) => (p.title, p.icon_url),
                // A dead project lookup must not sink the whole resolution.
                Err(_) => (pid.to_string(), None),
            }
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

    /// Resolve one structured `cf:{project_id}:{file_id}` entry into a node.
    /// Concrete CF files are known by id — only metadata needs fetching.
    /// Root failures abort the resolution like Modrinth root failures do;
    /// dependency failures are left for the resolver to report on the edge.
    async fn process_cf_version(
        &mut self,
        vid: &str,
        queue: &mut VecDeque<String>,
    ) -> Result<(), String> {
        let _ = queue;
        let vid = vid.to_string();
        let raw = &vid[3..];
        let mut parts = raw.splitn(2, ':');
        let (Some(pid_str), Some(fid_str)) = (parts.next(), parts.next()) else {
            return Ok(());
        };
        let (Ok(pid), Ok(fid)) = (pid_str.parse::<u64>(), fid_str.parse::<u64>()) else {
            return Ok(());
        };
        let is_root = self.roots.contains(&vid);
        let file = match self.cf_files.get(&fid) {
            Some(f) => Some(f.clone()),
            None => match crate::curseforge::get_mod_file(pid, fid, &self.cf_key).await {
                Ok(f) => {
                    self.cf_files.insert(fid, f.clone());
                    Some(f)
                }
                Err(e) => {
                    if is_root {
                        return Err(format!("Failed to resolve CurseForge file {}: {}", vid, e));
                    }
                    return Ok(());
                }
            },
        };
        let Some(cf_file) = file else { return Ok(()) };

        self.ensure_project(pid_str, true).await;
        let (name, icon) = self
            .projects
            .get(pid_str)
            .cloned()
            .unwrap_or_else(|| (pid_str.to_string(), None));

        let plan_file = PlanFile {
            // Empty: the CF chain (signed URL → CDN mirrors) is resolved at
            // download time from (project_id, file_id).
            url: String::new(),
            filename: cf_file.file_name.clone(),
            sha1: cf_file.sha1_hash().map(|s| s.to_string()),
            file_id: Some(fid.to_string()),
        };

        let mut deps: Vec<DepEdge> = Vec::new();
        for d in &cf_file.dependencies {
            let Some(kind) = content::normalize_cf_dep_type(d.relation_type) else {
                continue;
            };
            let dep_pid = d.mod_id.to_string();
            if kind == DEP_REQUIRED && !self.installed_pids.contains(&dep_pid) {
                // Choice deferred to the resolve cycle (newest CF file for
                // this instance's MC/loader).
                self.need_resolve.insert(format!("cf:{}", dep_pid));
            } else if kind != DEP_REQUIRED {
                // Titles for optional entries and conflict messages.
                self.ensure_project(&dep_pid, true).await;
            }
            deps.push(DepEdge {
                source_version_id: vid.clone(),
                dep_type: kind.to_string(),
                project_id: Some(dep_pid),
                version_id: None,
            });
        }

        self.nodes.insert(
            vid.clone(),
            VersionNode {
                version_id: vid.clone(),
                project_id: pid_str.to_string(),
                name,
                version_label: cf_file.display_name.clone(),
                icon_url: icon,
                file: plan_file,
                deps,
                provider: "curseforge".to_string(),
            },
        );
        Ok(())
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
                if vid.starts_with("cf:") {
                    self.process_cf_version(&vid, &mut queue).await?;
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
                        file_id: None,
                    });
                let Some(file) = file else {
                    if self.roots.contains(&vid) {
                        return Err(format!("Version {} has no downloadable file", vid));
                    }
                    continue;
                };

                self.ensure_project(&version.project_id, false).await;
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
                                self.ensure_project(pid, false).await;
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
                                self.ensure_project(pid, false).await;
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
                        provider: "modrinth".to_string(),
                    },
                );
            }

            // Queue drained: resolve deferred project-only references whose
            // project already has no node (pins are all known now).
            if self.need_resolve.is_empty() {
                break;
            }
            let pending: Vec<String> = std::mem::take(&mut self.need_resolve).into_iter().collect();
            for key in pending {
                // `cf:{pid}` → CurseForge project, plain id → Modrinth.
                let (pid, cf) = match key.strip_prefix("cf:") {
                    Some(raw) => (raw.to_string(), true),
                    None => (key.clone(), false),
                };
                if self.installed_pids.contains(&pid)
                    || self.nodes.values().any(|n| n.project_id == pid)
                {
                    continue;
                }
                if cf {
                    let pid_num: u64 = match pid.parse() {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    match crate::curseforge::get_mod_files(
                        pid_num,
                        self.mc_version.as_deref(),
                        self.loader.as_deref(),
                        0,
                        &self.cf_key,
                    )
                    .await
                    {
                        // The list comes sorted newest-first with unavailable
                        // files filtered — take the newest release.
                        Ok(resp) if !resp.data.is_empty() => {
                            let file = resp.data[0].clone();
                            self.cf_files.insert(file.id, file);
                            let vid = format!("cf:{}:{}", pid, resp.data[0].id);
                            self.enqueue(&mut queue, &vid);
                        }
                        Ok(_) => {
                            self.ensure_project(&pid, true).await;
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
                            self.ensure_project(&pid, true).await;
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
                        self.ensure_project(&pid, false).await;
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
                        self.ensure_project(&pid, false).await;
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
        if let Some(raw) = id.strip_prefix("cf:") {
            // Structured CF root: cf:{project_id}:{file_id}, both numeric.
            let mut parts = raw.splitn(2, ':');
            let (Some(pid), Some(fid)) = (parts.next(), parts.next()) else {
                return Err("Invalid CurseForge version id".to_string());
            };
            if pid.is_empty()
                || fid.is_empty()
                || !pid.chars().all(|c| c.is_ascii_digit())
                || !fid.chars().all(|c| c.is_ascii_digit())
            {
                return Err("Invalid CurseForge version id".to_string());
            }
        } else if !valid_id(id) {
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
                name: c.project_name.clone().unwrap_or_else(|| c.filename.clone()),
                version_id: c.version_id.clone(),
            })
        })
        .collect();
    let installed_pids = installed_projects
        .iter()
        .map(|p| p.project_id.clone())
        .collect();
    let cf_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };

    let fetcher = GraphFetcher::new(
        &root_version_ids,
        mc_version,
        loader,
        installed_pids,
        cf_key,
    );
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
/// the old per-mod 200 ms frontend polling loop. Files with a project id on
/// a queryable provider (Modrinth, CurseForge) are classified against that
/// provider — everything else is reported as `unknown`. `filenames` restricts
/// the batch to files not classified yet (incremental refresh); `None`
/// classifies the whole content folder.
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
    let wanted: Option<HashSet<String>> = filenames.map(|f| f.into_iter().take(4000).collect());

    let targets: Vec<InstalledContent> = content::list_installed(&dir, content_type)
        .into_iter()
        .filter(|c| {
            matches!(c.provider.as_str(), "modrinth" | "curseforge") && c.project_id.is_some()
        })
        .filter(|c| wanted.as_ref().is_none_or(|w| w.contains(&c.filename)))
        .collect();
    let cf_key = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        config.curseforge_api_key.clone()
    };

    let mut out = Vec::with_capacity(targets.len());
    for (i, item) in targets.iter().enumerate() {
        let Some(pid) = item.project_id.as_deref() else {
            continue;
        };
        if i > 0 {
            // One shared budget: Modrinth's unauthenticated rate limit and
            // the (undocumented) CF limit both tolerate this cadence; it is
            // the same 200 ms the old per-item frontend loop used.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        let status = if item.provider == "curseforge" {
            classify_cf_one(pid, mc_version.as_deref(), loader.as_deref(), &cf_key).await
        } else {
            classify_one(pid, mc_version.as_deref(), loader.as_deref()).await
        };
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

/// CurseForge half of [`classify_one`]: same states, CF file listing as the
/// version source (newest-first, unavailable files already filtered). A
/// missing/failed listing is `unknown` — a blip must never claim
/// incompatibility.
async fn classify_cf_one(
    project_id: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
    api_key: &str,
) -> &'static str {
    let Ok(pid) = project_id.parse::<u64>() else {
        return content::COMPAT_UNKNOWN;
    };
    let strict = match crate::curseforge::get_mod_files(pid, mc_version, loader, 0, api_key).await {
        Ok(r) => r.data.len(),
        Err(_) => return content::COMPAT_UNKNOWN,
    };
    if strict > 0 {
        return content::classify_compat(true, strict, None);
    }
    let loader_only = match loader {
        Some(l) if !l.is_empty() && !l.eq_ignore_ascii_case("vanilla") => {
            match crate::curseforge::get_mod_files(pid, None, loader, 0, api_key).await {
                Ok(r) => Some(r.data.len()),
                Err(_) => return content::COMPAT_UNKNOWN,
            }
        }
        _ => None,
    };
    content::classify_compat(true, strict, loader_only)
}

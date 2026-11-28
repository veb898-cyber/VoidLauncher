//! Pure compatibility analysis over the existing installed-content and JAR
//! metadata models. It performs no filesystem or network access.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

pub use crate::commands::mods::{ModDependencyMetadata, ModEntryMetadata, ModMetadata};
pub use crate::content::InstalledContent;
pub use crate::instances::{Instance, LoaderType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CompatibilityStatus {
    Confirmed,
    Potential,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatibilityFinding {
    pub code: String,
    pub status: CompatibilityStatus,
    /// Descriptor name or `installed-content` / `instance-config`.
    pub source: String,
    /// The declaration or state that produced this result.
    pub basis: String,
    pub explanation: String,
    pub affected_files: Vec<String>,
    /// Internal mod IDs only; provider project IDs are deliberately excluded.
    pub affected_mod_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatibilityReport {
    pub complete: bool,
    pub files_analyzed: usize,
    pub findings: Vec<CompatibilityFinding>,
}

#[derive(Debug, Clone)]
struct ActiveMod {
    filename: String,
    version: Option<String>,
    source: String,
}

type ActiveMods = BTreeMap<String, Vec<ActiveMod>>;
type DisabledMods = BTreeMap<String, Vec<String>>;

#[derive(Clone, Copy)]
struct DependencyAnalysisContext<'a> {
    mc_version: &'a str,
    loader: &'a LoaderType,
    loader_version: Option<&'a str>,
    active: &'a ActiveMods,
    disabled: &'a DisabledMods,
    all_files_complete: bool,
}

/// Analyze a metadata snapshot produced by `get_mod_metadata`. The caller
/// pairs it with `InstalledContent`, whose filename-derived enabled bit is
/// authoritative. Missing metadata is represented as unknown, never clean.
pub fn analyze_installed_mods(
    instance: &Instance,
    installed: &[InstalledContent],
    metadata: &[ModMetadata],
) -> CompatibilityReport {
    let by_filename: BTreeMap<&str, &ModMetadata> = metadata
        .iter()
        .map(|item| (item.filename.as_str(), item))
        .collect();
    let active_files: Vec<_> = installed.iter().filter(|file| file.enabled).collect();
    let disabled_files: Vec<_> = installed.iter().filter(|file| !file.enabled).collect();
    let mut findings = Vec::new();
    let mut complete = true;

    let mut active: ActiveMods = BTreeMap::new();
    let mut disabled_ids: DisabledMods = BTreeMap::new();
    for file in installed {
        let Some(meta) = by_filename.get(file.filename.as_str()) else {
            complete = false;
            findings.push(finding(
                "metadata_unavailable",
                CompatibilityStatus::Unknown,
                "installed-content",
                "no cached JAR metadata was supplied",
                "The file could not be checked because parsed metadata is unavailable.",
                vec![file.filename.clone()],
                Vec::new(),
            ));
            continue;
        };
        if !meta.metadata_complete {
            complete = false;
            findings.push(finding(
                "metadata_incomplete",
                CompatibilityStatus::Unknown,
                "jar-descriptor",
                "descriptor was absent, malformed, unsupported, or only partially understood",
                "Some declared mod entries or relationships could not be verified.",
                vec![file.filename.clone()],
                meta.mod_id.clone().into_iter().collect(),
            ));
        }
        if meta.mod_entries.is_empty() {
            complete = false;
            findings.push(finding(
                "mod_identity_unknown", CompatibilityStatus::Unknown, "jar-descriptor",
                "no internal mod entry could be extracted",
                "The file has no usable internal mod ID, so dependencies and duplicate IDs cannot be checked for it.",
                vec![file.filename.clone()], Vec::new(),
            ));
        }
        for entry in &meta.mod_entries {
            if !entry.complete {
                complete = false;
            }
            if !entry_applies_to_loader(entry, &instance.loader, &instance.mc_version) {
                continue;
            }
            let identities: BTreeSet<_> = std::iter::once(entry.mod_id.as_str())
                .chain(entry.provides.iter().map(String::as_str))
                .collect();
            if file.enabled {
                for identity in identities {
                    let occurrences = active.entry(identity.to_string()).or_default();
                    occurrences.push(ActiveMod {
                        filename: file.filename.clone(),
                        version: entry.version.clone(),
                        source: entry.source.clone(),
                    });
                }
            } else {
                for identity in identities {
                    disabled_ids
                        .entry(identity.to_string())
                        .or_default()
                        .push(file.filename.clone());
                }
            }
        }
    }

    for file in &disabled_files {
        findings.push(finding(
            "mod_disabled", CompatibilityStatus::Potential, "installed-content",
            "the `.disabled` filename suffix excludes this file from the active mod set",
            "This mod is disabled and was not counted as a duplicate, dependency provider, or active conflict participant.",
            vec![file.filename.clone()], by_filename.get(file.filename.as_str())
                .map(|m| m.mod_entries.iter().map(|e| e.mod_id.clone()).collect()).unwrap_or_default(),
        ));
    }

    for (mod_id, occurrences) in &active {
        if occurrences.len() > 1 {
            // Fabric nested JARs participate as resolver candidates. When a
            // duplicate ID involves a nested candidate, metadata alone cannot
            // prove that both candidates will be loaded simultaneously.
            let has_nested_candidate = occurrences
                .iter()
                .any(|occurrence| occurrence.source.contains('!'));
            findings.push(finding(
                "duplicate_mod_id",
                if has_nested_candidate {
                    CompatibilityStatus::Potential
                } else {
                    CompatibilityStatus::Confirmed
                },
                "jar-descriptor",
                if has_nested_candidate {
                    "the same internal mod ID is declared by multiple enabled entries, including a nested-JAR candidate"
                } else {
                    "the same internal mod ID is declared by multiple enabled JAR entries"
                },
                &format!("Enabled files declare internal mod ID `{mod_id}` more than once."),
                occurrences
                    .iter()
                    .map(|occurrence| occurrence.filename.clone())
                    .collect(),
                vec![mod_id.clone()],
            ));
        }
    }

    let all_files_complete = active_files.iter().all(|file| {
        by_filename.get(file.filename.as_str()).is_some_and(|meta| {
            meta.metadata_complete
                && meta
                    .mod_entries
                    .iter()
                    .filter(|entry| {
                        entry_applies_to_loader(entry, &instance.loader, &instance.mc_version)
                    })
                    .all(|entry| entry.complete)
        })
    });
    let dependency_context = DependencyAnalysisContext {
        mc_version: &instance.mc_version,
        loader: &instance.loader,
        loader_version: instance.loader_version.as_deref(),
        active: &active,
        disabled: &disabled_ids,
        all_files_complete,
    };
    for file in &active_files {
        let Some(meta) = by_filename.get(file.filename.as_str()) else {
            continue;
        };
        for entry in meta
            .mod_entries
            .iter()
            .filter(|entry| entry_applies_to_loader(entry, &instance.loader, &instance.mc_version))
        {
            for dependency in &entry.dependencies {
                analyze_dependency(
                    &dependency_context,
                    file.filename.as_str(),
                    entry,
                    dependency,
                    &mut findings,
                );
            }
        }
    }

    check_loader_advertisement(instance, &active_files, &by_filename, &mut findings);
    if findings
        .iter()
        .any(|finding| finding.status == CompatibilityStatus::Unknown)
    {
        complete = false;
    }
    // Metadata incompleteness prevents a complete negative conclusion even if
    // no particular declared edge could be shown to fail.
    findings.sort_by(|a, b| {
        (&a.code, &a.affected_files, &a.affected_mod_ids).cmp(&(
            &b.code,
            &b.affected_files,
            &b.affected_mod_ids,
        ))
    });
    findings.dedup();
    CompatibilityReport {
        complete,
        files_analyzed: installed.len(),
        findings,
    }
}

fn check_loader_advertisement(
    instance: &Instance,
    active: &[&InstalledContent],
    metadata: &BTreeMap<&str, &ModMetadata>,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let loader = match instance.loader {
        LoaderType::Vanilla => None,
        LoaderType::Fabric => Some("Fabric"),
        LoaderType::Forge => Some("Forge"),
        LoaderType::NeoForge => Some("NeoForge"),
    };
    for file in active {
        let Some(meta) = metadata.get(file.filename.as_str()) else {
            continue;
        };
        if meta.loaders.is_empty()
            || loader.is_some_and(|loader| {
                meta.loaders
                    .iter()
                    .any(|item| item.eq_ignore_ascii_case(loader))
            })
        {
            continue;
        }
        let legacy_forge = loader == Some("NeoForge")
            && meta
                .loaders
                .iter()
                .any(|item| item.eq_ignore_ascii_case("Forge"));
        let version = mc_version_parts(&instance.mc_version);
        let old_neoforge_uses_forge = legacy_forge && version.is_some_and(|v| v < (1, 20, 5));
        if old_neoforge_uses_forge {
            continue;
        }
        let status = if legacy_forge && version.is_none() {
            CompatibilityStatus::Unknown
        } else {
            CompatibilityStatus::Potential
        };
        if legacy_forge && version.is_none() {
            findings.push(finding(
                "loader_compatibility_unknown", status, "jar-descriptor",
                "Forge metadata is accepted as NeoForge only for Minecraft versions through 1.20.4",
                "The instance Minecraft version could not be parsed to decide whether this Forge descriptor applies to NeoForge.",
                vec![file.filename.clone()], meta.mod_entries.iter().map(|e| e.mod_id.clone()).collect(),
            ));
        } else {
            findings.push(finding(
                "loader_mismatch",
                status,
                "jar-descriptor",
                "the JAR does not advertise support for this instance loader",
                &format!(
                    "This file declares {:?}, while the instance uses {}.",
                    meta.loaders,
                    loader.unwrap_or("Vanilla")
                ),
                vec![file.filename.clone()],
                meta.mod_entries.iter().map(|e| e.mod_id.clone()).collect(),
            ));
        }
    }
}

fn entry_applies_to_loader(
    entry: &ModEntryMetadata,
    loader: &LoaderType,
    mc_version: &str,
) -> bool {
    let descriptor = entry.source.rsplit('!').next().unwrap_or(&entry.source);
    match descriptor {
        "fabric.mod.json" => matches!(loader, LoaderType::Fabric),
        "META-INF/neoforge.mods.toml" => matches!(loader, LoaderType::NeoForge),
        "META-INF/mods.toml" | "mcmod.info" => match loader {
            LoaderType::Forge => true,
            LoaderType::NeoForge => {
                mc_version_parts(mc_version).is_some_and(|version| version < (1, 20, 5))
            }
            _ => false,
        },
        _ => true,
    }
}

fn analyze_dependency(
    context: &DependencyAnalysisContext<'_>,
    owner_file: &str,
    owner: &ModEntryMetadata,
    dependency: &ModDependencyMetadata,
    findings: &mut Vec<CompatibilityFinding>,
) {
    let DependencyAnalysisContext {
        mc_version,
        loader,
        loader_version,
        active,
        disabled,
        all_files_complete,
    } = *context;
    if dependency.side.as_deref() == Some("SERVER") {
        return;
    }
    if dependency
        .side
        .as_deref()
        .is_some_and(|side| side != "CLIENT" && side != "BOTH")
    {
        findings.push(finding(
            "dependency_side_unknown",
            CompatibilityStatus::Unknown,
            &owner_source(owner),
            "dependency has an unsupported side declaration",
            "The dependency was retained, but its side applicability could not be established.",
            vec![owner_file.to_string()],
            vec![owner.mod_id.clone(), dependency.mod_id.clone()],
        ));
        return;
    }
    if let Some(compatible) = loader_dependency_compatible(&dependency.mod_id, loader, mc_version) {
        if !compatible {
            match dependency.kind.as_str() {
                "required" => findings.push(finding(
                    "loader_dependency_mismatch",
                    if dependency.complete && owner.complete {
                        CompatibilityStatus::Confirmed
                    } else {
                        CompatibilityStatus::Potential
                    },
                    &owner_source(owner),
                    &format!("the mod requires `{}` but the instance uses a different loader", dependency.mod_id),
                    &format!("`{}` declares `{}` as a required loader dependency, but this instance uses `{loader:?}`.", owner.mod_id, dependency.mod_id),
                    vec![owner_file.to_string()],
                    vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                )),
                "optional" | "recommended" => findings.push(finding(
                    "loader_dependency_unavailable",
                    CompatibilityStatus::Potential,
                    &owner_source(owner),
                    &format!("optional loader dependency `{}` is not available", dependency.mod_id),
                    &format!("`{}` declares `{}` as an optional loader dependency.", owner.mod_id, dependency.mod_id),
                    vec![owner_file.to_string()],
                    vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                )),
                "suggested" | "incompatible" | "breaks" | "conflicts" | "discouraged" => {}
                _ => findings.push(finding(
                    "dependency_kind_unknown",
                    CompatibilityStatus::Unknown,
                    &owner_source(owner),
                    "the loader relationship kind is not understood",
                    "The loader declaration was retained but cannot be assigned safe compatibility semantics.",
                    vec![owner_file.to_string()],
                    vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                )),
            }
            return;
        }
    }
    let (environment_version, is_environment) = match dependency.mod_id.as_str() {
        "minecraft" => (Some(mc_version), true),
        "java" => (None, true),
        "fabricloader" | "forge" | "neoforge" => {
            if loader_dependency_compatible(&dependency.mod_id, loader, mc_version) == Some(true) {
                (loader_version, true)
            } else {
                (None, true)
            }
        }
        _ => (None, false),
    };
    let candidates = active.get(&dependency.mod_id);
    if is_environment && environment_version.is_none() {
        findings.push(finding(
            "environment_version_unknown",
            CompatibilityStatus::Unknown,
            &owner_source(owner),
            &format!("the instance version for `{}` is not available", dependency.mod_id),
            "The declared environment constraint was not evaluated because its active version is unavailable.",
            vec![owner_file.to_string()],
            vec![owner.mod_id.clone(), dependency.mod_id.clone()],
        ));
        return;
    }
    let provider = if is_environment {
        environment_version.map(|v| {
            vec![ActiveMod {
                filename: "instance".to_string(),
                version: Some(v.to_string()),
                source: "instance-config".to_string(),
            }]
        })
    } else {
        candidates.cloned()
    };

    match dependency.kind.as_str() {
        "incompatible" | "breaks" | "conflicts" | "discouraged" => {
            let Some(files) = provider else {
                return;
            };
            let mut matching_files = Vec::new();
            let mut unknown_files = Vec::new();
            let has_requirement = !dependency.alternatives.is_empty()
                || dependency
                    .requirement
                    .as_deref()
                    .is_some_and(|value| !value.is_empty());
            for candidate in &files {
                let matches = if has_requirement {
                    requirement_matches_dependency(dependency, candidate.version.as_deref())
                } else {
                    Some(true)
                };
                match matches {
                    Some(true) => matching_files.push(candidate.filename.clone()),
                    None => {
                        unknown_files.push((candidate.filename.clone(), candidate.source.clone()))
                    }
                    Some(false) => {}
                }
            }
            if !matching_files.is_empty() {
                let (code, status, explanation) = match dependency.kind.as_str() {
                    "conflicts" | "discouraged" => (
                        "declared_conflict",
                        CompatibilityStatus::Potential,
                        "The descriptor declares a warning-level conflict with an enabled mod.",
                    ),
                    _ => (
                        "declared_incompatibility",
                        if dependency.complete && owner.complete {
                            CompatibilityStatus::Confirmed
                        } else {
                            CompatibilityStatus::Potential
                        },
                        "The descriptor declares an incompatible enabled mod or environment.",
                    ),
                };
                let targets: Vec<_> = matching_files.iter().map(String::as_str).collect();
                findings.push(finding(
                    code,
                    status,
                    &owner_source(owner),
                    &format!("{} declares {} incompatible with {}", owner.mod_id, dependency.mod_id, dependency.requirement.as_deref().unwrap_or("the declared version range")),
                    &format!("`{}` declares `{}` incompatible in `{owner_file}`; matching active file(s): {}. {explanation}", owner.mod_id, dependency.mod_id, targets.join(", ")),
                    std::iter::once(owner_file.to_string()).chain(matching_files).collect(),
                    vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                ));
            } else if let Some((file, source)) = unknown_files.first() {
                findings.push(finding(
                    "incompatibility_unverified",
                    CompatibilityStatus::Unknown,
                    &owner_source(owner),
                    "the declared version range or installed version could not be evaluated",
                    &format!(
                        "The declaration could not be evaluated; metadata came from `{source}`."
                    ),
                    vec![owner_file.to_string(), file.clone()],
                    vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                ));
            }
        }
        "required" => {
            let Some(providers) = provider else {
                if let Some(disabled_files) = disabled.get(&dependency.mod_id) {
                    findings.push(finding(
                        "dependency_disabled",
                        CompatibilityStatus::Potential,
                        &owner_source(owner),
                        &format!(
                            "required mod `{}` exists only in disabled files",
                            dependency.mod_id
                        ),
                        &format!(
                            "`{}` requires `{}`, but its only detected file(s) are disabled.",
                            owner.mod_id, dependency.mod_id
                        ),
                        std::iter::once(owner_file.to_string())
                            .chain(disabled_files.iter().cloned())
                            .collect(),
                        vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                    ));
                } else {
                    let status = if all_files_complete && owner.complete {
                        CompatibilityStatus::Confirmed
                    } else {
                        CompatibilityStatus::Potential
                    };
                    findings.push(finding(
                        "missing_dependency", status, &owner_source(owner),
                        &format!("required dependency `{}` is not present among enabled mod entries", dependency.mod_id),
                        &format!("`{}` requires `{}`, but no enabled file declares that internal mod ID.", owner.mod_id, dependency.mod_id),
                        vec![owner_file.to_string()], vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                    ));
                }
                return;
            };
            let has_requirement = !dependency.alternatives.is_empty()
                || dependency
                    .requirement
                    .as_deref()
                    .is_some_and(|value| !value.is_empty());
            if has_requirement {
                if !is_environment && providers.iter().all(|item| item.version.is_none()) {
                    findings.push(finding(
                        "dependency_version_unknown",
                        CompatibilityStatus::Unknown,
                        &owner_source(owner),
                        &format!(
                            "required dependency `{}` has no parseable version",
                            dependency.mod_id
                        ),
                        "The dependency exists, but its installed version is unknown.",
                        std::iter::once(owner_file.to_string())
                            .chain(providers.iter().map(|candidate| candidate.filename.clone()))
                            .collect(),
                        vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                    ));
                } else {
                    let matched = providers.iter().any(|item| {
                        requirement_matches_dependency(dependency, item.version.as_deref())
                            == Some(true)
                    });
                    let unknown = providers.iter().any(|item| {
                        requirement_matches_dependency(dependency, item.version.as_deref())
                            .is_none()
                    });
                    if !matched && unknown {
                        findings.push(finding(
                            "dependency_version_unverified",
                            CompatibilityStatus::Unknown,
                            &owner_source(owner),
                            "the range or installed version is not supported by the conservative evaluator",
                            "The range was preserved from metadata but could not be evaluated safely.",
                            std::iter::once(owner_file.to_string()).chain(providers.iter().map(|candidate| candidate.filename.clone())).collect(),
                            vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                        ));
                    } else if !matched {
                        let status = if dependency.complete && owner.complete && all_files_complete
                        {
                            CompatibilityStatus::Confirmed
                        } else {
                            CompatibilityStatus::Potential
                        };
                        findings.push(finding(
                            "dependency_version_mismatch",
                            status,
                            &owner_source(owner),
                            "no enabled provider satisfies the declared version range",
                            &format!("`{}` requires `{}` to match the recorded range, but none of the enabled copies satisfy it.", owner.mod_id, dependency.mod_id),
                            std::iter::once(owner_file.to_string()).chain(providers.iter().map(|candidate| candidate.filename.clone())).collect(),
                            vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                        ));
                    }
                }
            }
        }
        "optional" | "recommended" => {
            let Some(providers) = provider else {
                if dependency.kind == "recommended" {
                    findings.push(finding(
                        "recommended_dependency_missing",
                        CompatibilityStatus::Potential,
                        &owner_source(owner),
                        &format!(
                            "recommended mod `{}` is not present among enabled entries",
                            dependency.mod_id
                        ),
                        &format!(
                            "`{}` recommends `{}`; its absence does not prevent loading.",
                            owner.mod_id, dependency.mod_id
                        ),
                        vec![owner_file.to_string()],
                        vec![owner.mod_id.clone(), dependency.mod_id.clone()],
                    ));
                }
                return;
            };
            if dependency.alternatives.is_empty()
                && dependency
                    .requirement
                    .as_deref()
                    .map(str::is_empty)
                    .unwrap_or(true)
            {
                return;
            }
            let matching = providers.iter().any(|provider| {
                requirement_matches_dependency(dependency, provider.version.as_deref())
                    == Some(true)
            });
            if matching {
                return;
            }
            let has_unknown_provider = providers.iter().any(|provider| {
                requirement_matches_dependency(dependency, provider.version.as_deref()).is_none()
            });
            let status = if has_unknown_provider {
                CompatibilityStatus::Unknown
            } else if matches!(loader, LoaderType::NeoForge)
                && dependency.kind == "optional"
                && dependency.complete
                && owner.complete
                && all_files_complete
            {
                // NeoForge permits an optional dependency to be absent, but
                // validates its version range when the dependency is present.
                CompatibilityStatus::Confirmed
            } else {
                CompatibilityStatus::Potential
            };
            findings.push(finding(
                if status == CompatibilityStatus::Unknown {
                    "dependency_version_unverified"
                } else {
                    "dependency_version_mismatch"
                },
                status,
                &owner_source(owner),
                "optional dependency version does not satisfy the declaration",
                "The optional mod is present, but its version does not satisfy the declared range.",
                std::iter::once(owner_file.to_string())
                    .chain(providers.iter().map(|candidate| candidate.filename.clone()))
                    .collect(),
                vec![owner.mod_id.clone(), dependency.mod_id.clone()],
            ));
        }
        "suggested" => {}
        _ => findings.push(finding(
            "dependency_kind_unknown",
            CompatibilityStatus::Unknown,
            &owner_source(owner),
            &format!("unrecognized relationship kind `{}`", dependency.kind),
            "The relationship was retained but not assigned required or incompatible semantics.",
            vec![owner_file.to_string()],
            vec![owner.mod_id.clone(), dependency.mod_id.clone()],
        )),
    }
}

fn owner_source(entry: &ModEntryMetadata) -> String {
    entry.source.clone()
}

fn loader_dependency_compatible(
    dependency_id: &str,
    loader: &LoaderType,
    mc_version: &str,
) -> Option<bool> {
    match dependency_id {
        "fabricloader" => Some(matches!(loader, LoaderType::Fabric)),
        "forge" => match loader {
            LoaderType::Forge => Some(true),
            LoaderType::NeoForge => {
                mc_version_parts(mc_version).map(|version| version < (1, 20, 5))
            }
            _ => Some(false),
        },
        "neoforge" => Some(matches!(loader, LoaderType::NeoForge)),
        _ => None,
    }
}

fn requirement_matches_dependency(
    dependency: &ModDependencyMetadata,
    installed: Option<&str>,
) -> Option<bool> {
    if !dependency.alternatives.is_empty() {
        let mut unknown = false;
        for alternative in &dependency.alternatives {
            match requirement_matches(alternative, installed) {
                Some(true) => return Some(true),
                Some(false) => {}
                None => unknown = true,
            }
        }
        return if unknown { None } else { Some(false) };
    }
    dependency
        .requirement
        .as_deref()
        .filter(|requirement| !requirement.is_empty())
        .map(|requirement| requirement_matches(requirement, installed))
        .unwrap_or(Some(true))
}

fn finding(
    code: &str,
    status: CompatibilityStatus,
    source: &str,
    basis: &str,
    explanation: &str,
    affected_files: Vec<String>,
    affected_mod_ids: Vec<String>,
) -> CompatibilityFinding {
    let files: BTreeSet<_> = affected_files.into_iter().collect();
    let ids: BTreeSet<_> = affected_mod_ids.into_iter().collect();
    CompatibilityFinding {
        code: code.to_string(),
        status,
        source: source.to_string(),
        basis: basis.to_string(),
        explanation: explanation.to_string(),
        affected_files: files.into_iter().collect(),
        affected_mod_ids: ids.into_iter().collect(),
    }
}

/// Evaluate only common, well-defined forms. `None` means unknown syntax,
/// never a mismatch. Supports exact versions, wildcard, comparator chains,
/// and a single Maven interval such as `[1.20,1.21)`.
fn requirement_matches(requirement: &str, installed: Option<&str>) -> Option<bool> {
    let installed = installed?.trim().trim_start_matches('v');
    let req = requirement.trim();
    if req == "*" {
        return Some(true);
    }
    if req.is_empty() {
        return None;
    }
    if (req.starts_with('[') || req.starts_with('(')) && (req.ends_with(']') || req.ends_with(')'))
    {
        let inner = &req[1..req.len() - 1];
        let (low, high) = inner.split_once(',')?;
        let lower_ok = if low.trim().is_empty() {
            true
        } else {
            let cmp = compare_versions(installed, low.trim())?;
            if req.starts_with('[') {
                cmp != std::cmp::Ordering::Less
            } else {
                cmp == std::cmp::Ordering::Greater
            }
        };
        let upper_ok = if high.trim().is_empty() {
            true
        } else {
            let cmp = compare_versions(installed, high.trim())?;
            if req.ends_with(']') {
                cmp != std::cmp::Ordering::Greater
            } else {
                cmp == std::cmp::Ordering::Less
            }
        };
        return Some(lower_ok && upper_ok);
    }
    if req.contains("||") {
        let mut unknown = false;
        for alternative in req.split("||") {
            match requirement_matches(alternative.trim(), Some(installed)) {
                Some(true) => return Some(true),
                Some(false) => {}
                None => unknown = true,
            }
        }
        return if unknown { None } else { Some(false) };
    }
    let constraints: Vec<_> = req.split_whitespace().collect();
    if constraints.len() > 1 {
        let mut result = true;
        for part in constraints {
            result &= evaluate_comparator(installed, part)?;
        }
        return Some(result);
    }
    if let Some(prefix) = req.strip_suffix(".x").or_else(|| req.strip_suffix(".*")) {
        return Some(
            installed == prefix
                || installed
                    .strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with('.')),
        );
    }
    if let Some((op, version)) = [">=", "<=", ">", "<", "=", "^", "~"]
        .iter()
        .find_map(|op| req.strip_prefix(op).map(|value| (*op, value.trim())))
    {
        if op == "^" || op == "~" {
            return None;
        }
        return evaluate_comparator(installed, &format!("{op}{version}"));
    }
    if installed == req {
        return Some(true);
    }
    Some(compare_versions(installed, req)? == std::cmp::Ordering::Equal)
}

fn evaluate_comparator(installed: &str, comparator: &str) -> Option<bool> {
    let (op, raw) = [">=", "<=", ">", "<", "="]
        .iter()
        .find_map(|op| comparator.strip_prefix(op).map(|v| (*op, v.trim())))
        .unwrap_or(("=", comparator.trim()));
    let cmp = compare_versions(installed, raw)?;
    Some(match op {
        ">=" => cmp != std::cmp::Ordering::Less,
        "<=" => cmp != std::cmp::Ordering::Greater,
        ">" => cmp == std::cmp::Ordering::Greater,
        "<" => cmp == std::cmp::Ordering::Less,
        "=" => cmp == std::cmp::Ordering::Equal,
        _ => return None,
    })
}

fn compare_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    fn parse(value: &str) -> Option<(Vec<u64>, Option<Vec<String>>)> {
        let value = value.trim().trim_start_matches('v').split('+').next()?;
        let (core, prerelease) = value
            .split_once('-')
            .map_or((value, None), |(a, b)| (a, Some(b)));
        let core: Vec<u64> = core
            .split('.')
            .map(|part| part.parse().ok())
            .collect::<Option<_>>()?;
        if core.is_empty() {
            return None;
        }
        let prerelease = match prerelease {
            Some(value) => {
                if value.is_empty()
                    || !value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
                {
                    return None;
                }
                let identifiers: Vec<String> = value
                    .split(['.', '-'])
                    .map(str::to_ascii_lowercase)
                    .collect();
                if !identifiers.first().is_some_and(|first| {
                    ["alpha", "beta", "rc", "pre", "snapshot"]
                        .iter()
                        .any(|prefix| first.starts_with(prefix))
                }) {
                    return None;
                }
                Some(identifiers)
            }
            None => None,
        };
        Some((core, prerelease))
    }
    fn compare_pre(a: &[String], b: &[String]) -> std::cmp::Ordering {
        for i in 0..a.len().max(b.len()) {
            match (a.get(i), b.get(i)) {
                (None, None) => return std::cmp::Ordering::Equal,
                (None, Some(_)) => return std::cmp::Ordering::Less,
                (Some(_), None) => return std::cmp::Ordering::Greater,
                (Some(x), Some(y)) => {
                    let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                        (Err(_), Err(_)) => x.cmp(y),
                    };
                    if order != std::cmp::Ordering::Equal {
                        return order;
                    }
                }
            }
        }
        std::cmp::Ordering::Equal
    }
    let (a_core, a_pre) = parse(left)?;
    let (b_core, b_pre) = parse(right)?;
    for i in 0..a_core.len().max(b_core.len()) {
        let order = a_core
            .get(i)
            .copied()
            .unwrap_or(0)
            .cmp(&b_core.get(i).copied().unwrap_or(0));
        if order != std::cmp::Ordering::Equal {
            return Some(order);
        }
    }
    Some(match (a_pre.as_deref(), b_pre.as_deref()) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(a), Some(b)) => compare_pre(a, b),
    })
}

fn mc_version_parts(value: &str) -> Option<(u32, u32, u32)> {
    let mut parts = value.split('.').map(|part| part.parse::<u32>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(filename: &str, enabled: bool) -> InstalledContent {
        InstalledContent {
            filename: filename.into(),
            content_type: "mod".into(),
            provider: "modrinth".into(),
            project_id: Some("provider-project".into()),
            version_id: None,
            file_id: None,
            project_name: None,
            version_number: None,
            hash: None,
            enabled,
            installed_at: None,
        }
    }

    fn metadata(filename: &str, entries: Vec<ModEntryMetadata>, complete: bool) -> ModMetadata {
        ModMetadata {
            filename: filename.into(),
            name: filename.into(),
            version: String::new(),
            mod_id: None,
            authors: Vec::new(),
            homepage: None,
            provider: "Modrinth".into(),
            loaders: vec!["Fabric".into()],
            version_known: false,
            enabled: true,
            file_size: 0,
            icon: None,
            slug: Some("provider-project".into()),
            project_id: Some("provider-project".into()),
            slug_verified: true,
            mod_entries: entries,
            metadata_complete: complete,
        }
    }

    fn entry(
        id: &str,
        version: &str,
        dependencies: Vec<ModDependencyMetadata>,
    ) -> ModEntryMetadata {
        ModEntryMetadata {
            mod_id: id.into(),
            version: Some(version.into()),
            source: "fabric.mod.json".into(),
            provides: Vec::new(),
            dependencies,
            complete: true,
        }
    }

    fn dep(id: &str, requirement: Option<&str>, kind: &str) -> ModDependencyMetadata {
        ModDependencyMetadata {
            mod_id: id.into(),
            requirement: requirement.map(str::to_string),
            alternatives: Vec::new(),
            side: None,
            complete: true,
            kind: kind.into(),
        }
    }

    fn instance() -> Instance {
        Instance::new("test", "1.20.1", LoaderType::Fabric, Some("0.15.0".into()))
    }

    #[test]
    fn duplicate_internal_ids_are_confirmed_and_provider_ids_are_ignored() {
        let installed = vec![content("a.jar", true), content("b.jar", true)];
        let metadata = vec![
            metadata("a.jar", vec![entry("shared", "1.0", vec![])], true),
            metadata("b.jar", vec![entry("shared", "1.0", vec![])], true),
        ];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert_eq!(
            report
                .findings
                .iter()
                .filter(
                    |f| f.code == "duplicate_mod_id" && f.status == CompatibilityStatus::Confirmed
                )
                .count(),
            1
        );
        assert_eq!(
            report
                .findings
                .iter()
                .find(|f| f.code == "duplicate_mod_id")
                .unwrap()
                .affected_mod_ids,
            vec!["shared"]
        );
    }

    #[test]
    fn fabric_provided_ids_satisfy_required_dependencies() {
        let installed = vec![content("owner.jar", true), content("provider.jar", true)];
        let mut provider = entry("new_library_id", "1.0", vec![]);
        provider.provides.push("old_library_id".into());
        let metadata = vec![
            metadata(
                "owner.jar",
                vec![entry(
                    "owner",
                    "1.0",
                    vec![dep("old_library_id", Some("*"), "required")],
                )],
                true,
            ),
            metadata("provider.jar", vec![provider], true),
        ];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(!report
            .findings
            .iter()
            .any(|finding| finding.code == "missing_dependency"));
    }

    #[test]
    fn missing_required_dependency_is_confirmed_when_scan_is_complete() {
        let installed = vec![content("a.jar", true)];
        let metadata = vec![metadata(
            "a.jar",
            vec![entry(
                "a",
                "1.0",
                vec![dep("missing", Some("*"), "required")],
            )],
            true,
        )];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert_eq!(
            report
                .findings
                .iter()
                .find(|f| f.code == "missing_dependency")
                .unwrap()
                .status,
            CompatibilityStatus::Confirmed
        );
    }

    #[test]
    fn version_range_mismatch_and_incompatible_declaration_are_confirmed() {
        let installed = vec![content("a.jar", true), content("b.jar", true)];
        let metadata = vec![
            metadata(
                "a.jar",
                vec![entry(
                    "a",
                    "1.0",
                    vec![
                        dep("b", Some("[2,3)"), "required"),
                        dep("blocked", Some("*"), "incompatible"),
                    ],
                )],
                true,
            ),
            metadata("b.jar", vec![entry("b", "1.0", vec![])], true),
            metadata("c.jar", vec![entry("blocked", "1.0", vec![])], true),
        ];
        let mut installed = installed;
        installed.push(content("c.jar", true));
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "dependency_version_mismatch"
                && f.status == CompatibilityStatus::Confirmed));
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "declared_incompatibility"
                && f.status == CompatibilityStatus::Confirmed));
    }

    #[test]
    fn disabled_mod_does_not_satisfy_or_participate_in_active_conflicts() {
        let installed = vec![content("a.jar", true), content("b.jar.disabled", false)];
        let metadata = vec![
            metadata(
                "a.jar",
                vec![entry("a", "1.0", vec![dep("b", None, "required")])],
                true,
            ),
            metadata("b.jar.disabled", vec![entry("b", "1.0", vec![])], true),
        ];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "dependency_disabled"
                    && f.status == CompatibilityStatus::Potential)
        );
        assert!(report.findings.iter().any(|f| f.code == "mod_disabled"));
        assert!(!report.findings.iter().any(|f| f.code == "duplicate_mod_id"));
    }

    #[test]
    fn unknown_metadata_never_produces_a_confirmed_missing_dependency() {
        let installed = vec![content("a.jar", true), content("unknown.jar", true)];
        let metadata = vec![
            metadata(
                "a.jar",
                vec![entry("a", "1.0", vec![dep("b", None, "required")])],
                true,
            ),
            metadata("unknown.jar", vec![], false),
        ];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert_eq!(
            report
                .findings
                .iter()
                .find(|f| f.code == "missing_dependency")
                .unwrap()
                .status,
            CompatibilityStatus::Potential
        );
        assert!(!report.complete);
    }

    #[test]
    fn unsupported_range_syntax_is_unknown() {
        assert_eq!(requirement_matches("^1.2.0", Some("1.5.0")), None);
        assert_eq!(requirement_matches("<2 || >=2.5", Some("3.0")), Some(true));
        assert_eq!(requirement_matches("<2 || >=2.5", Some("2.2")), Some(false));
        assert_eq!(
            requirement_matches("[1.20,1.21)", Some("1.20.1")),
            Some(true)
        );
        assert_eq!(requirement_matches(">=1.21", Some("1.20.1")), Some(false));
    }

    #[test]
    fn a_matching_copy_satisfies_a_required_dependency_even_if_a_duplicate_is_old() {
        let installed = vec![
            content("owner.jar", true),
            content("lib-old.jar", true),
            content("lib-new.jar", true),
        ];
        let metadata = vec![
            metadata(
                "owner.jar",
                vec![entry(
                    "owner",
                    "1.0",
                    vec![dep("library", Some(">=2.0"), "required")],
                )],
                true,
            ),
            metadata("lib-old.jar", vec![entry("library", "1.0", vec![])], true),
            metadata("lib-new.jar", vec![entry("library", "2.1", vec![])], true),
        ];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(!report.findings.iter().any(|finding| {
            finding.code == "dependency_version_mismatch"
                && finding.affected_mod_ids == vec!["owner", "library"]
        }));
    }

    #[test]
    fn dependencies_from_another_loader_descriptor_are_not_analyzed_for_this_instance() {
        let installed = vec![content("multi-loader.jar", true)];
        let mut forge_entry = entry(
            "forge_mod",
            "1.0",
            vec![dep("forge_only_library", Some("*"), "required")],
        );
        forge_entry.source = "META-INF/mods.toml".into();
        let metadata = vec![metadata(
            "multi-loader.jar",
            vec![
                entry(
                    "fabric_mod",
                    "1.0",
                    vec![dep("fabric_only_library", Some("*"), "required")],
                ),
                forge_entry,
            ],
            true,
        )];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        let missing: Vec<_> = report
            .findings
            .iter()
            .filter(|finding| finding.code == "missing_dependency")
            .map(|finding| finding.affected_mod_ids[1].as_str())
            .collect();
        assert_eq!(missing, vec!["fabric_only_library"]);
    }

    #[test]
    fn duplicate_entries_within_one_descriptor_are_detected() {
        let installed = vec![content("multi.jar", true)];
        let metadata = vec![metadata(
            "multi.jar",
            vec![entry("same", "1.0", vec![]), entry("same", "1.0", vec![])],
            true,
        )];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(report.findings.iter().any(|finding| {
            finding.code == "duplicate_mod_id" && finding.status == CompatibilityStatus::Confirmed
        }));
    }

    #[test]
    fn fabric_nested_duplicate_id_is_potential_and_nested_edges_are_analyzed() {
        let installed = vec![content("parent.jar", true)];
        let mut parent = entry("shared", "2.0", vec![dep("nested", Some("*"), "required")]);
        let mut nested = entry("shared", "1.0", vec![dep("shared", Some("*"), "breaks")]);
        nested.source = "META-INF/jars/shared.jar!fabric.mod.json".into();
        let mut nested_provider = entry("nested", "1.0", vec![]);
        nested_provider.source = "META-INF/jars/nested.jar!fabric.mod.json".into();
        parent.source = "fabric.mod.json".into();

        let report = analyze_installed_mods(
            &instance(),
            &installed,
            &[metadata(
                "parent.jar",
                vec![parent, nested, nested_provider],
                true,
            )],
        );

        let duplicate = report
            .findings
            .iter()
            .find(|finding| finding.code == "duplicate_mod_id")
            .expect("nested duplicate ID should be reported");
        assert_eq!(duplicate.status, CompatibilityStatus::Potential);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "declared_incompatibility"));
        assert!(!report
            .findings
            .iter()
            .any(|finding| finding.code == "missing_dependency"));
    }

    #[test]
    fn fabric_conflicts_are_warnings_while_breaks_are_confirmed() {
        let installed = vec![content("owner.jar", true), content("target.jar", true)];
        let metadata = vec![
            metadata(
                "owner.jar",
                vec![entry(
                    "owner",
                    "1.0",
                    vec![
                        dep("target", Some("*"), "conflicts"),
                        dep("target", Some("*"), "breaks"),
                    ],
                )],
                true,
            ),
            metadata("target.jar", vec![entry("target", "1.0", vec![])], true),
        ];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(report.findings.iter().any(|finding| {
            finding.code == "declared_conflict" && finding.status == CompatibilityStatus::Potential
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.code == "declared_incompatibility"
                && finding.status == CompatibilityStatus::Confirmed
        }));
    }

    #[test]
    fn optional_dependency_absence_is_ignored_but_version_mismatch_is_potential() {
        let installed = vec![content("owner.jar", true)];
        let mut metas = vec![metadata(
            "owner.jar",
            vec![entry(
                "owner",
                "1.0",
                vec![dep("optional-lib", Some("[2,3)"), "optional")],
            )],
            true,
        )];
        let absent = analyze_installed_mods(&instance(), &installed, &metas);
        assert!(!absent
            .findings
            .iter()
            .any(|finding| finding.code == "missing_dependency"));

        let mut installed = installed;
        installed.push(content("optional.jar", true));
        metas.push(metadata(
            "optional.jar",
            vec![entry("optional-lib", "1.0", vec![])],
            true,
        ));
        let report = analyze_installed_mods(&instance(), &installed, &metas);
        assert!(report.findings.iter().any(|finding| {
            finding.code == "dependency_version_mismatch"
                && finding.status == CompatibilityStatus::Potential
        }));
    }

    #[test]
    fn neoforge_optional_dependency_version_mismatch_is_confirmed_when_evaluable() {
        let mut target = Instance::new(
            "test",
            "1.21.1",
            LoaderType::NeoForge,
            Some("21.1.1".into()),
        );
        let installed = vec![content("owner.jar", true), content("library.jar", true)];
        let mut owner = entry(
            "owner",
            "1.0",
            vec![dep("library", Some("[2,3)"), "optional")],
        );
        owner.source = "META-INF/neoforge.mods.toml".into();
        let mut library = entry("library", "1.0", vec![]);
        library.source = "META-INF/neoforge.mods.toml".into();
        let mut owner_meta = metadata("owner.jar", vec![owner], true);
        owner_meta.loaders = vec!["NeoForge".into()];
        let mut library_meta = metadata("library.jar", vec![library], true);
        library_meta.loaders = vec!["NeoForge".into()];

        let metadata_items = vec![owner_meta, library_meta];
        let report = analyze_installed_mods(&target, &installed, &metadata_items);
        assert!(report.findings.iter().any(|finding| {
            finding.code == "dependency_version_mismatch"
                && finding.status == CompatibilityStatus::Confirmed
                && finding.affected_mod_ids == vec!["library", "owner"]
        }));

        let mut partially_scanned = installed.clone();
        partially_scanned.push(content("unknown.jar", true));
        let partial_report = analyze_installed_mods(&target, &partially_scanned, &metadata_items);
        assert!(partial_report.findings.iter().any(|finding| {
            finding.code == "dependency_version_mismatch"
                && finding.status == CompatibilityStatus::Potential
        }));

        target.loader = LoaderType::Forge;
        let mut forge_owner = entry(
            "owner",
            "1.0",
            vec![dep("library", Some("[2,3)"), "optional")],
        );
        forge_owner.source = "META-INF/mods.toml".into();
        let mut forge_library = entry("library", "1.0", vec![]);
        forge_library.source = "META-INF/mods.toml".into();
        let mut forge_owner_meta = metadata("owner.jar", vec![forge_owner], true);
        forge_owner_meta.loaders = vec!["Forge".into()];
        let mut forge_library_meta = metadata("library.jar", vec![forge_library], true);
        forge_library_meta.loaders = vec!["Forge".into()];
        let forge_report =
            analyze_installed_mods(&target, &installed, &[forge_owner_meta, forge_library_meta]);
        assert!(forge_report.findings.iter().any(|finding| {
            finding.code == "dependency_version_mismatch"
                && finding.status == CompatibilityStatus::Potential
        }));
    }

    #[test]
    fn server_only_forge_dependency_is_not_missing_in_a_client_instance() {
        let installed = vec![content("owner.jar", true)];
        let mut dependency = dep("dedicated_server_api", Some("*"), "required");
        dependency.side = Some("SERVER".into());
        let metadata = vec![metadata(
            "owner.jar",
            vec![entry("owner", "1.0", vec![dependency])],
            true,
        )];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(!report
            .findings
            .iter()
            .any(|finding| finding.code == "missing_dependency"));
        assert!(report.complete);
    }

    #[test]
    fn loader_ids_are_environment_requirements_not_missing_mod_files() {
        let installed = vec![content("owner.jar", true)];
        let metadata = vec![metadata(
            "owner.jar",
            vec![entry(
                "owner",
                "1.0",
                vec![dep("forge", Some("[47,)"), "required")],
            )],
            true,
        )];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(report.findings.iter().any(|finding| {
            finding.code == "loader_dependency_mismatch"
                && finding.status == CompatibilityStatus::Confirmed
        }));
        assert!(!report
            .findings
            .iter()
            .any(|finding| finding.code == "missing_dependency"));
    }

    #[test]
    fn minecraft_and_loader_constraints_use_instance_versions() {
        let installed = vec![content("a.jar", true)];
        let metadata = vec![metadata(
            "a.jar",
            vec![entry(
                "a",
                "1.0",
                vec![
                    dep("minecraft", Some("[1.21,1.22)"), "required"),
                    dep("fabricloader", Some(">=0.16"), "required"),
                ],
            )],
            true,
        )];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|f| f.code == "dependency_version_mismatch"
                    && f.status == CompatibilityStatus::Confirmed)
                .count(),
            2
        );
        assert!(report
            .findings
            .iter()
            .all(|f| f.source == "fabric.mod.json"));
    }

    #[test]
    fn same_internal_id_from_two_descriptors_in_one_jar_is_not_duplicate_file_conflict() {
        let installed = vec![content("multi.jar", true)];
        let metadata = vec![metadata(
            "multi.jar",
            vec![
                entry("same", "1.0", vec![]),
                ModEntryMetadata {
                    source: "META-INF/mods.toml".into(),
                    provides: Vec::new(),
                    ..entry("same", "1.0", vec![])
                },
            ],
            true,
        )];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(!report.findings.iter().any(|f| f.code == "duplicate_mod_id"));
    }

    #[test]
    fn prerelease_order_is_semver_like_and_unknown_versions_stay_unknown() {
        assert_eq!(
            compare_versions("1.0.0-alpha.2", "1.0.0-alpha.10"),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.0.0-rc.1", "1.0.0"),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            requirement_matches(">=snapshot/latest", Some("1.20.1")),
            None
        );
    }

    #[test]
    fn missing_loader_version_is_unknown_not_a_missing_mod_conflict() {
        let mut target = instance();
        target.loader_version = None;
        let installed = vec![content("a.jar", true)];
        let metadata = vec![metadata(
            "a.jar",
            vec![entry(
                "a",
                "1.0",
                vec![dep("fabricloader", Some(">=0.15"), "required")],
            )],
            true,
        )];
        let report = analyze_installed_mods(&target, &installed, &metadata);
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "environment_version_unknown"
                && f.status == CompatibilityStatus::Unknown));
        assert!(!report
            .findings
            .iter()
            .any(|f| f.code == "missing_dependency"));
    }

    #[test]
    fn legacy_forge_descriptor_is_accepted_for_neoforge_through_minecraft_1204() {
        let mut target = Instance::new(
            "test",
            "1.20.4",
            LoaderType::NeoForge,
            Some("20.4.237".into()),
        );
        let installed = vec![content("legacy.jar", true)];
        let mut meta = metadata(
            "legacy.jar",
            vec![entry(
                "legacy",
                "1.0",
                vec![dep("forge", Some("[20.4,)"), "required")],
            )],
            true,
        );
        meta.loaders = vec!["Forge".into()];
        meta.mod_entries[0].source = "META-INF/mods.toml".into();
        let report = analyze_installed_mods(&target, &installed, &[meta.clone()]);
        assert!(!report
            .findings
            .iter()
            .any(|f| f.code == "loader_mismatch" || f.code == "missing_dependency"));
        target.mc_version = "1.20.5".into();
        let report = analyze_installed_mods(&target, &installed, &[meta]);
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "loader_mismatch" && f.status == CompatibilityStatus::Potential));
    }

    #[test]
    fn java_requirement_is_unknown_without_runtime_java_version() {
        let installed = vec![content("a.jar", true)];
        let metadata = vec![metadata(
            "a.jar",
            vec![entry(
                "a",
                "1.0",
                vec![dep("java", Some(">=21"), "required")],
            )],
            true,
        )];
        let report = analyze_installed_mods(&instance(), &installed, &metadata);
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "environment_version_unknown"
                && f.status == CompatibilityStatus::Unknown));
        assert!(!report
            .findings
            .iter()
            .any(|f| f.code == "missing_dependency"));
    }
}
